//! A 端解码：把 `Tx` 的 Op 应用到画布（RGBA）。wasm 和 bench 的校验共用。
//! 每像素记一个“最后写入的层”：0 空 / 1 bg（无损）/ 2 lit（有损图片）。r 遮罩的“图/文”着色与 `Verify` 都用它。

use crate::hash::{fold, row_hash_masked, Mask};
use crate::lit;
use crate::wire::*;

pub struct Canvas { pub w: usize, pub h: usize, pub px: Vec<u8>, pub layer: Vec<u8> }

/// 应用一批 Op 的结果
#[derive(Default)]
pub struct Applied {
    /// 每个写入矩形：(目标, Op 种类, 层)
    pub rects: Vec<(Dst, &'static str, u8)>,
    /// 待解码的有损图片（JPEG）：A 用 createImageBitmap 解，再 `put_rgba`
    pub images: Vec<(Dst, Vec<u8>)>,
    /// 校验失败的行段
    pub verify_fail: Vec<(u32, u32)>,
}

pub const LAYER_EMPTY: u8 = 0; pub const LAYER_BG: u8 = 1; pub const LAYER_LIT: u8 = 2;

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas { Canvas { w, h, px: vec![0; w * h * 4], layer: vec![0; w * h] } }
    pub fn row(&self, y: usize) -> &[u8] { &self.px[y * self.w * 4..(y + 1) * self.w * 4] }

    fn check(&self, d: &Dst) -> Result<(), String> {
        if d.region != 0 { return Err(format!("M1 只有区域 0，收到 {}", d.region)) }
        if d.w == 0 || d.h == 0 || (d.x + d.w) as usize > self.w || (d.y + d.h) as usize > self.h { return Err(format!("目标越界 {:?}", d)) }
        Ok(())
    }

    fn blit(&mut self, d: &Dst, rgba: &[u8], layer: u8) {
        let (x, y, w, h) = (d.x as usize, d.y as usize, d.w as usize, d.h as usize);
        for r in 0..h {
            let o = ((y + r) * self.w + x) * 4;
            self.px[o..o + w * 4].copy_from_slice(&rgba[r * w * 4..(r + 1) * w * 4]);
            let l = (y + r) * self.w + x; self.layer[l..l + w].fill(layer);
        }
    }

    /// JS 解出 JPEG 之后把像素写回画布（有损层）
    pub fn put_rgba(&mut self, d: &Dst, rgba: &[u8]) -> Result<(), String> {
        self.check(d)?;
        if rgba.len() < (d.w * d.h * 4) as usize { return Err("rgba 过短".into()) }
        self.blit(d, rgba, LAYER_LIT); Ok(())
    }

    pub fn apply(&mut self, ops: &[Op]) -> Result<Applied, String> {
        let mut ap = Applied::default();
        for op in ops { self.apply_one(op, &mut ap)? }
        Ok(ap)
    }

    fn apply_one(&mut self, op: &Op, ap: &mut Applied) -> Result<(), String> {
        match op {
            Op::Fill { dst, color } => {
                self.check(dst)?;
                let row: Vec<u8> = (0..dst.w).flat_map(|_| [color[0], color[1], color[2], 255]).collect();
                let full: Vec<u8> = (0..dst.h).flat_map(|_| row.iter().cloned()).collect();
                self.blit(dst, &full, LAYER_BG); ap.rects.push((*dst, "Fill", LAYER_BG));
            }
            Op::Lit { dst, enc, dict, data } => {
                self.check(dst)?;
                if dict.is_some() { return Err("M1 不支持 Lit.dict".into()) }
                let raw = lit::zstd_decode(data)?;
                let (w, h) = (dst.w as usize, dst.h as usize);
                let rgba = match enc { LitEnc::Palette => lit::palette_decode(&raw, w, h)?, LitEnc::Residual => lit::residual_decode(&raw, w, h)?, LitEnc::Raw => lit::raw_decode(&raw, w, h)? };
                self.blit(dst, &rgba, LAYER_BG); ap.rects.push((*dst, op.kind(), LAYER_BG));
            }
            Op::Image { dst, data, .. } => {
                self.check(dst)?;
                // 解码前先把层标成 lit（占位）；像素由 JS 解码后 put_rgba 写入
                for r in 0..dst.h as usize { let l = (dst.y as usize + r) * self.w + dst.x as usize; self.layer[l..l + dst.w as usize].fill(LAYER_LIT) }
                ap.images.push((*dst, data.clone())); ap.rects.push((*dst, "Image", LAYER_LIT));
            }
            Op::Verify { y0, y1, fold: want, .. } => {
                let mask = Mask::full(self.w as u32);
                let hs: Vec<u64> = (*y0 as usize..(*y1 as usize).min(self.h)).map(|y| row_hash_masked(self.row(y), &mask)).collect();
                if fold(&hs) != *want { ap.verify_fail.push((*y0, *y1)) }
            }
            // M1 不产生，也不支持：遇到就报错，让 A 走 Resync
            other => return Err(format!("M1 不支持 {}", other.kind())),
        }
        Ok(())
    }
}
