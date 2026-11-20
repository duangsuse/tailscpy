//! B 端编码器（40 §1、§4、§6、§7）：**对“A 已有的内容”求差**，不是对上一帧。
//!
//! M1 的简化（90 §M1）：一个临时页、整屏一个 `Fixed` 区域；原地变化暂直接写画布（`Overlay` 在 M2 任务 20）。
//! A 的状态 = 每行 hash 表 + 一份影子画布（源像素）。下行是一条有序流，发出去的就算 A 已有，所以不用等确认。
//!
//! 流程：push_frame（只拷变化行）→ encode：重算脏行 hash → “A 缺的行”→ 连续缺行成横带 →
//! 逐列比对定左右界 → 按条带分类编码（Fill / Palette / Residual），A11y 图片矩形经像素确认后走 JPEG。
//! **不读墙钟、不做 I/O**：时间不进来，同一输入序列产出完全相同的 Op。

use crate::dec::{LAYER_BG, LAYER_LIT};
use crate::hash::{fold, row_hash_masked, Mask};
use crate::lit::encode as le;
use crate::lit::FxMap;
use crate::params::Params;
use crate::region::column_mask;
use crate::wire::*;

pub struct Encoder {
    pub w: usize,
    pub h: usize,
    cur: Vec<u8>,
    shadow: Vec<u8>,
    cur_hash: Vec<u64>,
    stale: Vec<bool>,
    a_hash: Vec<u64>,
    a_valid: Vec<bool>,
    layer: Vec<u8>,
    mask: Mask,
    mask_sb: u32,
    hints: Vec<Rect>,
}

pub struct Out { pub ops: Vec<Op>, pub rows: u32 }

fn rect_of(d: &Dst) -> Rect { Rect { x: d.x, y: d.y, w: d.w, h: d.h } }

impl Encoder {
    pub fn new(w: usize, h: usize) -> Encoder {
        Encoder {
            w, h, cur: vec![0; w * h * 4], shadow: vec![0; w * h * 4], cur_hash: vec![0; h], stale: vec![true; h],
            a_hash: vec![0; h], a_valid: vec![false; h], layer: vec![0; w * h], mask: Mask::full(w as u32), mask_sb: 0, hints: vec![],
        }
    }

    /// A 的缓存作废（新会话 / `Resync` / `Welcome{Reset}`）：之后整屏都是“缺”的。
    pub fn reset_a(&mut self) {
        self.a_valid.fill(false); self.layer.fill(0); self.shadow.fill(0);
    }

