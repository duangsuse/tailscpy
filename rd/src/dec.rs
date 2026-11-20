//! A 端解码：把 `Tx` 的 Op 应用到画布（RGBA）。wasm 和 bench 的校验共用。
//!
//! 画布两种：
//! - 区域 0（`Fixed`，整屏）：按屏幕坐标寻址的 RGBA；
//! - 区域 1（`Scroll`）：按内容坐标（`wire::ORIGIN` 为起点）寻址的长截屏，**分块稀疏存储**（`TILE` 行一块，没写过的块不占内存）。
//!   视口位置由 A 决定（本地滚动）；`compose` 把 Fixed 画布和视口处的滚动行合成一帧屏幕。
//!
//! 每像素记一个“最后写入的层”：0 空 / 1 bg（无损）/ 2 lit（有损图片）。r 遮罩的“图/文”着色与 `Verify` 都用它。

use crate::hash::{fold, row_hash_masked, Mask};
use crate::lit;
use crate::wire::*;
use std::collections::HashMap;

pub const TILE: usize = 128;
pub const LAYER_EMPTY: u8 = 0; pub const LAYER_BG: u8 = 1; pub const LAYER_LIT: u8 = 2;

struct Tile { px: Vec<u8>, layer: Vec<u8>, have: Vec<bool> }

pub struct ScrollCanvas {
    pub rect: Rect,
    tiles: HashMap<i32, Tile>,
    /// B 的采集头：屏幕第一行对应的画布 y
    pub head: i32,
    /// 已写入内容的范围 [y_min, y_max)
    pub y_min: i32,
    pub y_max: i32,
    pub at_top: bool,
    pub at_bottom: bool,
    pub total_h: Option<u32>,
}

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

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
    pub layer: Vec<u8>,
    pub scroll: Option<ScrollCanvas>,
    /// 右缘滚动条窄带宽度（来自参数表 `sb_w`）：滚动行不含这一带，合成时用左邻列延伸
    pub sb: usize,
    pub view: Vec<u8>,
    /// 合成后每像素的层（r 遮罩的图 / 文着色）
    pub view_layer: Vec<u8>,
}

impl ScrollCanvas {
    fn new(rect: Rect) -> ScrollCanvas { ScrollCanvas { rect, tiles: HashMap::new(), head: ORIGIN as i32, y_min: i32::MAX, y_max: i32::MIN, at_top: false, at_bottom: false, total_h: None } }
    fn tile(&mut self, ti: i32, w: usize) -> &mut Tile { self.tiles.entry(ti).or_insert_with(|| Tile { px: vec![0; TILE * w * 4], layer: vec![0; TILE * w], have: vec![false; TILE] }) }
    fn clear(&mut self) { self.tiles.clear(); self.y_min = i32::MAX; self.y_max = i32::MIN }
    pub fn has_row(&self, y: i32) -> bool { self.tiles.get(&y.div_euclid(TILE as i32)).map_or(false, |t| t.have[y.rem_euclid(TILE as i32) as usize]) }
    /// 一行：(像素, 层)
    pub fn row(&self, y: i32, w: usize) -> Option<(&[u8], &[u8])> {
        let t = self.tiles.get(&y.div_euclid(TILE as i32))?; let r = y.rem_euclid(TILE as i32) as usize;
        if !t.have[r] { return None }
        Some((&t.px[r * w * 4..(r + 1) * w * 4], &t.layer[r * w..(r + 1) * w]))
    }
    /// 写一个矩形（rgba 是 w*h*4）
    fn blit(&mut self, cw: usize, d: &Dst, rgba: &[u8], layer: u8) {
        for r in 0..d.h as usize {
            let y = d.y as i32 + r as i32; let (ti, tr) = (y.div_euclid(TILE as i32), y.rem_euclid(TILE as i32) as usize);
            let t = self.tile(ti, cw); let (x, w) = (d.x as usize, d.w as usize);
            t.px[(tr * cw + x) * 4..(tr * cw + x + w) * 4].copy_from_slice(&rgba[r * w * 4..(r + 1) * w * 4]);
            t.layer[tr * cw + x..tr * cw + x + w].fill(layer);
            t.have[tr] = true;
        }
        self.y_min = self.y_min.min(d.y as i32); self.y_max = self.y_max.max((d.y + d.h) as i32);
    }
    fn set_layer(&mut self, cw: usize, d: &Dst, layer: u8) {
        for r in 0..d.h as usize {
            let y = d.y as i32 + r as i32; let (ti, tr) = (y.div_euclid(TILE as i32), y.rem_euclid(TILE as i32) as usize);
            let t = self.tile(ti, cw); t.layer[tr * cw + d.x as usize..tr * cw + (d.x + d.w) as usize].fill(layer); t.have[tr] = true;
        }
        self.y_min = self.y_min.min(d.y as i32); self.y_max = self.y_max.max((d.y + d.h) as i32);
    }
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas { Canvas { w, h, px: vec![0; w * h * 4], layer: vec![0; w * h], scroll: None, sb: 0, view: vec![0; w * h * 4], view_layer: vec![0; w * h] } }
    pub fn row(&self, y: usize) -> &[u8] { &self.px[y * self.w * 4..(y + 1) * self.w * 4] }

