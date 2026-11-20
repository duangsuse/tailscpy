//! B 端编码器（40 §1、§3、§4、§6、§7）：**对“A 已有的内容”求差**，不是对上一帧。
//!
//! 两类区域：
//! - **Fixed（区域 0，整屏，屏幕坐标）**：A 的状态 = 每行 hash 表 + 影子画布（源像素）。
//! - **Scroll（区域 1，A11y 提案的竖直滚动区）**：按**内容坐标**（`wire::ORIGIN` 起）寻址的长截屏。B 每帧先用上一帧的行 hash 求位移 `dy`
//!   （`heuristics::detect_shift`，刚注入滚动时先验证预期位移），采集头 `head += dy`；屏幕第 r 行的内容坐标 = `head + r`；
//!   `known: 内容行 → hash` 里已有的行 A 已经有了，**不发**（回程零流量），只发缺的行。位移认不出来且大部分行都变了 → 当作换页，清空重来。
//!   右缘 `sb_w` 窄带不进 hash 也不发（滚动条滑块不会被写进画布），A 合成时用左邻列延伸。
//!
//! M2 的简化（90 §M2）：一个页面；滚动画布只有主画布一张（没有零散段）；没有 Overlay（任务 20 紧接着做）。
//! 下行是一条有序流，发出去的就算 A 已有，所以不用等确认。
//! **不读墙钟、不做 I/O**：时间不进来，同一输入序列产出完全相同的 Op。

use crate::dec::{LAYER_BG, LAYER_LIT};
use crate::hash::{fold, row_hash_masked, Mask};
use crate::heuristics::{detect_shift, ShiftCfg};
use crate::lit::encode as le;
use crate::lit::FxMap;
use crate::params::Params;
use crate::region::column_mask;
use crate::wire::*;
use std::collections::{HashMap, HashSet};

struct ScrollEnc {
    rect: Rect,
    smask: Mask,
    sb: u32,
    /// 屏幕第一行（rect.y）对应的画布 y
    head: i64,
    known: HashMap<i64, u64>,
    lit: HashSet<i64>,
    prev_h: Vec<u64>,
    prev_ok: bool,
    at_top: bool,
    at_bottom: bool,
    last_known: (i32, i32, bool, bool),
    last_head: i64,
}

pub struct Encoder {
    pub w: usize,
    pub h: usize,
    cur: Vec<u8>,
    shadow: Vec<u8>,
    cur_hash: Vec<u64>,
    info: Vec<bool>,
    stale: Vec<bool>,
    a_hash: Vec<u64>,
    a_valid: Vec<bool>,
    layer: Vec<u8>,
    fmask: Mask,
    hints: Vec<Rect>,
    defer_since: Option<u64>,
    scroll: Option<ScrollEnc>,
    proposal: Option<Rect>,
    expect: Option<i32>,
}

/// `defer`：有“像素上像照片、但 A11y 还没确认”的条带被压后了（图片先别当无损发），调用方过一会儿要再编一次
/// `regions`：区域有变化时给出新的区域表，调用方必须在这批 Op 之前先发 `Down::Regions`
pub struct Out { pub ops: Vec<Op>, pub rows: u32, pub defer: bool, pub regions: Option<Vec<Region>> }

fn remap(op: Op, region: u16, f: &dyn Fn(u32) -> u32) -> Op {
    let m = |d: Dst| Dst { region, y: f(d.y), ..d };
    match op {
        Op::Fill { dst, color } => Op::Fill { dst: m(dst), color },
        Op::Lit { dst, enc, dict, data } => Op::Lit { dst: m(dst), enc, dict, data },
        Op::Image { dst, sprite, q, mask, data } => Op::Image { dst: m(dst), sprite, q, mask, data },
        o => o,
    }
}

impl Encoder {
    pub fn new(w: usize, h: usize) -> Encoder {
        Encoder {
            w, h, cur: vec![0; w * h * 4], shadow: vec![0; w * h * 4], cur_hash: vec![0; h], info: vec![false; h], stale: vec![true; h],
            a_hash: vec![0; h], a_valid: vec![false; h], layer: vec![0; w * h], fmask: Mask::full(w as u32), hints: vec![], defer_since: None,
            scroll: None, proposal: None, expect: None,
        }
    }

    /// A 的缓存作废（新会话 / `Resync` / `Welcome{Reset}`）：之后整屏都是“缺”的，Scroll 区域重新按提案建。
    pub fn reset_a(&mut self) {
        self.a_valid.fill(false); self.layer.fill(0); self.shadow.fill(0); self.scroll = None; self.stale.fill(true);
    }

    /// A11y 给的图片矩形提案（屏幕像素）。只是提案：要过像素确认才会走 JPEG。
    pub fn set_hints(&mut self, h: Vec<Rect>) { self.hints = h }

    /// A11y 给的 Scroll 区域提案（屏幕像素，取其竖直范围，宽度一律整屏）。None = 没有可滚动区域。
    pub fn set_scroll_proposal(&mut self, r: Option<Rect>) { self.proposal = r.map(|r| Rect { x: 0, y: r.y.min(self.h as u32), w: self.w as u32, h: r.h.min((self.h as u32).saturating_sub(r.y)) }).filter(|r| r.h >= 64) }

    /// 刚注入了一次滚动，预期这一帧起位移约为 dy（像素，往下滚为正）；None = 没有
    pub fn set_expect(&mut self, dy: Option<i32>) { self.expect = dy }

    /// 预期的滚动一直没有帧响应（滚不动了）：按方向记“到边”
    pub fn expect_timeout(&mut self) {
        if let (Some(e), Some(s)) = (self.expect.take(), self.scroll.as_mut()) { if e > 0 { s.at_bottom = true } else if e < 0 { s.at_top = true } }
    }

    /// 采集头（屏幕第一行对应的画布 y）和滚动区矩形
    pub fn scroll_head(&self) -> Option<(i64, Rect)> { self.scroll.as_ref().map(|s| (s.head, s.rect)) }
    pub fn scroll_edges(&self) -> (bool, bool) { self.scroll.as_ref().map_or((false, false), |s| (s.at_top, s.at_bottom)) }

