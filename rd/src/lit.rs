//! `Lit` 的编码与解码（40 §6）：Palette（≤256 色 + 转义，亮度序索引平面）与 Residual（通道去相关 + Paeth），都过 zstd。
//! 编码只在 B 端（C 库 zstd），解码两端都有（纯 Rust ruzstd，wasm 可用）。格式见 `wire::LitEnc`。

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default)] pub struct Fx(u64);
impl Hasher for Fx {
    fn finish(&self) -> u64 { self.0 }
    fn write(&mut self, b: &[u8]) { for &x in b { self.0 = (self.0.rotate_left(5) ^ x as u64).wrapping_mul(0x517c_c1b7_2722_0a95) } }
    fn write_u32(&mut self, v: u32) { self.0 = (self.0.rotate_left(5) ^ v as u64).wrapping_mul(0x517c_c1b7_2722_0a95) }
}
pub type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;

pub fn zstd_decode(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut d = ruzstd::decoding::StreamingDecoder::new(data).map_err(|e| format!("zstd 头: {e}"))?;
    let mut out = Vec::new();
    d.read_to_end(&mut out).map_err(|e| format!("zstd 解码: {e}"))?;
    Ok(out)
}

#[inline] fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (a as i32, b as i32, c as i32);
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c }
}

/// 解 Palette：raw 是 zstd 解出的字节。返回 w*h*4 的 RGBA（A=255）。
pub fn palette_decode(raw: &[u8], w: usize, h: usize) -> Result<Vec<u8>, String> {
    if raw.len() < 3 { return Err("palette 过短".into()) }
    let n = u16::from_le_bytes([raw[0], raw[1]]) as usize; let esc = raw[2] != 0;
    let pal_end = 3 + 3 * n; let idx_end = pal_end + w * h;
    if n == 0 || n > 256 || raw.len() < idx_end { return Err("palette 头不合法".into()) }
    let pal = &raw[3..pal_end]; let idx = &raw[pal_end..idx_end]; let mut escp = &raw[idx_end..];
    let mut out = vec![255u8; w * h * 4];
    for (i, &k) in idx.iter().enumerate() {
        let rgb: &[u8] = if esc && k == 255 {
            if escp.len() < 3 { return Err("转义像素不足".into()) }
            let (a, b) = escp.split_at(3); escp = b; a
        } else {
            let k = k as usize; if k >= n { return Err("索引越界".into()) }
            &pal[k * 3..k * 3 + 3]
        };
        out[i * 4..i * 4 + 3].copy_from_slice(rgb);
    }
    Ok(out)
}

/// 解 Residual：三个平面依次是 G、R−G、B−G 的 Paeth 残差。
pub fn residual_decode(raw: &[u8], w: usize, h: usize) -> Result<Vec<u8>, String> {
    let np = w * h;
    if raw.len() < 3 * np { return Err("residual 过短".into()) }
    let mut planes = [vec![0u8; np], vec![0u8; np], vec![0u8; np]];
    for (c, pl) in planes.iter_mut().enumerate() {
        let r = &raw[c * np..(c + 1) * np];
        for y in 0..h { for x in 0..w {
            let i = y * w + x;
            let a = if x > 0 { pl[i - 1] } else { 0 };
            let b = if y > 0 { pl[i - w] } else { 0 };
            let cc = if x > 0 && y > 0 { pl[i - w - 1] } else { 0 };
            pl[i] = r[i].wrapping_add(paeth(a, b, cc));
        } }
    }
    let mut out = vec![255u8; np * 4];
    for i in 0..np {
        let g = planes[0][i]; out[i * 4] = planes[1][i].wrapping_add(g); out[i * 4 + 1] = g; out[i * 4 + 2] = planes[2][i].wrapping_add(g);
    }
    Ok(out)
}

pub fn raw_decode(raw: &[u8], w: usize, h: usize) -> Result<Vec<u8>, String> {
    if raw.len() < w * h * 4 { return Err("raw 过短".into()) }
    Ok(raw[..w * h * 4].to_vec())
}

// ───────── 编码（B 端）─────────
#[cfg(not(target_arch = "wasm32"))]
pub mod encode {
    use super::*;

    pub fn zstd_encode(raw: &[u8], level: i32) -> Vec<u8> { zstd::bulk::compress(raw, level).expect("zstd 压缩") }