    /// 区域列表（`Page` / `Regions`）：有 Scroll 区域就建（矩形变了就重建），没有就撤。
    pub fn set_regions(&mut self, rs: &[Region]) {
        match rs.iter().find(|r| matches!(r.kind, RegionKind::Scroll)) {
            Some(r) => { if self.scroll.as_ref().map_or(true, |s| s.rect != r.rect) { self.scroll = Some(ScrollCanvas::new(r.rect)) } }
            None => self.scroll = None,
        }
    }

    fn check(&self, d: &Dst) -> Result<(), String> {
        if d.w == 0 || d.h == 0 { return Err(format!("空目标 {:?}", d)) }
        match d.region {
            0 => if (d.x + d.w) as usize > self.w || (d.y + d.h) as usize > self.h { return Err(format!("目标越界 {:?}", d)) },
            1 => { if self.scroll.is_none() { return Err("没有 Scroll 区域".into()) } if (d.x + d.w) as usize > self.w { return Err(format!("目标越界 {:?}", d)) } }
            r => return Err(format!("没有区域 {r}")),
        }
        Ok(())
    }

    fn blit(&mut self, d: &Dst, rgba: &[u8], layer: u8) {
        if d.region == 1 { let cw = self.w; self.scroll.as_mut().unwrap().blit(cw, d, rgba, layer); return }
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
                let full: Vec<u8> = (0..dst.w * dst.h).flat_map(|_| [color[0], color[1], color[2], 255]).collect();
                self.blit(dst, &full, LAYER_BG); ap.rects.push((*dst, "Fill", LAYER_BG));
            }
            Op::Lit { dst, enc, dict, data } => {
                self.check(dst)?;
                if dict.is_some() { return Err("M2 不支持 Lit.dict".into()) }
                let raw = lit::zstd_decode(data)?;
                let (w, h) = (dst.w as usize, dst.h as usize);
                let rgba = match enc { LitEnc::Palette => lit::palette_decode(&raw, w, h)?, LitEnc::Residual => lit::residual_decode(&raw, w, h)?, LitEnc::Raw => lit::raw_decode(&raw, w, h)? };
                self.blit(dst, &rgba, LAYER_BG); ap.rects.push((*dst, op.kind(), LAYER_BG));
            }
            Op::Image { dst, data, .. } => {
                self.check(dst)?;
                // 解码前先把层标成 lit（占位）；像素由 JS 解码后 put_rgba 写入
                if dst.region == 1 { let cw = self.w; self.scroll.as_mut().unwrap().set_layer(cw, dst, LAYER_LIT) }
                else { for r in 0..dst.h as usize { let l = (dst.y as usize + r) * self.w + dst.x as usize; self.layer[l..l + dst.w as usize].fill(LAYER_LIT) } }
                ap.images.push((*dst, data.clone())); ap.rects.push((*dst, "Image", LAYER_LIT));
            }
            Op::Copy { src: Src::Canvas { region: 0, x, y, .. }, dst } if dst.region == 1 => {
                // 同页搬移：Fixed 画布（屏幕坐标）→ Scroll 画布（内容坐标）。B 在“Scroll 区域新建、已发过的像素搬进去”时用
                self.check(dst)?;
                if (*x + dst.w) as usize > self.w || (*y + dst.h) as usize > self.h { return Err("Copy 源越界".into()) }
                let mut rgba = Vec::with_capacity((dst.w * dst.h * 4) as usize); let mut lay = Vec::new();
                for r in 0..dst.h as usize { let o = ((*y as usize + r) * self.w + *x as usize) * 4; rgba.extend_from_slice(&self.px[o..o + dst.w as usize * 4]);
                    let l = (*y as usize + r) * self.w + *x as usize; lay.extend_from_slice(&self.layer[l..l + dst.w as usize]) }
                self.blit(dst, &rgba, LAYER_BG);
                // 有损像素的层标记跟着搬
                let cw = self.w; let sc = self.scroll.as_mut().unwrap();
                for r in 0..dst.h as usize { for c in 0..dst.w as usize { if lay[r * dst.w as usize + c] == LAYER_LIT {
                    sc.set_layer(cw, &Dst { region: 1, x: dst.x + c as u32, y: dst.y + r as u32, w: 1, h: 1 }, LAYER_LIT) } } }
                ap.rects.push((*dst, "Copy", LAYER_BG));
            }
            Op::Clear { region } => match region {
                1 => { if let Some(s) = self.scroll.as_mut() { s.clear() } }
                _ => { self.px.fill(0); self.layer.fill(0) }
            },
            Op::Head { region: 1, y } => { if let Some(s) = self.scroll.as_mut() { s.head = *y } }
            Op::Known { region: 1, at_top, at_bottom, total_h, .. } => { if let Some(s) = self.scroll.as_mut() { s.at_top = *at_top; s.at_bottom = *at_bottom; s.total_h = *total_h } }
            Op::Verify { region, y0, y1, fold: want } => {
                let hs: Vec<u64> = if *region == 1 {
                    let Some(sc) = self.scroll.as_ref() else { return Err("没有 Scroll 区域".into()) };
                    let mask = Mask::excluding(self.w as u32, &[((self.w - self.sb) as u32, self.w as u32)]);
                    (*y0..*y1).map(|y| sc.row(y as i32, self.w).map_or(0, |(r, _)| row_hash_masked(r, &mask))).collect()
                } else {
                    let mask = Mask::full(self.w as u32);
                    (*y0 as usize..(*y1 as usize).min(self.h)).map(|y| row_hash_masked(self.row(y), &mask)).collect()
                };
                if fold(&hs) != *want { ap.verify_fail.push((*y0, *y1)) }
            }
            // 其余 M2 不产生，也不支持：遇到就报错，让 A 走 Resync
            other => return Err(format!("M2 不支持 {}", other.kind())),
        }
        Ok(())
    }

    /// 把 Fixed 画布和视口 `vy`（画布 y，滚动区第一行）处的滚动行合成一帧屏幕，放进 `view`。
    /// 滚动区里没有内容的行用占位色（取上一行已有内容的首像素，没有就深灰）；右缘窄带（`sb`）用左邻列延伸。返回缺行数。
    pub fn compose(&mut self, vy: i32) -> usize {
        self.view.copy_from_slice(&self.px); self.view_layer.copy_from_slice(&self.layer);
        let Some(sc) = self.scroll.as_ref() else { return 0 };
        let (w, rb) = (self.w, self.w * 4); let r = sc.rect;
        let mut missing = 0; let mut ph = [0x30u8, 0x30, 0x30, 255];
        for i in 0..r.h as usize {
            let y = r.y as usize + i; let dst = &mut self.view[y * rb..(y + 1) * rb];
            match sc.row(vy + i as i32, w) {
                Some((px, lay)) => {
                    dst.copy_from_slice(px); self.view_layer[y * w..(y + 1) * w].copy_from_slice(lay);
                    let sb = self.sb.min(w.saturating_sub(1));
                    if sb > 0 { let src: [u8; 4] = px[(w - sb - 1) * 4..(w - sb) * 4].try_into().unwrap(); for x in w - sb..w { dst[x * 4..x * 4 + 4].copy_from_slice(&src) } }
                    ph = [px[0], px[1], px[2], 255];
                }
                None => { missing += 1; self.view_layer[y * w..(y + 1) * w].fill(0); for x in 0..w { dst[x * 4..x * 4 + 4].copy_from_slice(&ph) } }
            }
        }
        missing
    }
}
