//! A 端 wasm 的 C ABI（没有 wasm-bindgen，手写）。viewer 通过线性内存交换字节：
//!   `rd_alloc` 拿缓冲 → JS 写入 → 调函数 → 结果放在 `rd_out_ptr()` 开始的 `返回值` 字节里。
//! 画布（RGBA）和“层”平面都留在 wasm 内存里，JS 直接按 `rd_canvas_ptr()` 取视图（`Copy` 的源必须可读）。
//! 有损图片：wasm 不带 JPEG 解码，`rd_down_apply` 之后用 `rd_image_*` 取出 JPEG，JS 用 createImageBitmap 解完调 `rd_put_rgba` 写回。

use crate::dec::Canvas;
use crate::wire::*;
use std::cell::RefCell;

struct St { canvas: Option<Canvas>, out: Vec<u8>, images: Vec<(Dst, Vec<u8>)> }
thread_local! { static ST: RefCell<St> = RefCell::new(St { canvas: None, out: vec![], images: vec![] }) }

fn with<R>(f: impl FnOnce(&mut St) -> R) -> R { ST.with(|s| f(&mut s.borrow_mut())) }
fn set_out(s: &mut St, text: String) -> i32 { s.out = text.into_bytes(); s.out.len() as i32 }

#[no_mangle] pub extern "C" fn rd_alloc(n: usize) -> *mut u8 { let mut v = Vec::<u8>::with_capacity(n.max(1)); let p = v.as_mut_ptr(); std::mem::forget(v); p }
#[no_mangle] pub unsafe extern "C" fn rd_free(p: *mut u8, n: usize) { drop(Vec::from_raw_parts(p, 0, n.max(1))) }
#[no_mangle] pub extern "C" fn rd_ver() -> u32 { VER as u32 }
#[no_mangle] pub extern "C" fn rd_out_ptr() -> *const u8 { with(|s| s.out.as_ptr()) }

/// Up 的 JSON → 带 u32 LE 长度前缀的 postcard，放进 out。返回字节数；出错返回 -1，错误文本在 out。
#[no_mangle] pub unsafe extern "C" fn rd_up_encode(p: *const u8, n: usize) -> i32 {
    let s = std::str::from_utf8(std::slice::from_raw_parts(p, n)).unwrap_or("");
    with(|st| match from_json::<Up>(s) {
        Ok(m) => { st.out = frame(&m); st.out.len() as i32 }
        Err(e) => { set_out(st, e); -1 }
    })
}

#[no_mangle] pub extern "C" fn rd_canvas_init(w: u32, h: u32) { with(|s| { s.canvas = Some(Canvas::new(w as usize, h as usize)); s.images.clear() }) }
#[no_mangle] pub extern "C" fn rd_canvas_ptr() -> *const u8 { with(|s| s.canvas.as_ref().map_or(std::ptr::null(), |c| c.px.as_ptr())) }
#[no_mangle] pub extern "C" fn rd_layer_ptr() -> *const u8 { with(|s| s.canvas.as_ref().map_or(std::ptr::null(), |c| c.layer.as_ptr())) }

