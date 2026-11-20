//! B 端核心状态与编码线程。和网络层（net.rs）、JNI（lib.rs）解耦：编码线程只往“当前连接”的 tx 通道放字节。
//! 编码线程：来帧才干活（没有新帧就是零流量，不用定时器驱动）；在途未确认字节超过上限时暂停，恢复后取最新一帧编一次（10 §6）。

use rd::enc::Encoder;
use rd::params::Params;
use rd::wire::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// 当前连接（同一时刻只服务一个 A，50 §3）
pub struct Conn {
    pub id: u64,
    pub ctl: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    pub tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    pub conn: Option<wtransport::Connection>,   // 虚拟 A（va.rs）没有真连接
}

#[derive(Default)]
pub struct StatsAcc {
    pub layers: [u64; 3],          // bg lit fg
    pub by_kind: Vec<(String, u64)>,
    pub tx_msgs: u64,
    pub frames: u64,               // 编出非空 Tx 的次数
    pub rows_sent: u64,
    pub enc_ms_total: f64,
    pub enc_ms_last: f64,
    pub tx_bytes: u64,
}

#[derive(Default)]
struct Work { dirty: bool, kick: bool, soft: bool, last: Option<Instant>, first: Option<Instant> }

pub struct Frame { pub buf: Vec<u8>, pub w: usize, pub h: usize }

pub struct Core {
    pub up_tx: Sender<String>,
    pub up_rx: Mutex<Receiver<String>>,
    pub params: Mutex<Params>,
    pub apps: Mutex<Vec<AppInfo>>,
    pub token: String,
    pub conn: Mutex<Option<Conn>>,
    pub conn_seq: AtomicU64,
    pub enc: Mutex<Option<Encoder>>,
    pub frame: Mutex<Frame>,
    work: Mutex<Work>,
    cv: Condvar,
    pub tx_seq: AtomicU64,
    pub sent: AtomicU64,
    pub acked: AtomicU64,
    pending: Mutex<VecDeque<(u64, u64)>>,   // (tx seq, 累计发出字节)
    pub stats: Mutex<StatsAcc>,
    pub rev: AtomicU64,
    pub t0: Instant,
    pub last_push: Mutex<Instant>,
    pub deferring: std::sync::atomic::AtomicBool,
    pub va: Mutex<Option<crate::va::VaState>>,
    /// A 报的视口（画布 y）：采集头要追上它
    pub view: Mutex<Option<i32>>,
    pub req: Mutex<Option<ScrollReq>>,
    /// 滚轮因子（像素 / 格），按实测位移自学（因容器而异：RecyclerView 168，设置首页的 ScrollView 约 357）
    pub wheel_f: Mutex<f64>,
    /// 调试统计：请求数 / 超时数 / 最近一次的 (step, notches)
    pub dbg_req: Mutex<(u32, u32, i32, f64)>,
    last_resets: std::sync::atomic::AtomicU32,
    f_learned: std::sync::atomic::AtomicBool,
    timeouts: std::sync::atomic::AtomicU32,
}

pub const PAGE: PageId = 1;

/// 采集头调度发出的一次滚动请求（等帧里出现位移才算响应）
pub struct ScrollReq { pub t: Instant, pub notches: f64, pub step: i32 }

impl Core {
    pub fn new(token: String) -> Arc<Core> {
        let (up_tx, up_rx) = std::sync::mpsc::channel();
        Arc::new(Core {
            up_tx, up_rx: Mutex::new(up_rx), params: Mutex::new(Params::new()), apps: Mutex::new(vec![]), token,
            conn: Mutex::new(None), conn_seq: AtomicU64::new(0), enc: Mutex::new(None), frame: Mutex::new(Frame { buf: vec![], w: 0, h: 0 }),
            work: Mutex::new(Work::default()), cv: Condvar::new(), tx_seq: AtomicU64::new(0), sent: AtomicU64::new(0), acked: AtomicU64::new(0),
            pending: Mutex::new(VecDeque::new()), stats: Mutex::new(StatsAcc::default()), rev: AtomicU64::new(0), t0: Instant::now(), last_push: Mutex::new(Instant::now()), deferring: std::sync::atomic::AtomicBool::new(false), va: Mutex::new(None), view: Mutex::new(None), req: Mutex::new(None), wheel_f: Mutex::new(168.0), dbg_req: Mutex::new((0, 0, 0, 0.0)), last_resets: std::sync::atomic::AtomicU32::new(0), f_learned: std::sync::atomic::AtomicBool::new(false), timeouts: std::sync::atomic::AtomicU32::new(0),
        })
    }