    #[inline] fn px(src: &[u8], stride: usize, x: usize, y: usize) -> u32 {
        let o = y * stride + x * 4; (src[o] as u32) << 16 | (src[o + 1] as u32) << 8 | src[o + 2] as u32
    }
    #[inline] fn luma(c: u32) -> u32 { ((c >> 16) & 255) * 77 + ((c >> 8) & 255) * 150 + (c & 255) * 29 }

    /// 矩形里的颜色直方：颜色数超过 `cap` 就放弃（None）。
    pub fn histogram(src: &[u8], stride: usize, x: usize, y: usize, w: usize, h: usize, cap: usize) -> Option<FxMap<u32, u32>> {
        let mut m: FxMap<u32, u32> = FxMap::default();
        let mut last = u32::MAX; let mut run = 0u32;
        for yy in y..y + h { for xx in x..x + w {
            let c = px(src, stride, xx, yy);
            if c == last { run += 1; continue }
            if run > 0 { *m.entry(last).or_insert(0) += run }
            last = c; run = 1;
            if m.len() > cap { return None }
        } }
        if run > 0 { *m.entry(last).or_insert(0) += run }
        if m.len() > cap { None } else { Some(m) }
    }

    /// Palette 原始字节（未压缩）。颜色数 ≤256 无转义；否则取前 255 个高频色，其余像素走转义，转义占比超过 `max_esc` 返回 None。
    pub fn palette_raw(src: &[u8], stride: usize, x: usize, y: usize, w: usize, h: usize, hist: &FxMap<u32, u32>, max_esc: f64) -> Option<Vec<u8>> {
        let mut cols: Vec<(u32, u32)> = hist.iter().map(|(&c, &n)| (c, n)).collect();
        let esc = cols.len() > 256;
        if esc {
            cols.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let rest: u64 = cols[255..].iter().map(|c| c.1 as u64).sum();
            if rest as f64 > max_esc * (w * h) as f64 { return None }
            cols.truncate(255);
        }
        cols.sort_by_key(|c| (luma(c.0), c.0));
        let n = cols.len();
        let mut idx: FxMap<u32, u8> = FxMap::default();
        for (i, c) in cols.iter().enumerate() { idx.insert(c.0, i as u8); }
        let mut out = Vec::with_capacity(3 + 3 * n + w * h);
        out.extend_from_slice(&(n as u16).to_le_bytes()); out.push(esc as u8);
        for c in &cols { out.extend_from_slice(&[(c.0 >> 16) as u8, (c.0 >> 8) as u8, c.0 as u8]) }
        let mut escs: Vec<u8> = Vec::new();
        let (mut lc, mut lk) = (u32::MAX, 0u8);
        for yy in y..y + h { for xx in x..x + w {
            let c = px(src, stride, xx, yy);
            let k = if c == lc { lk } else {
                let k = match idx.get(&c) { Some(&k) => k, None => 255 };
                lc = c; lk = k; k
            };
            if esc && k == 255 && !idx.contains_key(&c) { escs.extend_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]) }
            out.push(k);
        } }
        out.extend_from_slice(&escs);
        Some(out)
    }

    pub fn residual_raw(src: &[u8], stride: usize, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let np = w * h;
        let mut planes = [vec![0u8; np], vec![0u8; np], vec![0u8; np]];
        for yy in 0..h { for xx in 0..w {
            let o = (y + yy) * stride + (x + xx) * 4; let i = yy * w + xx;
            let (r, g, b) = (src[o], src[o + 1], src[o + 2]);
            planes[0][i] = g; planes[1][i] = r.wrapping_sub(g); planes[2][i] = b.wrapping_sub(g);
        } }
        let mut out = vec![0u8; 3 * np];
        for (c, pl) in planes.iter().enumerate() {
            for yy in 0..h { for xx in 0..w {
                let i = yy * w + xx;
                let a = if xx > 0 { pl[i - 1] } else { 0 };
                let b = if yy > 0 { pl[i - w] } else { 0 };
                let cc = if xx > 0 && yy > 0 { pl[i - w - 1] } else { 0 };
                out[c * np + i] = pl[i].wrapping_sub(paeth(a, b, cc));
            } }
        }
        out
    }

    pub fn raw_raw(src: &[u8], stride: usize, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 4);
        for yy in y..y + h { let o = yy * stride + x * 4; out.extend_from_slice(&src[o..o + w * 4]) }
        out
    }
}