    /// A11y 给的图片矩形提案（屏幕像素）。只是提案：要过像素确认才会走 JPEG。
    pub fn set_hints(&mut self, h: Vec<Rect>) { self.hints = h }

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
        let rb = self.w * 4; let first = &self.cur[..4];
        self.cur.chunks_exact(4).all(|p| p == first) && rb > 0
    }

    fn sync_mask(&mut self, sb: u32) {
        if sb == self.mask_sb { return }
        self.mask = column_mask(self.w as u32, sb, &[]); self.mask_sb = sb;
        // hash 定义变了：A 已有的行按影子画布重算，不用重发
        let rb = self.w * 4;
        for y in 0..self.h { if self.a_valid[y] { self.a_hash[y] = row_hash_masked(&self.shadow[y * rb..(y + 1) * rb], &self.mask) } self.stale[y] = true }
    }

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

    fn lit_rect(&self, x: usize, y: usize, w: usize, h: usize, level: i32) -> Op {
        let rb = self.w * 4; let dst = Dst { region: 0, x: x as u32, y: y as u32, w: w as u32, h: h as u32 };
        match le::histogram(&self.cur, rb, x, y, w, h, 4096) {
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
        }
    }

    /// 编一帧：返回要发的 Op（没有缺的就是空）。同时把这些 Op 记入 A 的状态。
    pub fn encode(&mut self, p: &Params) -> Out {
        self.sync_mask(p.get("sb_w") as u32);
        let (w, h, rb) = (self.w, self.h, self.w * 4);
        let level = p.get("zstd_level") as i32; let strip = (p.get("strip_rows") as usize).max(1);
        for y in 0..h { if self.stale[y] { self.cur_hash[y] = row_hash_masked(&self.cur[y * rb..(y + 1) * rb], &self.mask); self.stale[y] = false } }
        let missing: Vec<bool> = (0..h).map(|y| !self.a_valid[y] || self.a_hash[y] != self.cur_hash[y]).collect();
        if !missing.iter().any(|&m| m) { return Out { ops: vec![], rows: 0 } }

        // 连续缺行 → 横带，逐列比对定左右界
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

        // 图片：A11y 提案 + 像素确认 + 确实有变化
        let mut images: Vec<Rect> = vec![];
        if p.get("img_lossy") > 0.5 {
            for hnt in self.hints.clone() {
                let r = Rect { x: hnt.x.min(w as u32), y: hnt.y.min(h as u32), w: hnt.w.min((w as u32).saturating_sub(hnt.x)), h: hnt.h.min((h as u32).saturating_sub(hnt.y)) };
                if r.w == 0 || r.h == 0 || r.w * r.h < p.get("img_min_area") as u32 { continue }
                if !bands.iter().any(|b| (b.0 as u32) < r.y + r.h && (b.1 as u32) > r.y) { continue }
                if !self.rect_differs(&r) || !self.is_photo(&r, p) { continue }
                if !images.iter().any(|i| i.x < r.x + r.w && r.x < i.x + i.w && i.y < r.y + r.h && r.y < i.y + i.h) { images.push(r) }
            }
        }

        let mut ops: Vec<Op> = vec![];
        for &(y0, y1, x0, x1) in &bands {
            let mut cuts = vec![y0, y1];
            for r in &images { for c in [r.y as usize, (r.y + r.h) as usize] { if c > y0 && c < y1 { cuts.push(c) } } }
            cuts.sort(); cuts.dedup();
            for sg in cuts.windows(2) {
                let (s0, s1) = (sg[0], sg[1]);
                let mut cover: Vec<(usize, usize)> = images.iter().filter(|r| (r.y as usize) <= s0 && ((r.y + r.h) as usize) >= s1).map(|r| (r.x as usize, (r.x + r.w) as usize)).collect();
                cover.sort();
                let mut xs = x0;
                let mut free: Vec<(usize, usize)> = vec![];
                for (a, b) in cover { if a > xs { free.push((xs, a.min(x1))) } xs = xs.max(b) }
                if xs < x1 { free.push((xs, x1)) }
                for (a, b) in free { if a >= b { continue }
                    let mut yy = s0; while yy < s1 { let hh = strip.min(s1 - yy); ops.push(self.lit_rect(a, yy, b - a, hh, level)); yy += hh }
                }
            }
        }
        for r in &images { ops.push(Op::Image { dst: Dst { region: 0, x: r.x, y: r.y, w: r.w, h: r.h }, sprite: None, q: p.get("img_q") as u8, mask: vec![], data: self.jpeg(r, p.get("img_q") as u8) }) }

        // 记入 A 的状态
        let mut rows = 0u32;
        for &(y0, y1, _, _) in &bands { for yy in y0..y1 {
            self.shadow[yy * rb..(yy + 1) * rb].copy_from_slice(&self.cur[yy * rb..(yy + 1) * rb]);
            self.a_hash[yy] = self.cur_hash[yy]; self.a_valid[yy] = true; rows += 1;
        } }
        for op in &ops { if let Some(d) = op.dst() { let l = if matches!(op, Op::Image { .. }) { LAYER_LIT } else { LAYER_BG };
            for r in 0..d.h as usize { let o = (d.y as usize + r) * w + d.x as usize; self.layer[o..o + d.w as usize].fill(l) } } }

        // 校验：只覆盖本帧写过、且不含有损像素的连续行
        if p.get("verify") > 0.5 {
            let full = Mask::full(w as u32); let mut touched = vec![false; h];
            for &(y0, y1, _, _) in &bands { for yy in y0..y1 { touched[yy] = true } }
            let clean = |yy: usize| touched[yy] && self.layer[yy * w..(yy + 1) * w].iter().all(|&l| l != LAYER_LIT);
            let mut yy = 0;
            while yy < h {
                if !clean(yy) { yy += 1; continue }
                let s = yy; while yy < h && clean(yy) { yy += 1 }
                let hs: Vec<u64> = (s..yy).map(|r| row_hash_masked(&self.shadow[r * rb..(r + 1) * rb], &full)).collect();
                ops.push(Op::Verify { region: 0, y0: s as u32, y1: yy as u32, fold: fold(&hs) });
            }
        }
        let _ = rect_of;
        Out { ops, rows }
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
        let out = enc.encode(p);
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
        let f3 = text_frame(3); let out = { e.push_frame(&f3, W * 4); e.encode(&p) };
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
        let a = { let mut e = Encoder::new(W, H); e.push_frame(&text_frame(0), W * 4); e.encode(&p).ops };
        let b = { let mut e = Encoder::new(W, H); e.push_frame(&text_frame(0), W * 4); e.encode(&p).ops };
        assert_eq!(a, b);
    }
    #[test] fn blank_detection() {
        let mut e = Encoder::new(W, H); e.push_frame(&vec![0u8; W * H * 4], W * 4); assert!(e.is_blank());
        e.push_frame(&text_frame(0), W * 4); assert!(!e.is_blank());
    }
    #[test] fn masked_columns_do_not_trigger_updates() {
        let mut p = Params::new(); p.set("sb_w", 8.0);
        let (mut e, mut c) = (Encoder::new(W, H), Canvas::new(W, H));
        let mut f = text_frame(0); run(&mut e, &mut c, &p, &f);
        for y in 10..30 { let o = (y * W + W - 3) * 4; f[o..o + 3].copy_from_slice(&[1, 2, 3]) }   // 右缘窄带内变化
        let o = run(&mut e, &mut c, &p, &f);
        assert!(o.ops.is_empty(), "窄带内的变化不进 hash（M2 起由多帧一致性写入）");
    }
}