    /// 新连接就位：旧连接留下的在途计数作废
    pub fn reset_flow(&self) { self.pending.lock().unwrap().clear(); let s = self.sent.load(Ordering::Relaxed); self.acked.store(s, Ordering::Relaxed); }

    pub fn up_event(&self, json: String) { let _ = self.up_tx.send(json); }

    pub fn notify_frame(&self) {
        let mut w = self.work.lock().unwrap(); let now = Instant::now(); *self.last_push.lock().unwrap() = now;
        w.dirty = true; w.last = Some(now); if w.first.is_none() { w.first = Some(now) }
        drop(w); self.cv.notify_all()
    }
    pub fn kick(&self) { self.work.lock().unwrap().kick = true; self.cv.notify_all() }
    /// 只是回来看看 Overlay 该不该 Commit / Drop：让编码线程转一圈，但不算“画面在变”（is_idle 不看它）
    pub fn kick_soft(&self) { self.work.lock().unwrap().soft = true; self.cv.notify_all() }
    pub fn wake(&self) { self.cv.notify_all() }

    pub fn inflight(&self) -> u64 { self.sent.load(Ordering::Relaxed).saturating_sub(self.acked.load(Ordering::Relaxed)) }

    pub fn on_ack(&self, seq: u64) {
        let mut p = self.pending.lock().unwrap();
        let mut cum = None;
        while let Some(&(s, c)) = p.front() { if s <= seq { cum = Some(c); p.pop_front(); } else { break } }
        if let Some(c) = cum { self.acked.fetch_max(c, Ordering::Relaxed); }
        drop(p); self.cv.notify_all();
    }

    pub fn send_ctl(&self, d: &Down) -> bool {
        match self.conn.lock().unwrap().as_ref() { Some(c) => c.ctl.send(frame(d)).is_ok(), None => false }
    }

    /// 在 tx 流上发一条（带 seq）；返回是否进了队列。`seq_of` 由调用方在构造消息时用 `next_seq` 取。
    pub fn next_seq(&self) -> u64 { self.tx_seq.fetch_add(1, Ordering::Relaxed) + 1 }

    pub fn send_tx(&self, seq: u64, d: &Down) -> bool {
        let bytes = frame(d); let n = bytes.len() as u64;
        let Some(tx) = self.conn.lock().unwrap().as_ref().map(|c| c.tx.clone()) else { return false };
        let cum = self.sent.fetch_add(n, Ordering::Relaxed) + n;
        self.pending.lock().unwrap().push_back((seq, cum));
        tx.blocking_send(bytes).is_ok()
    }

    /// 新会话 / 重连 / `Resync`：A 的缓存作废，整屏重发。
    pub fn begin(&self, w: usize, h: usize) {
        {
            let mut e = self.enc.lock().unwrap();
            match e.as_mut() { Some(en) if en.w == w && en.h == h => en.reset_a(), _ => *e = Some(Encoder::new(w, h)) }
        }
        {
            let mut f = self.frame.lock().unwrap();
            if f.w != w || f.h != h { f.buf = vec![0; w * h * 4]; f.w = w; f.h = h }
        }
        *self.stats.lock().unwrap() = StatsAcc::default();
        self.rev.store(0, Ordering::Relaxed);
        *self.view.lock().unwrap() = None; *self.req.lock().unwrap() = None;   // 新会话：A 的视口还没报，不追旧目标
        let seq = self.next_seq();
        self.send_tx(seq, &Down::Page { seq, id: PAGE, op: PageOp::Replace, temp: true, modal: None, at_root: true,
            regions: vec![Region { id: 0, kind: RegionKind::Fixed { anchor: 0, z: 0 }, rect: Rect { x: 0, y: 0, w: w as u32, h: h as u32 }, start_y: 0 }] });
        self.kick();
    }

