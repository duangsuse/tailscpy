//! 编码器 trait 与朴素基线（80 §7）：每帧整屏对上一帧按行 diff，合并成若干矩形，脏区像素 zstd。
//! M1 的 rd 编码器接同一个 trait。

/// 一帧编码的结果。
pub struct Encoded {
    /// 下行字节数
    pub bytes: u64,
    /// 按 Op 类型分：(名字, 字节, 个数)
    pub ops: Vec<(&'static str, u64, u32)>,
    /// 真实编码的线上字节；影子编码为 None（只统计、不编码）
    pub wire: Option<Vec<u8>>,
}

pub trait Encoder {
    fn name(&self) -> &'static str;
    /// 喂 B 的当前整屏 RGBA（w*h*4），返回这一帧要发的东西。
    fn encode(&mut self, frame: &[u8]) -> Encoded;
}

/// 脏矩形（像素坐标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect { pub x: usize, pub y: usize, pub w: usize, pub h: usize }

/// 行 hash 表 + 上一帧：求脏矩形。首帧整屏。
pub struct Diff {
    w: usize,
    h: usize,
    /// 相隔不超过这么多未变行的脏带合并成一个矩形
    gap: usize,
    prev: Vec<u8>,
    hash: Vec<u64>,
    started: bool,
}

impl Diff {
    pub fn new(w: usize, h: usize, gap: usize) -> Self {
        Diff { w, h, gap, prev: vec![0; w * h * 4], hash: vec![0; h], started: false }
    }

