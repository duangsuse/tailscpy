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
    pub conn: wtransport::Connection,
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
struct Work { dirty: bool, kick: bool, last: Option<Instant>, first: Option<Instant> }

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
}

pub const PAGE: PageId = 1;

impl Core {
    pub fn new(token: String) -> Arc<Core> {
        let (up_tx, up_rx) = std::sync::mpsc::channel();
        Arc::new(Core {
            up_tx, up_rx: Mutex::new(up_rx), params: Mutex::new(Params::new()), apps: Mutex::new(vec![]), token,
            conn: Mutex::new(None), conn_seq: AtomicU64::new(0), enc: Mutex::new(None), frame: Mutex::new(Frame { buf: vec![], w: 0, h: 0 }),
            work: Mutex::new(Work::default()), cv: Condvar::new(), tx_seq: AtomicU64::new(0), sent: AtomicU64::new(0), acked: AtomicU64::new(0),
            pending: Mutex::new(VecDeque::new()), stats: Mutex::new(StatsAcc::default()), rev: AtomicU64::new(0), t0: Instant::now(),
        })
    }

    /// 新连接就位：旧连接留下的在途计数作废
    pub fn reset_flow(&self) { self.pending.lock().unwrap().clear(); let s = self.sent.load(Ordering::Relaxed); self.acked.store(s, Ordering::Relaxed); }

    pub fn up_event(&self, json: String) { let _ = self.up_tx.send(json); }

    pub fn notify_frame(&self) {
        let mut w = self.work.lock().unwrap(); let now = Instant::now();
        w.dirty = true; w.last = Some(now); if w.first.is_none() { w.first = Some(now) }
        drop(w); self.cv.notify_all()
    }
    pub fn kick(&self) { self.work.lock().unwrap().kick = true; self.cv.notify_all() }
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
        let seq = self.next_seq();
        self.send_tx(seq, &Down::Page { seq, id: PAGE, op: PageOp::Replace, temp: true, modal: None, at_root: true,
            regions: vec![Region { id: 0, kind: RegionKind::Fixed { anchor: 0, z: 0 }, rect: Rect { x: 0, y: 0, w: w as u32, h: h as u32 }, start_y: 0 }] });
        self.kick();
    }

    pub fn end(&self) { *self.enc.lock().unwrap() = None }

    pub fn stats_down(&self) -> Down {
        let s = self.stats.lock().unwrap();
        let conn_rtt = self.conn.lock().unwrap().as_ref().map(|c| c.conn.rtt().as_secs_f64() * 1000.0).unwrap_or(0.0);
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
        // 1. 等活：有新帧或有人催（会话开始 / 参数变了），并且有连接、有会话
        {
            let mut w = core.work.lock().unwrap();
            loop {
                let ready = (w.dirty || w.kick) && core.conn.lock().unwrap().is_some() && core.enc.lock().unwrap().is_some();
                if ready { break }
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
        { let mut w = core.work.lock().unwrap(); w.dirty = false; w.kick = false; w.first = None; }
        // 3. 编
        let p = core.params.lock().unwrap().clone();
        let t = Instant::now();
        let (ops, rows) = {
            let mut g = core.enc.lock().unwrap();
            let Some(enc) = g.as_mut() else { continue };
            { let f = core.frame.lock().unwrap(); if f.w == enc.w && f.h == enc.h { enc.push_frame(&f.buf, f.w * 4); } }
            let o = enc.encode(&p); (o.ops, o.rows)
        };
        if ops.is_empty() { continue }
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
    }
}