    /// 链路空闲：没有待编的帧 / 催促 / 压后条带，发出的都已被 A 应用，且最近一次推帧已过去一段时间
    pub fn is_idle(&self) -> bool { self.idle_why().is_empty() }

    /// 不空闲的原因（空串 = 空闲）：调试口 / runner 看卡在哪
    pub fn idle_why(&self) -> &'static str {
        let w = self.work.lock().unwrap();
        // 滚动：没有在途的滚动请求，且采集头已经追上 A 报的视口。（锁一个取一个、立刻放：drive_scroll 是 enc → req → view 的顺序）
        if self.req.lock().unwrap().is_some() { return "scroll_req" }
        let view = *self.view.lock().unwrap();
        let sc = self.enc.lock().unwrap().as_ref().and_then(|e| e.scroll_head().map(|(h, _)| (h, e.scroll_edges())));
        if let (Some(v), Some((head, (top, bot)))) = (view, sc) {
            let diff = v as i64 - head; if diff != 0 && !((diff > 0 && bot) || (diff < 0 && top)) { return "head_not_at_view" }
        }
        let applied = self.va.lock().unwrap().as_ref().map_or(self.tx_seq.load(Ordering::Relaxed), |v| v.applied_seq);
        if w.dirty { return "dirty" } if w.kick { return "kick" } if self.deferring.load(Ordering::Relaxed) { return "deferring" }
        if applied != self.tx_seq.load(Ordering::Relaxed) { return "unapplied" }
        if self.last_push.lock().unwrap().elapsed() < Duration::from_millis(150) { return "recent_frame" }
        ""
    }

    pub fn set_view(&self, y: i32) { *self.view.lock().unwrap() = Some(y); self.kick() }

    /// 采集头调度（30 §3）：让 B 的滚动位置追上 A 的视口。每次一小步（≤ 区域高度的 60%，保证相邻两帧有足够重叠供位移检测），
    /// 一次只有一个在途请求；用滚轮精确公式 `v = (N + 0.5) / F`，F 由实测位移自学。
    pub fn drive_scroll(&self, shift: i32) {
        let mut g = self.enc.lock().unwrap();
        let Some(enc) = g.as_mut() else { return };
        let Some((head, rect)) = enc.scroll_head() else { return };
        let mut req = self.req.lock().unwrap();
        if let Some(r) = req.as_ref() {
            if shift != 0 {
                let mut f = self.wheel_f.lock().unwrap();
                *f = rd::heuristics::update_wheel_factor(*f, r.notches.abs(), r.step as f64, shift as f64);
                self.f_learned.store(true, Ordering::Relaxed); self.timeouts.store(0, Ordering::Relaxed);
                *req = None;
            } else if r.t.elapsed() > Duration::from_millis(700) {
                // 一次没响应可能只是慢（注入 + 出帧 + 合并等待）：重发一次；连续两次才算“滚不动了”= 到边
                if self.timeouts.fetch_add(1, Ordering::Relaxed) >= 1 { enc.expect_timeout(); self.timeouts.store(0, Ordering::Relaxed) } else { enc.set_expect(None) }
                *req = None; self.dbg_req.lock().unwrap().1 += 1
            } else { return }
        }
        let Some(ty) = *self.view.lock().unwrap() else { return };
        let diff = ty as i64 - head;
        if diff == 0 { return }
        let (top, bot) = enc.scroll_edges();
        if (diff > 0 && bot) || (diff < 0 && top) { return }
        // 滚轮因子还没量过（因容器而异，差 2 倍以上）：先走一小步量出来，免得第一步冲过头
        let max = if self.f_learned.load(Ordering::Relaxed) { (rect.h as i64 * 6 / 10).max(8) } else { 160 };
        let step = diff.clamp(-max, max) as i32;
        let f = *self.wheel_f.lock().unwrap();
        let notches = (step.abs() as f64 + 0.5) / f * step.signum() as f64;
        enc.set_expect(Some(step));
        *req = Some(ScrollReq { t: Instant::now(), notches, step }); { let mut d = self.dbg_req.lock().unwrap(); d.0 += 1; d.2 = step; d.3 = notches; }
        self.up_event(format!("{{\"ev\":\"scroll\",\"x\":{},\"y\":{},\"notches\":{:.5}}}", self.frame.lock().unwrap().w / 2, rect.y + rect.h / 2, notches));
    }

    /// 点击换算与点前校验（30 §6、10 §5）：内容坐标 y → 屏幕 y。该内容行 A 有的 hash 必须等于 B 屏幕上现在那一行，否则 Stale（不点）。
    /// B 的采集头还没追上视口时最多等 `wait_ms`。返回 JSON：{ok:true, sy} / {ok:false, why:"stale|offscreen|unknown"}
    pub fn resolve_tap(&self, cy: i64, wait_ms: u64) -> serde_json::Value {
        let t0 = Instant::now();
        loop {
            let r = {
                let g = self.enc.lock().unwrap();
                let Some(enc) = g.as_ref() else { return serde_json::json!({"ok": false, "why": "nosession"}) };
                match (enc.scroll_head(), enc.scroll_mask()) {
                    (Some((head, rect)), Some(mask)) => {
                        let sy = rect.y as i64 + (cy - head);
                        if sy < rect.y as i64 || sy >= (rect.y + rect.h) as i64 { Err("offscreen") }
                        else {
                            let Some(have) = enc.scroll_known(cy) else { return serde_json::json!({"ok": false, "why": "unknown"}) };
                            let f = self.frame.lock().unwrap(); let rb = f.w * 4;
                            let now = rd::hash::row_hash_masked(&f.buf[sy as usize * rb..(sy as usize + 1) * rb], &mask);
                            if now == have { Ok(sy) } else { Err("stale") }
                        }
                    }
                    _ => return serde_json::json!({"ok": false, "why": "noscroll"}),
                }
            };
            match r {
                Ok(sy) => return serde_json::json!({"ok": true, "sy": sy}),
                Err(why) => { if t0.elapsed() >= Duration::from_millis(wait_ms) { return serde_json::json!({"ok": false, "why": why}) } std::thread::sleep(Duration::from_millis(25)) }
            }
        }
    }

    pub fn end(&self) { *self.enc.lock().unwrap() = None }

    pub fn stats_down(&self) -> Down {
        let s = self.stats.lock().unwrap();
        let conn_rtt = self.conn.lock().unwrap().as_ref().map(|c| c.conn.as_ref().map_or(0.0, |x| x.rtt().as_secs_f64() * 1000.0)).unwrap_or(0.0);
        Down::Stats(Stats {
            layers: vec![("bg".into(), s.layers[0]), ("lit".into(), s.layers[1]), ("fg".into(), s.layers[2])],
            saved: vec![],
            extra: vec![
                ("t_ms".into(), self.t0.elapsed().as_secs_f64() * 1000.0), ("tx_bytes".into(), s.tx_bytes as f64), ("frames".into(), s.frames as f64),
                ("rows".into(), s.rows_sent as f64), ("enc_ms".into(), s.enc_ms_last), ("enc_ms_total".into(), s.enc_ms_total),
                ("inflight".into(), self.inflight() as f64), ("rtt_ms".into(), conn_rtt),
            ],
        })
    }
}