    pub fn rects(&mut self, cur: &[u8]) -> Vec<Rect> {
        let rb = self.w * 4;
        assert_eq!(cur.len(), rb * self.h, "帧大小与几何不符");
        let hs: Vec<u64> = (0..self.h).map(|y| rd::row_hash(&cur[y * rb..(y + 1) * rb])).collect();
        let first = !self.started;
        // 变化的行 → 连续行段 → 间隔 ≤ gap 的合并
        let mut bands: Vec<(usize, usize)> = Vec::new();
        let mut y = 0;
        while y < self.h {
            if !first && hs[y] == self.hash[y] { y += 1; continue; }
            let s = y;
            while y < self.h && (first || hs[y] != self.hash[y]) { y += 1; }
            match bands.last_mut() {
                Some(b) if s - b.1 <= self.gap => b.1 = y,
                _ => bands.push((s, y)),
            }
        }
        // 每条带再收紧列范围
        let out = bands.into_iter().map(|(y0, y1)| {
            let (mut x0, mut x1) = (self.w, 0);
            if first { (x0, x1) = (0, self.w); } else {
                for y in y0..y1 {
                    let (a, b) = (&cur[y * rb..(y + 1) * rb], &self.prev[y * rb..(y + 1) * rb]);
                    if a == b { continue; }
                    let l = (0..self.w).find(|&x| a[x * 4..x * 4 + 4] != b[x * 4..x * 4 + 4]).unwrap();
                    let r = (0..self.w).rev().find(|&x| a[x * 4..x * 4 + 4] != b[x * 4..x * 4 + 4]).unwrap();
                    x0 = x0.min(l);
                    x1 = x1.max(r + 1);
                }
                // 行 hash 变了但像素逐字节相同只可能是 hash 冲突之外的情况；保守起见退回整行
                if x1 <= x0 { (x0, x1) = (0, self.w); }
            }
            Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
        }).collect();
        self.prev.copy_from_slice(cur);
        self.hash = hs;
        self.started = true;
        out
    }
}

fn gather(cur: &[u8], stride: usize, r: Rect) -> Vec<u8> {
    let mut v = Vec::with_capacity(r.w * r.h * 4);
    for y in r.y..r.y + r.h { v.extend_from_slice(&cur[y * stride + r.x * 4..y * stride + (r.x + r.w) * 4]); }
    v
}

/// 朴素基线。线上格式：n u16 LE，然后 n 个 [x,y,w,h u16 LE, zlen u32 LE, zstd(矩形 RGBA)]。无变化时为空。
pub struct Naive { diff: Diff, w: usize, level: i32 }

impl Naive {
    pub fn new(w: usize, h: usize, level: i32, gap: usize) -> Self { Naive { diff: Diff::new(w, h, gap), w, level } }
}

impl Encoder for Naive {
    fn name(&self) -> &'static str { "naive-zstd" }
    fn encode(&mut self, frame: &[u8]) -> Encoded {
        let rects = self.diff.rects(frame);
        if rects.is_empty() { return Encoded { bytes: 0, ops: vec![], wire: Some(vec![]) }; }
        let mut wire = (rects.len() as u16).to_le_bytes().to_vec();
        for r in &rects {
            let z = zstd::bulk::compress(&gather(frame, self.w * 4, *r), self.level).expect("zstd 压缩");
            for v in [r.x, r.y, r.w, r.h] { wire.extend_from_slice(&(v as u16).to_le_bytes()); }
            wire.extend_from_slice(&(z.len() as u32).to_le_bytes());
            wire.extend_from_slice(&z);
        }
        Encoded { bytes: wire.len() as u64, ops: vec![("Dirty", wire.len() as u64, rects.len() as u32)], wire: Some(wire) }
    }
}

/// 影子编码占位：只算脏矩形，字节数按未压缩像素 + 头估计，不真正编码（M1 起换成“被省掉的矩形按字面量编一遍”）。
pub struct Shadow { diff: Diff }

impl Shadow {
    pub fn new(w: usize, h: usize, gap: usize) -> Self { Shadow { diff: Diff::new(w, h, gap) } }
}

impl Encoder for Shadow {
    fn name(&self) -> &'static str { "shadow-raw" }
    fn encode(&mut self, frame: &[u8]) -> Encoded {
        let rects = self.diff.rects(frame);
        let bytes: u64 = rects.iter().map(|r| (r.w * r.h * 4 + 12) as u64).sum();
        let ops = if rects.is_empty() { vec![] } else { vec![("Dirty", bytes, rects.len() as u32)] };
        Encoded { bytes, ops, wire: None }
    }
}

/// 朴素解码器：把一帧的线上字节应用到 A 画布。
pub fn decode(canvas: &mut [u8], w: usize, h: usize, wire: &[u8]) -> Result<(), String> {
    if wire.is_empty() { return Ok(()); }
    let bad = || "线上字节截断".to_string();
    let u16at = |o: usize| wire.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or_else(bad);
    let n = u16at(0)?;
    let mut p = 2;
    for _ in 0..n {
        let (x, y, rw, rh) = (u16at(p)?, u16at(p + 2)?, u16at(p + 4)?, u16at(p + 6)?);
        let zl = wire.get(p + 8..p + 12).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize).ok_or_else(bad)?;
        let z = wire.get(p + 12..p + 12 + zl).ok_or_else(bad)?;
        p += 12 + zl;
        if x + rw > w || y + rh > h { return Err("矩形越界".into()); }
        let px = zstd::bulk::decompress(z, rw * rh * 4).map_err(|e| format!("zstd 解压: {e}"))?;
        if px.len() != rw * rh * 4 { return Err("矩形像素数不符".into()); }
        for j in 0..rh { canvas[((y + j) * w + x) * 4..((y + j) * w + x + rw) * 4].copy_from_slice(&px[j * rw * 4..(j + 1) * rw * 4]); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{splitmix, synth, Replay};

    #[test]
    fn dirty_rects_merge_and_tighten() {
        let (w, h) = (16usize, 40usize);
        let mut d = Diff::new(w, h, 3);
        let mut f = vec![0u8; w * h * 4];
        assert_eq!(d.rects(&f), vec![Rect { x: 0, y: 0, w, h }]);   // 首帧整屏
        assert!(d.rects(&f).is_empty());
        f[(2 * w + 5) * 4] = 1;     // 行 2 列 5
        f[(4 * w + 9) * 4 + 2] = 1; // 行 4 列 9：与行 2 间隔 1 行 ≤ gap，合并
        f[(20 * w + 1) * 4] = 1;    // 行 20：另起一个
        assert_eq!(d.rects(&f), vec![Rect { x: 5, y: 2, w: 5, h: 3 }, Rect { x: 1, y: 20, w: 1, h: 1 }]);
    }

    #[test]
    fn naive_roundtrip_pixel_exact() {
        let tr = synth("enc", 96, 160);
        let mut rp = Replay::new(&tr);
        let mut enc = Naive::new(tr.w, tr.h, 3, 8);
        let mut a = vec![0u8; tr.w * tr.h * 4];
        let (mut total, mut n) = (0u64, 0);
        while rp.advance().unwrap().is_some() {
            let e = enc.encode(&rp.cur);
            total += e.bytes;
            decode(&mut a, tr.w, tr.h, e.wire.as_ref().unwrap()).unwrap();
            assert!(a == rp.cur, "第 {n} 帧 A 与 B 不一致");
            n += 1;
        }
        assert!(n >= 8 && total > 0);
    }

    #[test]
    fn random_noise_roundtrip() {
        let (w, h) = (33usize, 50usize);
        let mut s = 7u64;
        let mut enc = Naive::new(w, h, 1, 2);
        let mut a = vec![0u8; w * h * 4];
        let mut f = vec![0u8; w * h * 4];
        for _ in 0..20 {
            for _ in 0..30 { let r = splitmix(&mut s) as usize; let n = f.len(); f[r % n] = (r >> 40) as u8; }
            let e = enc.encode(&f);
            decode(&mut a, w, h, e.wire.as_ref().unwrap()).unwrap();
            assert!(a == f);
        }
    }

    #[test]
    fn shadow_has_no_wire() {
        let mut s = Shadow::new(4, 4, 1);
        let e = s.encode(&[0u8; 64]);
        assert!(e.wire.is_none() && e.bytes == 4 * 4 * 4 + 12);
    }
}