    /// 拷入最新一帧（stride = 每行字节数）。只拷变化的行并标记 hash 过期；返回变化行数。
    pub fn push_frame(&mut self, frame: &[u8], stride: usize) -> usize {
        let rb = self.w * 4; let mut n = 0;
        for y in 0..self.h {
            let s = &frame[y * stride..y * stride + rb]; let d = &mut self.cur[y * rb..(y + 1) * rb];
            if s != d { d.copy_from_slice(s); self.stale[y] = true; n += 1 }
        }
        n
    }

    pub fn cur(&self) -> &[u8] { &self.cur }

    /// 当前帧几乎是纯色（受保护内容 / App 没拉起来 → `Fault{BlankFrame}`，40 §11）
    pub fn is_blank(&self) -> bool {
        let first = &self.cur[..4];
        self.cur.chunks_exact(4).all(|p| p == first) && self.w > 0
    }

    fn in_scroll(&self, y: usize) -> bool { self.scroll.as_ref().map_or(false, |s| y >= s.rect.y as usize && y < (s.rect.y + s.rect.h) as usize) }

    /// 像素确认：A11y 说是图片，像素上也要“颜色多、几乎没有长游程”。
    fn is_photo(&self, r: &Rect, p: &Params) -> bool {
        let rb = self.w * 4; let (w, h) = (r.w as usize, r.h as usize);
        let step = (((w * h) as f64 / 4096.0).sqrt().ceil() as usize).max(1);
        let mut set: FxMap<u32, ()> = FxMap::default(); let (mut n, mut eq) = (0u32, 0u32);
        let mut y = 0;
        while y < h {
            let mut x = 0;
            while x + 1 < w {
                let o = (r.y as usize + y) * rb + (r.x as usize + x) * 4;
                let c = u32::from_le_bytes([self.cur[o], self.cur[o + 1], self.cur[o + 2], 0]);
                let c2 = u32::from_le_bytes([self.cur[o + 4], self.cur[o + 5], self.cur[o + 6], 0]);
                set.insert(c, ()); n += 1; if c == c2 { eq += 1 }
                x += step;
            }
            y += step;
        }
        n >= 16 && set.len() as f64 >= p.get("img_min_colors") * (n as f64 / 4096.0).min(1.0) && (eq as f64) < 0.5 * n as f64
    }

    fn rect_differs(&self, r: &Rect) -> bool {
        let rb = self.w * 4;
        (r.y..r.y + r.h).any(|y| {
            let y = y as usize;
            !self.a_valid[y] || self.cur[y * rb + r.x as usize * 4..y * rb + (r.x + r.w) as usize * 4] != self.shadow[y * rb + r.x as usize * 4..y * rb + (r.x + r.w) as usize * 4]
        })
    }