/// 编码线程。永不返回。
pub fn encoder_loop(core: Arc<Core>) {
    loop {
        // 1. 等活：有新帧或有人催（会话开始 / 参数变了 / 视口变了），并且有连接、有会话
        {
            let mut w = core.work.lock().unwrap();
            loop {
                let ready = (w.dirty || w.kick || w.soft) && core.conn.lock().unwrap().is_some() && core.enc.lock().unwrap().is_some();
                if ready { break }
                // 有在途的滚动请求却一直没有帧响应（滚不动了）：到时间要能判超时，不能等下一帧
                if core.req.lock().unwrap().is_some() { drop(w); core.drive_scroll(0); std::thread::sleep(Duration::from_millis(30)); w = core.work.lock().unwrap(); continue }
                w = core.cv.wait_timeout(w, Duration::from_millis(200)).unwrap().0;
            }
        }
        // 1b. 合并：画面还在连续变化时多等一会儿（安静 coalesce_ms 才编，最多拖 coalesce_max_ms）。
        //     启动动画 / 加载中的中间状态不必一帧一发；“稳定”用帧到达的间隔判断
        {
            let (q, mx) = { let p = core.params.lock().unwrap(); (p.get("coalesce_ms"), p.get("coalesce_max_ms")) };
            loop {
                let (last, first, kick) = { let w = core.work.lock().unwrap(); (w.last, w.first, w.kick) };
                if kick { break }
                let (Some(l), Some(f)) = (last, first) else { break };
                let (quiet, age) = (l.elapsed().as_secs_f64() * 1000.0, f.elapsed().as_secs_f64() * 1000.0);
                if quiet >= q || age >= mx { break }
                std::thread::sleep(Duration::from_millis(((q - quiet).min(mx - age)).max(1.0) as u64));
            }
        }
        // 2. 背压：在途未确认字节超过上限就不编，等 Ack（恢复后取最新一帧，中间帧自然被丢）
        let cap = (core.params.lock().unwrap().get("inflight_kb") * 1024.0) as u64;
        if core.inflight() > cap {
            let w = core.work.lock().unwrap();
            let _ = core.cv.wait_timeout(w, Duration::from_millis(10)).unwrap();
            continue;
        }
        { let mut w = core.work.lock().unwrap(); w.dirty = false; w.kick = false; w.soft = false; w.first = None; }
        // 3. 编
        let p = core.params.lock().unwrap().clone();
        let t = Instant::now();
        let (ops, rows, defer, regions, shift, resets, ov_pending) = {
            let mut g = core.enc.lock().unwrap();
            let Some(enc) = g.as_mut() else { continue };
            { let f = core.frame.lock().unwrap(); if f.w == enc.w && f.h == enc.h { enc.push_frame(&f.buf, f.w * 4); } }
            let o = enc.encode(&p, core.t0.elapsed().as_millis() as u64); (o.ops, o.rows, o.defer, o.regions, enc.last_dy, enc.n_reset, o.ov_pending)
        };
        let more = defer; core.deferring.store(defer, Ordering::Relaxed);
        // 换页（滚动画布被清空）：旧视口目标作废，等 A 重新报
        if resets != core.last_resets.swap(resets, Ordering::Relaxed) { *core.view.lock().unwrap() = None; *core.req.lock().unwrap() = None }
        core.drive_scroll(shift);
        if let Some(rs) = regions { let seq = core.next_seq(); core.send_tx(seq, &Down::Regions { seq, page: PAGE, regions: rs }); }
        if ops.is_empty() { if more { std::thread::sleep(Duration::from_millis(50)); core.kick() } else if ov_pending { std::thread::sleep(Duration::from_millis(50)); core.kick_soft() } continue }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        // 4. 统计（逐 Op 的线上字节）
        let mut layers = [0u64; 3]; let mut kinds: Vec<(&'static str, u64)> = vec![];
        for op in &ops {
            let n = to_postcard(op).len() as u64;
            let li = match op.layer() { "bg" => 0, "lit" => 1, _ => 2 };
            layers[li] += n;
            match kinds.iter_mut().find(|k| k.0 == op.kind()) { Some(k) => k.1 += n, None => kinds.push((op.kind(), n)) }
        }
        let seq = core.next_seq(); let rev = core.rev.fetch_add(1, Ordering::Relaxed) as u32 + 1;
        let msg = Down::Tx { seq, page: PAGE, rev, ops };
        let n = frame(&msg).len() as u64;
        if core.send_tx(seq, &msg) {
            let mut s = core.stats.lock().unwrap();
            for i in 0..3 { s.layers[i] += layers[i] }
            for (k, b) in kinds { match s.by_kind.iter_mut().find(|x| x.0 == k) { Some(x) => x.1 += b, None => s.by_kind.push((k.to_string(), b)) } }
            s.frames += 1; s.rows_sent += rows as u64; s.enc_ms_last = ms; s.enc_ms_total += ms; s.tx_bytes += n; s.tx_msgs += 1;
        }
        if more { std::thread::sleep(Duration::from_millis(50)); core.kick() }   // 有压后的条带：过一会儿再编（等 A11y 提案）
        else if ov_pending { std::thread::sleep(Duration::from_millis(50)); core.kick_soft() }   // 有在途的 Overlay：回来判断 Commit / Drop
    }
}