/// 一条 Down（postcard 消息体，不含长度前缀）：解码并应用，摘要 JSON 放进 out。出错返回 -1（out 里是原因）。
/// - `Tx`：Op 应用到画布；摘要里带每个写入矩形（种类、层）、待解码图片数、校验失败行段
/// - `Page{Replace}`：画布清空（B 同步重置了它对 A 的认识）
/// - 其它：原样转 JSON
#[no_mangle] pub unsafe extern "C" fn rd_down_apply(p: *const u8, n: usize) -> i32 {
    let body = std::slice::from_raw_parts(p, n);
    with(|st| {
        let m: Down = match from_postcard(body) { Ok(m) => m, Err(e) => return { set_out(st, format!("postcard: {e}")); -1 } };
        match m {
            Down::Tx { seq, page, rev, ops } => {
                let Some(cv) = st.canvas.as_mut() else { return { set_out(st, "画布未初始化".into()); -1 } };
                match cv.apply(&ops) {
                    Ok(ap) => {
                        let rects: Vec<serde_json::Value> = ap.rects.iter().map(|(d, k, l)| serde_json::json!({"k": k, "l": l, "r": d.region, "x": d.x, "y": d.y, "w": d.w, "h": d.h})).collect();
                        st.images = ap.images;
                        let imgs: Vec<serde_json::Value> = st.images.iter().enumerate().map(|(i, (d, _))| serde_json::json!({"i": i, "r": d.region, "x": d.x, "y": d.y, "w": d.w, "h": d.h})).collect();
                        let j = serde_json::json!({"Tx": {"seq": seq, "page": page, "rev": rev, "rects": rects, "images": imgs, "verify_fail": ap.verify_fail}});
                        set_out(st, j.to_string())
                    }
                    Err(e) => { set_out(st, e); -1 }
                }
            }
            Down::Page { op: PageOp::Replace, ref regions, .. } => {
                if let Some(c) = st.canvas.as_mut() { c.px.fill(0); c.layer.fill(0); c.scroll = None; c.set_regions(regions) }
                set_out(st, to_json(&m))
            }
            Down::Regions { ref regions, .. } => {
                if let Some(c) = st.canvas.as_mut() { c.set_regions(regions) }
                set_out(st, to_json(&m))
            }
            other => set_out(st, to_json(&other)),
        }
    })
}

#[no_mangle] pub extern "C" fn rd_image_ptr(i: usize) -> *const u8 { with(|s| s.images.get(i).map_or(std::ptr::null(), |x| x.1.as_ptr())) }
#[no_mangle] pub extern "C" fn rd_image_len(i: usize) -> usize { with(|s| s.images.get(i).map_or(0, |x| x.1.len())) }

/// JS 解出 JPEG 之后把像素写回（有损层）。region 0 = Fixed（屏幕坐标），1 = Scroll（画布 y）
#[no_mangle] pub unsafe extern "C" fn rd_put_rgba(region: u32, x: u32, y: u32, w: u32, h: u32, p: *const u8) -> i32 {
    let px = std::slice::from_raw_parts(p, (w * h * 4) as usize);
    with(|s| match s.canvas.as_mut() { Some(c) => c.put_rgba(&Dst { region: region as u16, x, y, w, h }, px).map(|_| 0).unwrap_or(-1), None => -1 })
}

/// 右缘滚动条窄带宽度（参数表 `sb_w`）
#[no_mangle] pub extern "C" fn rd_set_sb(n: u32) { with(|s| if let Some(c) = s.canvas.as_mut() { c.sb = n as usize }) }

/// 合成一帧屏幕（Fixed 画布 + 视口 vy 处的滚动行）。返回滚动区里缺内容的行数
#[no_mangle] pub extern "C" fn rd_compose(vy: i32) -> i32 { with(|s| s.canvas.as_mut().map_or(0, |c| c.compose(vy) as i32)) }
#[no_mangle] pub extern "C" fn rd_view_ptr() -> *const u8 { with(|s| s.canvas.as_ref().map_or(std::ptr::null(), |c| c.view.as_ptr())) }
#[no_mangle] pub extern "C" fn rd_view_layer_ptr() -> *const u8 { with(|s| s.canvas.as_ref().map_or(std::ptr::null(), |c| c.view_layer.as_ptr())) }

/// Scroll 区域信息（JSON 放进 out）：没有就返回 -1
#[no_mangle] pub extern "C" fn rd_scroll_info() -> i32 {
    with(|s| {
        let Some(sc) = s.canvas.as_ref().and_then(|c| c.scroll.as_ref()) else { return -1 };
        let j = serde_json::json!({"rect": [sc.rect.x, sc.rect.y, sc.rect.w, sc.rect.h], "head": sc.head, "y_min": if sc.y_min == i32::MAX { 0 } else { sc.y_min }, "y_max": if sc.y_max == i32::MIN { 0 } else { sc.y_max },
            "at_top": sc.at_top, "at_bottom": sc.at_bottom, "total_h": sc.total_h});
        set_out(s, j.to_string())
    })
}
#[no_mangle] pub extern "C" fn rd_out_len() -> usize { with(|s| s.out.len()) }