    fn jpeg(&self, r: &Rect, q: u8) -> Vec<u8> {
        let rb = self.w * 4; let mut rgb = Vec::with_capacity((r.w * r.h * 3) as usize);
        for y in r.y..r.y + r.h { let o = y as usize * rb + r.x as usize * 4; for px in self.cur[o..o + r.w as usize * 4].chunks_exact(4) { rgb.extend_from_slice(&px[..3]) } }
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, q).encode(&rgb, r.w as u16, r.h as u16, jpeg_encoder::ColorType::Rgb).expect("jpeg 编码");
        out
    }

    /// `defer_photo`：颜色多得放弃直方图（像照片）的条带返回 None，由调用方压后
    fn lit_rect(&self, x: usize, y: usize, w: usize, h: usize, level: i32, defer_photo: bool) -> Option<Op> {
        let rb = self.w * 4; let dst = Dst { region: 0, x: x as u32, y: y as u32, w: w as u32, h: h as u32 };
        let hist = le::histogram(&self.cur, rb, x, y, w, h, 4096);
        if hist.is_none() && defer_photo { return None }
        Some(match hist {
            Some(m) if m.len() == 1 => { let c = *m.keys().next().unwrap(); Op::Fill { dst, color: [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255] } }
            Some(m) if m.len() <= 256 => {
                let raw = le::palette_raw(&self.cur, rb, x, y, w, h, &m, 1.0).expect("palette");
                Op::Lit { dst, enc: LitEnc::Palette, dict: None, data: le::zstd_encode(&raw, level) }
            }
            Some(m) => {
                // 颜色多：Palette+转义 与 Residual 两种都编，取小（40 §6）
                let res = le::zstd_encode(&le::residual_raw(&self.cur, rb, x, y, w, h), level);
                match le::palette_raw(&self.cur, rb, x, y, w, h, &m, 0.03) {
                    Some(raw) => { let pal = le::zstd_encode(&raw, level); if pal.len() <= res.len() { Op::Lit { dst, enc: LitEnc::Palette, dict: None, data: pal } } else { Op::Lit { dst, enc: LitEnc::Residual, dict: None, data: res } } }
                    None => Op::Lit { dst, enc: LitEnc::Residual, dict: None, data: res },
                }
            }
            None => Op::Lit { dst, enc: LitEnc::Residual, dict: None, data: le::zstd_encode(&le::residual_raw(&self.cur, rb, x, y, w, h), level) },
        })
    }

    /// 这个矩形里 A 现在是不是全是无损内容（没有有损像素，且已经写过）：晚到的 A11y 图片提案据此决定要不要改发 JPEG
    fn rect_lossless_known(&self, r: &Rect) -> bool {
        (r.y..r.y + r.h).all(|y| { let o = y as usize * self.w + r.x as usize; self.layer[o..o + r.w as usize].iter().all(|&l| l == LAYER_BG) })
    }

    /// 一条横带（屏幕坐标）：把 `images` 挖掉之后按条带编码。返回的 Op 是屏幕坐标、区域 0；被压后的行记进 `deferred`。
    fn lit_band(&self, y0: usize, y1: usize, x0: usize, x1: usize, images: &[Rect], level: i32, strip: usize, defer_on: bool, deferred: &mut [bool], ops: &mut Vec<Op>) {
        let mut cuts = vec![y0, y1];
        for r in images { for c in [r.y as usize, (r.y + r.h) as usize] { if c > y0 && c < y1 { cuts.push(c) } } }
        cuts.sort(); cuts.dedup();
        for sg in cuts.windows(2) {
            let (s0, s1) = (sg[0], sg[1]);
            let mut cover: Vec<(usize, usize)> = images.iter().filter(|r| (r.y as usize) <= s0 && ((r.y + r.h) as usize) >= s1).map(|r| (r.x as usize, (r.x + r.w) as usize)).collect();
            cover.sort();
            let mut xs = x0; let mut free: Vec<(usize, usize)> = vec![];
            for (a, b) in cover { if a > xs { free.push((xs, a.min(x1))) } xs = xs.max(b) }
            if xs < x1 { free.push((xs, x1)) }
            for (a, b) in free { if a >= b { continue }
                let mut yy = s0;
                while yy < s1 { let hh = strip.min(s1 - yy);
                    match self.lit_rect(a, yy, b - a, hh, level, defer_on) { Some(op) => ops.push(op), None => for r in yy..yy + hh { deferred[r] = true } }
                    yy += hh }
            }
        }
    }

    fn regions_msg(&self) -> Vec<Region> {
        let mut v = vec![Region { id: 0, kind: RegionKind::Fixed { anchor: 0, z: 0 }, rect: Rect { x: 0, y: 0, w: self.w as u32, h: self.h as u32 }, start_y: 0 }];
        if let Some(s) = &self.scroll { v.push(Region { id: 1, kind: RegionKind::Scroll, rect: s.rect, start_y: s.head as i32 }) }
        v
    }

    /// 提案和当前 Scroll 区域对不上：建 / 撤 / 重建。首次建时把已按 Fixed 发出的行用同页 `Copy` 搬进去，不重发像素。
    fn sync_scroll(&mut self, p: &Params, ops: &mut Vec<Op>) -> Option<Vec<Region>> {
        let want = self.proposal;
        if self.scroll.as_ref().map(|s| s.rect) == want { return None }
        let old = self.scroll.take();
        self.stale.fill(true);
        let Some(rect) = want else {
            if let Some(o) = old { for y in o.rect.y..o.rect.y + o.rect.h { self.a_valid[y as usize] = false } }   // 撤销后这些行按 Fixed 重发
            return Some(self.regions_msg());
        };
        let sb = p.get("sb_w") as u32; let rb = self.w * 4;
        let mut s = ScrollEnc { rect, smask: column_mask(self.w as u32, sb, &[]), sb, head: ORIGIN as i64, known: HashMap::new(), lit: HashSet::new(), prev_h: vec![], prev_ok: false,
            at_top: false, at_bottom: false, last_known: (0, 0, false, false), last_head: i64::MIN };
        if old.is_none() {
            // 已按 Fixed 发出、A 里有的行 → Copy 进 Scroll 画布，known 里记 hash
            let (y0, y1) = (rect.y as usize, (rect.y + rect.h) as usize);
            let mut y = y0;
            while y < y1 {
                if !self.a_valid[y] { y += 1; continue }
                let st = y; while y < y1 && self.a_valid[y] { y += 1 }
                for yy in st..y {
                    let cy = ORIGIN as i64 + (yy - y0) as i64;
                    s.known.insert(cy, row_hash_masked(&self.shadow[yy * rb..(yy + 1) * rb], &s.smask));
                    if self.layer[yy * self.w..(yy + 1) * self.w].iter().any(|&l| l == LAYER_LIT) { s.lit.insert(cy); }
                }
                ops.push(Op::Copy { src: Src::Canvas { page: 0, region: 0, x: 0, y: st as u32 }, dst: Dst { region: 1, x: 0, y: ORIGIN + (st - y0) as u32, w: self.w as u32, h: (y - st) as u32 } });
            }
        }
        for y in rect.y..rect.y + rect.h { self.a_valid[y as usize] = false }
        ops.push(Op::Head { region: 1, y: ORIGIN as i32 });
        self.scroll = Some(s);
        Some(self.regions_msg())
    }

    /// 编一帧：返回要发的 Op（没有缺的就是空）。同时把这些 Op 记入 A 的状态。
    pub fn encode(&mut self, p: &Params, now_ms: u64) -> Out {
        let (w, h, rb) = (self.w, self.h, self.w * 4);
        let level = p.get("zstd_level") as i32; let strip = (p.get("strip_rows") as usize).max(1);
        let mut ops: Vec<Op> = vec![];
        let regions = self.sync_scroll(p, &mut ops);
        let (smask, scroll_rows) = match &self.scroll { Some(s) => (Some(s.smask.clone()), s.rect.y as usize..(s.rect.y + s.rect.h) as usize), None => (None, 0..0) };
        for y in 0..h {
            if self.stale[y] {
                let m = if scroll_rows.contains(&y) { smask.as_ref().unwrap() } else { &self.fmask };
                let row = &self.cur[y * rb..(y + 1) * rb];
                self.cur_hash[y] = row_hash_masked(row, m);
                self.info[y] = { let f = &row[..4]; !row.chunks_exact(4).all(|p| p == f) };
                self.stale[y] = false;
            }
        }
        let defer_ms = p.get("img_defer_ms") as u64;
        let defer_on = p.get("img_lossy") > 0.5 && defer_ms > 0 && now_ms.saturating_sub(*self.defer_since.get_or_insert(now_ms)) < defer_ms;
        let mut deferred = vec![false; h];
        let mut rows = 0u32;

        // ───────── Scroll 区域 ─────────
        let mut scroll_img_ops: Vec<Op> = vec![];
        if self.scroll.is_some() { rows += self.encode_scroll(p, defer_on, level, strip, &mut ops, &mut scroll_img_ops, &mut deferred); }

        // ───────── Fixed 区域（不含 Scroll 的行）─────────
        let missing: Vec<bool> = (0..h).map(|y| !scroll_rows.contains(&y) && (!self.a_valid[y] || self.a_hash[y] != self.cur_hash[y])).collect();
        let mut bands: Vec<(usize, usize, usize, usize)> = vec![];   // y0, y1, x0, x1
        let mut y = 0;
        while y < h {
            if !missing[y] { y += 1; continue }
            let y0 = y; while y < h && missing[y] { y += 1 }
            let (mut x0, mut x1) = (w, 0usize);
            for yy in y0..y {
                if !self.a_valid[yy] { x0 = 0; x1 = w; break }
                let (c, s) = (&self.cur[yy * rb..(yy + 1) * rb], &self.shadow[yy * rb..(yy + 1) * rb]);
                if c == s { continue }
                let f = c.iter().zip(s).position(|(a, b)| a != b).unwrap() / 4;
                let l = rb - 1 - c.iter().rev().zip(s.iter().rev()).position(|(a, b)| a != b).unwrap();
                x0 = x0.min(f); x1 = x1.max(l / 4 + 1);
            }
            if x0 < x1 { bands.push((y0, y, x0, x1)) }
            else { for yy in y0..y { self.a_hash[yy] = self.cur_hash[yy]; self.a_valid[yy] = true } }   // 只有被掩掉的列变了
        }

        // 图片：A11y 提案 + 像素确认 + 确实有变化（或晚到：已按无损发过）
        let mut images: Vec<Rect> = vec![];
        if p.get("img_lossy") > 0.5 {
            for hnt in self.hints.clone() {
                let r = Rect { x: hnt.x.min(w as u32), y: hnt.y.min(h as u32), w: hnt.w.min((w as u32).saturating_sub(hnt.x)), h: hnt.h.min((h as u32).saturating_sub(hnt.y)) };
                if r.w == 0 || r.h == 0 || r.w * r.h < p.get("img_min_area") as u32 { continue }
                if (r.y..r.y + r.h).any(|y| scroll_rows.contains(&(y as usize))) { continue }   // Scroll 区域里的图片在 encode_scroll 里处理
                let hit = bands.iter().any(|b| (b.0 as u32) < r.y + r.h && (b.1 as u32) > r.y) && self.rect_differs(&r);
                // A11y 晚到：这块已经按无损发过了，确认是照片就改发 JPEG（只损失省流，不出错图，50/60 规则 1）
                let late = !hit && self.rect_lossless_known(&r);
                if !(hit || late) || !self.is_photo(&r, p) { continue }
                if !images.iter().any(|i| i.x < r.x + r.w && r.x < i.x + i.w && i.y < r.y + r.h && r.y < i.y + i.h) { images.push(r) }
            }
        }
        let fixed_empty = bands.is_empty() && images.is_empty();
        if fixed_empty && ops.is_empty() && scroll_img_ops.is_empty() && regions.is_none() { return Out { ops: vec![], rows: 0, defer: deferred.iter().any(|&d| d), regions: None } }
        for &(y0, y1, x0, x1) in &bands { self.lit_band(y0, y1, x0, x1, &images, level, strip, defer_on, &mut deferred, &mut ops) }
        for r in &images { ops.push(Op::Image { dst: Dst { region: 0, x: r.x, y: r.y, w: r.w, h: r.h }, sprite: None, q: p.get("img_q") as u8, mask: vec![], data: self.jpeg(r, p.get("img_q") as u8) }) }
        ops.extend(scroll_img_ops);

        // 记入 A 的状态（Fixed）
        for &(y0, y1, _, _) in &bands { for yy in y0..y1 {
            if deferred[yy] { continue }   // 压后的行保持“缺”，下次再编
            self.shadow[yy * rb..(yy + 1) * rb].copy_from_slice(&self.cur[yy * rb..(yy + 1) * rb]);
            self.a_hash[yy] = self.cur_hash[yy]; self.a_valid[yy] = true; rows += 1;
        } }
        for op in &ops { if op.dst().map_or(false, |d| d.region == 0) && !matches!(op, Op::Copy { .. }) { let d = op.dst().unwrap(); let l = if matches!(op, Op::Image { .. }) { LAYER_LIT } else { LAYER_BG };
            for r in 0..d.h as usize { let o = (d.y as usize + r) * w + d.x as usize; self.layer[o..o + d.w as usize].fill(l) } } }

        // 校验（Fixed）：只覆盖本帧写过、且不含有损像素的连续行
        if p.get("verify") > 0.5 {
            let full = Mask::full(w as u32); let mut touched = vec![false; h];
            for &(y0, y1, _, _) in &bands { for yy in y0..y1 { touched[yy] = !deferred[yy] } }
            let clean = |yy: usize| touched[yy] && self.layer[yy * w..(yy + 1) * w].iter().all(|&l| l != LAYER_LIT);
            let mut yy = 0;
            while yy < h {
                if !clean(yy) { yy += 1; continue }
                let s = yy; while yy < h && clean(yy) { yy += 1 }
                let hs: Vec<u64> = (s..yy).map(|r| row_hash_masked(&self.shadow[r * rb..(r + 1) * rb], &full)).collect();
                ops.push(Op::Verify { region: 0, y0: s as u32, y1: yy as u32, fold: fold(&hs) });
            }
        }
        let defer = deferred.iter().any(|&d| d);
        if !defer { self.defer_since = None }
        Out { ops, rows, defer, regions }
    }

    /// Scroll 区域一帧：求位移 → 采集头 → 缺的行（内容坐标）→ 编码。返回写了多少行。
    fn encode_scroll(&mut self, p: &Params, defer_on: bool, level: i32, strip: usize, ops: &mut Vec<Op>, img_ops: &mut Vec<Op>, deferred: &mut [bool]) -> u32 {
        let (w, rb) = (self.w, self.w * 4);
        let cfg = ShiftCfg { min_ratio: p.get("shift_min_ratio"), min_rows: p.get("shift_min_rows") as u32, ..Default::default() };
        let page_change = p.get("page_change_ratio");
        let (rect, sb) = { let s = self.scroll.as_ref().unwrap(); (s.rect, s.sb as usize) };
        let (y0, rh) = (rect.y as usize, rect.h as usize);
        let ch: Vec<u64> = self.cur_hash[y0..y0 + rh].to_vec(); let info: Vec<bool> = self.info[y0..y0 + rh].to_vec();
        let expect = self.expect;
        let s = self.scroll.as_mut().unwrap();
        let mut dy = 0i32; let mut reset = false;
        if s.prev_ok {
            let same = ch.iter().zip(&s.prev_h).filter(|(a, b)| a == b).count();
            if same == rh {
                // 一点没变。预期了滚动却没动 → 到边的迹象由调用方的超时处理（expect_timeout）
            } else if let Some(sh) = detect_shift(&s.prev_h, &ch, &info, expect, &cfg) {
                dy = sh.dy; s.head += dy as i64;
                if let Some(e) = expect { if (dy.abs() as f64) < 0.5 * e.abs() as f64 { if e > 0 { s.at_bottom = true } else { s.at_top = true } } else { s.at_top = false; s.at_bottom = false } }
                self.expect = None;
            } else if (rh - same) as f64 > page_change * rh as f64 { reset = true }
        }
        if s.head < 1024 || s.head > 2 * ORIGIN as i64 { reset = true }
        if reset { s.known.clear(); s.lit.clear(); s.head = ORIGIN as i64; s.at_top = false; s.at_bottom = false; dy = 0; ops.push(Op::Clear { region: 1 }); self.expect = None; }
        let s = self.scroll.as_mut().unwrap();
        // 缺的行（内容坐标）
        let missing: Vec<bool> = (0..rh).map(|i| s.known.get(&(s.head + i as i64)) != Some(&ch[i])).collect();
        let mut written = 0u32;
        let mut bands: Vec<(usize, usize, usize, usize)> = vec![];   // 屏幕行 i0..i1（相对 rect），列 x0..x1
        let xmax = w - sb;
        let mut i = 0;
        while i < rh {
            if !missing[i] { i += 1; continue }
            let i0 = i; while i < rh && missing[i] { i += 1 }
            let (mut x0, mut x1) = (xmax, 0usize);
            for ii in i0..i {
                let cy = s.head + ii as i64; let pj = ii as i64 + dy as i64;
                if s.known.contains_key(&cy) && pj >= 0 && (pj as usize) < rh && s.prev_ok {
                    // 原地变化：和上一帧同一内容位置的行比，收窄列范围
                    let (c, pv) = (&self.cur[(y0 + ii) * rb..(y0 + ii) * rb + xmax * 4], &self.shadow[(y0 + pj as usize) * rb..(y0 + pj as usize) * rb + xmax * 4]);
                    if c == pv { continue }
                    let f = c.iter().zip(pv).position(|(a, b)| a != b).unwrap() / 4;
                    let l = c.len() - 1 - c.iter().rev().zip(pv.iter().rev()).position(|(a, b)| a != b).unwrap();
                    x0 = x0.min(f); x1 = x1.max(l / 4 + 1);
                } else { x0 = 0; x1 = xmax; break }
            }
            if x0 < x1 { bands.push((i0, i, x0, x1)) } else { for ii in i0..i { s.known.insert(s.head + ii as i64, ch[ii]); } }   // 只有被掩掉的列变了
        }
        // 图片（A11y 提案 ∩ 缺的行，像素确认）
        let mut images: Vec<Rect> = vec![];
        if p.get("img_lossy") > 0.5 {
            for hnt in self.hints.clone() {
                let (hx, hw) = (hnt.x.min(xmax as u32), hnt.w.min((xmax as u32).saturating_sub(hnt.x)));
                let ry0 = hnt.y.max(rect.y); let ry1 = (hnt.y + hnt.h).min(rect.y + rect.h);
                if hw == 0 || ry1 <= ry0 || hw * (ry1 - ry0) < p.get("img_min_area") as u32 { continue }
                // 只取落在某条缺行带里的部分
                for &(i0, i1, _, _) in &bands {
                    let (a, b) = (ry0.max((y0 + i0) as u32), ry1.min((y0 + i1) as u32));
                    if b <= a || hw * (b - a) < p.get("img_min_area") as u32 { continue }
                    let r = Rect { x: hx, y: a, w: hw, h: b - a };
                    if self.is_photo(&r, p) && !images.iter().any(|i| i.x < r.x + r.w && r.x < i.x + i.w && i.y < r.y + r.h && r.y < i.y + i.h) { images.push(r) }
                }
            }
        }
        let head = self.scroll.as_ref().unwrap().head;
        let to_canvas = move |sy: u32| -> u32 { (head + (sy as i64 - y0 as i64)) as u32 };
        let mut lit_ops: Vec<Op> = vec![];
        for &(i0, i1, x0, x1) in &bands {
            let mut sc_deferred = vec![false; self.h];
            self.lit_band(y0 + i0, y0 + i1, x0, x1, &images, level, strip, defer_on, &mut sc_deferred, &mut lit_ops);
            for r in y0 + i0..y0 + i1 { if sc_deferred[r] { deferred[r] = true } }
        }
        for op in lit_ops { ops.push(remap(op, 1, &to_canvas)) }
        for r in &images { img_ops.push(remap(Op::Image { dst: Dst { region: 0, x: r.x, y: r.y, w: r.w, h: r.h }, sprite: None, q: p.get("img_q") as u8, mask: vec![], data: self.jpeg(r, p.get("img_q") as u8) }, 1, &to_canvas)) }
        // 记入状态
        let s = self.scroll.as_mut().unwrap();
        for &(i0, i1, _, _) in &bands { for ii in i0..i1 {
            if deferred[y0 + ii] { continue }
            s.known.insert(s.head + ii as i64, ch[ii]); written += 1;
        } }
        for r in &images { for sy in r.y..r.y + r.h { s.lit.insert(s.head + (sy as i64 - y0 as i64)); } }
        // Head / Known
        if s.head != s.last_head { ops.push(Op::Head { region: 1, y: s.head as i32 }); s.last_head = s.head }
        let (kmin, kmax) = (s.known.keys().min().copied().unwrap_or(0) as i32, s.known.keys().max().map_or(0, |&m| m as i32 + 1));
        let kn = (kmin, kmax, s.at_top, s.at_bottom);
        if kn != s.last_known { ops.push(Op::Known { region: 1, y_min: kmin, y_max: kmax, at_top: s.at_top, at_bottom: s.at_bottom, total_h: None }); s.last_known = kn }
        // 校验（Scroll）：本帧写过、不含有损像素的连续内容行
        if p.get("verify") > 0.5 {
            let mut touched = vec![false; rh];
            for &(i0, i1, _, _) in &bands { for ii in i0..i1 { touched[ii] = !deferred[y0 + ii] } }
            let clean = |ii: usize| touched[ii] && !s.lit.contains(&(s.head + ii as i64));
            let mut ii = 0;
            while ii < rh {
                if !clean(ii) { ii += 1; continue }
                let a = ii; while ii < rh && clean(ii) { ii += 1 }
                ops.push(Op::Verify { region: 1, y0: (s.head + a as i64) as u32, y1: (s.head + ii as i64) as u32, fold: fold(&ch[a..ii]) });
            }
        }
        // 这一帧就是下一帧的“上一帧”
        s.prev_h = ch; s.prev_ok = true;
        for r in y0..y0 + rh { self.shadow[r * rb..(r + 1) * rb].copy_from_slice(&self.cur[r * rb..(r + 1) * rb]) }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dec::Canvas;

    const W: usize = 120; const H: usize = 200;

    fn text_frame(seed: u32) -> Vec<u8> {
        // 白底 + 几条“文字”（少量颜色）+ 一块彩色渐变（颜色多）
        let mut f = vec![255u8; W * H * 4];
        for y in 0..H { for x in 0..W {
            let o = (y * W + x) * 4;
            if y % 20 < 10 && (x * 7 + y * 3 + seed as usize) % 5 < 2 && y < 100 { f[o..o + 3].copy_from_slice(&[30, 30, 30]) }
            if y >= 100 && y < 140 { f[o] = (x * 2) as u8; f[o + 1] = (y * 3) as u8; f[o + 2] = ((x ^ y) * 5) as u8 }
        } }
        f
    }
    fn photo(f: &mut [u8], r: Rect, seed: u32) {
        let mut s = seed.wrapping_mul(2654435761).wrapping_add(1);
        for y in r.y..r.y + r.h { for x in r.x..r.x + r.w {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let o = (y as usize * W + x as usize) * 4; let v = (s >> 24) as u8;
            f[o] = v; f[o + 1] = v.wrapping_add((x * 3) as u8); f[o + 2] = v.wrapping_mul(3);
        } }
    }
    fn rows_equal(a: &[u8], b: &[u8], skip: &[Rect]) -> bool {
        for y in 0..H { for x in 0..W {
            if skip.iter().any(|r| (x as u32) >= r.x && (x as u32) < r.x + r.w && (y as u32) >= r.y && (y as u32) < r.y + r.h) { continue }
            let o = (y * W + x) * 4; if a[o..o + 3] != b[o..o + 3] { return false }
        } }
        true
    }
    fn run(enc: &mut Encoder, cv: &mut Canvas, p: &Params, frame: &[u8]) -> Out {
        enc.push_frame(frame, W * 4);
        let out = enc.encode(p, 0);
        let ap = cv.apply(&out.ops).unwrap();
        assert!(ap.verify_fail.is_empty(), "校验失败 {:?}", ap.verify_fail);
        out
    }
    fn size(o: &Out) -> usize { o.ops.iter().map(|op| to_postcard(op).len()).sum() }

    #[test] fn first_frame_is_lossless_and_second_identical_frame_is_empty() {
        let mut p = Params::new(); p.set("verify", 1.0);
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        let f = text_frame(0);
        let o = run(&mut e, &mut c, &p, &f);
        assert!(!o.ops.is_empty()); assert!(rows_equal(&c.px, &f, &[]));
        let o2 = run(&mut e, &mut c, &p, &f);
        assert!(o2.ops.is_empty(), "画面静止时下行必须为零");
    }
    #[test] fn small_change_sends_small_tx_with_column_bounds() {
        let p = Params::new();
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        let mut f = text_frame(0);
        let first = run(&mut e, &mut c, &p, &f);
        for y in 50..58 { for x in 30..40 { let o = (y * W + x) * 4; f[o..o + 3].copy_from_slice(&[200, 0, 0]) } }
        let o = run(&mut e, &mut c, &p, &f);
        assert!(rows_equal(&c.px, &f, &[]));
        assert!(size(&o) * 10 < size(&first), "{} vs {}", size(&o), size(&first));
        for op in &o.ops { let d = op.dst().unwrap(); assert!(d.x >= 30 && d.x + d.w <= 40, "列界应收窄 {:?}", d) }
    }
    #[test] fn multiple_frames_converge_regardless_of_dropped_intermediate() {
        // 中间帧丢弃不出错：只编“最新一帧对 A 已有”
        let p = Params::new();
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        run(&mut e, &mut c, &p, &text_frame(0));
        e.push_frame(&text_frame(1), W * 4); e.push_frame(&text_frame(2), W * 4);   // 两帧只编最后一次
        let f3 = text_frame(3); let out = { e.push_frame(&f3, W * 4); e.encode(&p, 0) };
        c.apply(&out.ops).unwrap();
        assert!(rows_equal(&c.px, &f3, &[]));
    }
    #[test] fn photo_hint_goes_jpeg_and_rest_stays_lossless() {
        let mut p = Params::new(); p.set("verify", 1.0);
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        let mut f = text_frame(0); let r = Rect { x: 10, y: 145, w: 96, h: 50 }; photo(&mut f, r, 1);
        e.set_hints(vec![r]);
        let o = run(&mut e, &mut c, &p, &f);
        let imgs: Vec<_> = o.ops.iter().filter(|op| matches!(op, Op::Image { .. })).collect();
        assert_eq!(imgs.len(), 1);
        let Op::Image { data, .. } = imgs[0] else { unreachable!() };
        assert_eq!(&data[..2], &[0xff, 0xd8]);   // JPEG SOI
        // 图片矩形内的像素这里（没有 JPEG 解码）保持占位，其余逐像素一致
        assert!(rows_equal(&c.px, &f, &[r]));
    }
    #[test] fn photo_without_hint_is_deferred_then_sent_lossless_after_timeout() {
        let p = Params::new();
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        let mut f = text_frame(0); let r = Rect { x: 10, y: 145, w: 96, h: 50 }; photo(&mut f, r, 1);
        e.push_frame(&f, W * 4);
        let o1 = e.encode(&p, 0); c.apply(&o1.ops).unwrap();
        assert!(o1.defer, "没有 A11y 提案的照片条带先压后");
        assert!(o1.ops.iter().all(|op| op.dst().map_or(true, |d| d.y + d.h <= 145 || d.y >= 192)), "文字部分照发，照片行不发");
        let o2 = e.encode(&p, 10_000); c.apply(&o2.ops).unwrap();   // 超时：照片按无损发
        assert!(!o2.defer); assert!(rows_equal(&c.px, &f, &[]));
    }
    #[test] fn deferred_photo_becomes_jpeg_when_hint_arrives() {
        let p = Params::new();
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        let mut f = text_frame(0); let r = Rect { x: 10, y: 145, w: 96, h: 50 }; photo(&mut f, r, 1);
        e.push_frame(&f, W * 4); let o1 = e.encode(&p, 0); c.apply(&o1.ops).unwrap(); assert!(o1.defer);
        e.set_hints(vec![r]);
        let o2 = e.encode(&p, 50); c.apply(&o2.ops).unwrap();
        assert!(o2.ops.iter().any(|op| matches!(op, Op::Image { .. })) && !o2.defer);
        assert!(rows_equal(&c.px, &f, &[r]));
    }
    #[test] fn late_hint_converts_already_sent_lossless_photo_to_jpeg() {
        let mut p = Params::new(); p.set("img_defer_ms", 0.0);
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        let mut f = text_frame(0); let r = Rect { x: 10, y: 145, w: 96, h: 50 }; photo(&mut f, r, 1);
        e.push_frame(&f, W * 4); let o1 = e.encode(&p, 0); c.apply(&o1.ops).unwrap();
        assert!(!o1.ops.iter().any(|op| matches!(op, Op::Image { .. })));
        e.set_hints(vec![r]);
        let o2 = e.encode(&p, 100); c.apply(&o2.ops).unwrap();
        assert_eq!(o2.ops.iter().filter(|op| matches!(op, Op::Image { .. })).count(), 1);
        let o3 = e.encode(&p, 200); assert!(o3.ops.is_empty(), "只转一次，不反复重发");
    }
    #[test] fn flat_hint_is_not_trusted() {
        // A11y 说是图片，像素却是纯色 / 少色：以像素为准，走 Lit（40 §11）
        let p = Params::new();
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        let f = text_frame(0); e.set_hints(vec![Rect { x: 0, y: 0, w: 100, h: 90 }]);
        let o = run(&mut e, &mut c, &p, &f);
        assert!(o.ops.iter().all(|op| !matches!(op, Op::Image { .. })));
        assert!(rows_equal(&c.px, &f, &[]));
    }
    #[test] fn lossy_off_uses_lossless_everywhere() {
        let mut p = Params::new(); p.set("img_lossy", 0.0);
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        let mut f = text_frame(0); let r = Rect { x: 10, y: 145, w: 96, h: 50 }; photo(&mut f, r, 1); e.set_hints(vec![r]);
        run(&mut e, &mut c, &p, &f);
        assert!(rows_equal(&c.px, &f, &[]));
    }
    #[test] fn reset_a_resends_everything() {
        let p = Params::new();
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        let f = text_frame(0); run(&mut e, &mut c, &p, &f);
        e.reset_a(); let mut c2 = Canvas::new(W, H);
        let o = run(&mut e, &mut c2, &p, &f);
        assert!(!o.ops.is_empty()); assert!(rows_equal(&c2.px, &f, &[]));
    }
    #[test] fn deterministic_ops() {
        let p = Params::new();
        let a = { let mut e = Encoder::new(W, H); e.push_frame(&text_frame(0), W * 4); e.encode(&p, 0).ops };
        let b = { let mut e = Encoder::new(W, H); e.push_frame(&text_frame(0), W * 4); e.encode(&p, 0).ops };
        assert_eq!(a, b);
    }
    #[test] fn blank_detection() {
        let mut e = Encoder::new(W, H); e.push_frame(&vec![0u8; W * H * 4], W * 4); assert!(e.is_blank());
        e.push_frame(&text_frame(0), W * 4); assert!(!e.is_blank());
    }

    // ───────── Scroll ─────────
    const SW: usize = 120; const SH: usize = 200; const HDR: usize = 20; const FTR: usize = 20;
    fn content_row(k: i64) -> Vec<u8> {
        let mut r = vec![255u8; SW * 4]; let mut s = (k as u64).wrapping_mul(0x9E3779B97F4A7C15) | 1;
        for _ in 0..14 { s ^= s << 13; s ^= s >> 7; s ^= s << 17; let x = (s % (SW as u64 - 30)) as usize; r[x * 4..x * 4 + 3].copy_from_slice(&[(s >> 20) as u8 % 90, 20, 20]) }
        r[0..3].copy_from_slice(&[(k.rem_euclid(200)) as u8, 1, 1]);   // 保证每行都不同
        r
    }
    fn scroll_frame(off: i64) -> Vec<u8> {
        let mut f = vec![255u8; SW * SH * 4];
        for y in 0..SH {
            let row = if y < HDR || y >= SH - FTR { let mut r = vec![200u8; SW * 4]; for px in r.chunks_exact_mut(4) { px[3] = 255 } r[(y % 7) * 4] = 10; r } else { content_row(off + (y - HDR) as i64) };
            f[y * SW * 4..(y + 1) * SW * 4].copy_from_slice(&row);
        }
        f
    }
    fn srect() -> Rect { Rect { x: 0, y: HDR as u32, w: SW as u32, h: (SH - HDR - FTR) as u32 } }
    /// A 合成后和 B 帧逐像素一致（滚动区右缘窄带除外：窄带用左邻列延伸）
    fn compose_equals(c: &mut Canvas, f: &[u8], sb: usize) -> bool {
        let vy = c.scroll.as_ref().map_or(0, |s| s.head); c.compose(vy);
        for y in 0..SH { for x in 0..SW {
            if c.scroll.is_some() && y >= HDR && y < SH - FTR && x >= SW - sb { continue }
            let o = (y * SW + x) * 4; if c.view[o..o + 3] != f[o..o + 3] { return false }
        } }
        true
    }
    fn srun(e: &mut Encoder, c: &mut Canvas, p: &Params, f: &[u8], t: u64) -> Out {
        e.push_frame(f, SW * 4); let o = e.encode(p, t);
        if let Some(rs) = &o.regions { c.set_regions(rs) }
        let ap = c.apply(&o.ops).unwrap(); assert!(ap.verify_fail.is_empty(), "校验失败 {:?}", ap.verify_fail); o
    }
    fn sbytes(o: &Out) -> usize { o.ops.iter().filter(|op| !matches!(op, Op::Head { .. } | Op::Known { .. })).map(|op| to_postcard(op).len()).sum() }
    fn sp() -> Params { let mut p = Params::new(); p.set("verify", 1.0); p.set("img_defer_ms", 0.0); p }

    #[test] fn scroll_sends_only_new_rows_and_back_is_free() {
        let p = sp(); let (mut e, mut c) = (Encoder::new(SW, SH), Canvas::new(SW, SH)); c.sb = 20;
        e.set_scroll_proposal(Some(srect()));
        let o0 = srun(&mut e, &mut c, &p, &scroll_frame(0), 0); assert!(compose_equals(&mut c, &scroll_frame(0), 20));
        let first = sbytes(&o0);
        let o1 = srun(&mut e, &mut c, &p, &scroll_frame(30), 10); assert!(compose_equals(&mut c, &scroll_frame(30), 20));
        assert!(sbytes(&o1) * 3 < first, "往下滚 30 行只该发新露出的 30 行：{} vs 首屏 {}", sbytes(&o1), first);
        assert_eq!(e.scroll_head().unwrap().0, ORIGIN as i64 + 30);
        let o2 = srun(&mut e, &mut c, &p, &scroll_frame(0), 20); assert!(compose_equals(&mut c, &scroll_frame(0), 20));
        assert_eq!(sbytes(&o2), 0, "滚回已见内容：零像素流量");
        let o3 = srun(&mut e, &mut c, &p, &scroll_frame(-25), 30); assert!(compose_equals(&mut c, &scroll_frame(-25), 20));
        assert!(sbytes(&o3) > 0 && sbytes(&o3) * 3 < first, "往上滚出没见过的 25 行：只发这 25 行");
    }
    #[test] fn scroll_region_created_after_fixed_frame_reuses_pixels_with_copy() {
        let p = sp(); let (mut e, mut c) = (Encoder::new(SW, SH), Canvas::new(SW, SH)); c.sb = 20;
        let f = scroll_frame(0); let o0 = srun(&mut e, &mut c, &p, &f, 0);
        e.set_scroll_proposal(Some(srect()));
        let o1 = srun(&mut e, &mut c, &p, &f, 10);
        assert!(o1.regions.is_some() && o1.ops.iter().any(|op| matches!(op, Op::Copy { .. })));
        assert!(sbytes(&o1) * 5 < sbytes(&o0), "已发出的像素搬进 Scroll 画布，不重发：{} vs {}", sbytes(&o1), sbytes(&o0));
        assert!(compose_equals(&mut c, &f, 20));
        let o2 = srun(&mut e, &mut c, &p, &scroll_frame(40), 20); assert!(compose_equals(&mut c, &scroll_frame(40), 20));
        assert!(sbytes(&o2) * 3 < sbytes(&o0));
    }
    #[test] fn unexplained_change_resets_scroll_canvas_like_a_page_change() {
        let p = sp(); let (mut e, mut c) = (Encoder::new(SW, SH), Canvas::new(SW, SH)); c.sb = 20;
        e.set_scroll_proposal(Some(srect()));
        srun(&mut e, &mut c, &p, &scroll_frame(0), 0);
        let o = srun(&mut e, &mut c, &p, &scroll_frame(5000), 10);   // 完全不相关的内容
        assert!(o.ops.iter().any(|op| matches!(op, Op::Clear { region: 1 })));
        assert!(compose_equals(&mut c, &scroll_frame(5000), 20));
        assert_eq!(e.scroll_head().unwrap().0, ORIGIN as i64);
    }
    #[test] fn scrollbar_band_changes_are_ignored_and_never_written() {
        let p = sp(); let (mut e, mut c) = (Encoder::new(SW, SH), Canvas::new(SW, SH)); c.sb = 20;
        e.set_scroll_proposal(Some(srect()));
        let mut f = scroll_frame(0); srun(&mut e, &mut c, &p, &f, 0);
        for y in 40..70 { let o = (y * SW + SW - 4) * 4; f[o..o + 3].copy_from_slice(&[90, 90, 90]) }   // 滚动条滑块
        let o = srun(&mut e, &mut c, &p, &f, 10);
        assert_eq!(sbytes(&o), 0, "窄带里的变化不发");
        // 滑块在滚动期间也不影响位移检测
        let mut f2 = scroll_frame(23); for y in 50..80 { let o = (y * SW + SW - 4) * 4; f2[o..o + 3].copy_from_slice(&[90, 90, 90]) }
        let o2 = srun(&mut e, &mut c, &p, &f2, 20); assert_eq!(e.scroll_head().unwrap().0, ORIGIN as i64 + 23);
        assert!(sbytes(&o2) < 3000);
        assert!(compose_equals(&mut c, &f2, 20));
    }
    #[test] fn expected_shift_helps_and_edge_is_recorded() {
        let p = sp(); let (mut e, mut c) = (Encoder::new(SW, SH), Canvas::new(SW, SH)); c.sb = 20;
        e.set_scroll_proposal(Some(srect())); srun(&mut e, &mut c, &p, &scroll_frame(0), 0);
        e.set_expect(Some(50)); srun(&mut e, &mut c, &p, &scroll_frame(49), 10);   // 预期 50，实际 49（精确公式的 ±1）
        assert_eq!(e.scroll_head().unwrap().0, ORIGIN as i64 + 49);
        e.set_expect(Some(100)); e.push_frame(&scroll_frame(49), SW * 4); e.encode(&p, 20);   // 滚不动：帧没变
        e.expect_timeout(); assert_eq!(e.scroll_edges(), (false, true));
    }
    #[test] fn scroll_works_when_fixed_header_and_footer_change_too() {
        let p = sp(); let (mut e, mut c) = (Encoder::new(SW, SH), Canvas::new(SW, SH)); c.sb = 20;
        e.set_scroll_proposal(Some(srect())); srun(&mut e, &mut c, &p, &scroll_frame(0), 0);
        let mut f = scroll_frame(17); for x in 10..60 { let o = (5 * SW + x) * 4; f[o..o + 3].copy_from_slice(&[0, 0, 200]) }   // 标题栏也变了
        let o = srun(&mut e, &mut c, &p, &f, 10);
        assert!(compose_equals(&mut c, &f, 20)); assert!(sbytes(&o) < 6000);
    }
}
