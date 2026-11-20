//! 虚拟 A（“实跑模式”，80 §5）：进程内的一个 A。它收到的就是线上那串 `Down`（和 WebTransport 写出去的字节一样），
//! 用 rd 解码器应用到自己的画布，所以整条编码 → 发送队列 → 解码链路都走了，只是不经网络；场景 runner 经调试口扮演 A 的输入。
//! JPEG 图片这里不解码（没有 JPEG 解码器）：有损像素在比较时一律排除。

use crate::core::{Conn, Core};
use rd::dec::{Canvas, LAYER_LIT};
use rd::hash::{row_hash_masked, Mask};
use rd::wire::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Default)]
pub struct VaState { pub canvas: Option<Canvas>, pub rx_bytes: u64, pub applied_seq: u64, pub txs: u64, pub verify_fail: u64, pub images: u64, pub err: String, pub last_vf: String }

pub fn attach(core: &Arc<Core>) {
    let id = core.conn_seq.fetch_add(1, Ordering::Relaxed) + 1;
    let (ctl_tx, mut ctl_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let (tx_tx, mut tx_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    *core.va.lock().unwrap() = Some(VaState::default());
    core.conn.lock().unwrap().replace(Conn { id, ctl: ctl_tx, tx: tx_tx, conn: None });
    core.reset_flow();
    std::thread::spawn(move || while ctl_rx.blocking_recv().is_some() {});
    let c = core.clone();
    std::thread::Builder::new().name("hj-va".into()).spawn(move || {
        while let Some(bytes) = tx_rx.blocking_recv() {
            let mut g = c.va.lock().unwrap();
            let Some(va) = g.as_mut() else { continue };
            va.rx_bytes += bytes.len() as u64;
            let d: Down = match from_postcard(&bytes[4..]) { Ok(d) => d, Err(e) => { va.err = e; continue } };
            let mut ack = None;
            match d {
                Down::Page { seq, regions, op: PageOp::Replace, .. } => {
                    if let Some(r) = regions.first() { let mut cv = Canvas::new(r.rect.w as usize, r.rect.h as usize); cv.sb = c.params.lock().unwrap().get("sb_w") as usize; cv.set_regions(&regions); va.canvas = Some(cv) }
                    va.applied_seq = seq; ack = Some(seq);
                }
                Down::Regions { seq, regions, .. } => {
                    if let Some(cv) = va.canvas.as_mut() { cv.set_regions(&regions) }
                    va.applied_seq = seq; ack = Some(seq);
                }
                Down::Tx { seq, ops, .. } => {
                    match va.canvas.as_mut().map(|cv| cv.apply(&ops)) {
                        Some(Ok(ap)) => { if !ap.verify_fail.is_empty() { va.last_vf = format!("{:?} ops={:?}", ap.verify_fail, ops.iter().map(|o| format!("{}:{:?}", o.kind(), o.dst().map(|d| (d.region, d.y, d.h)))).collect::<Vec<_>>()) } va.verify_fail += ap.verify_fail.len() as u64; va.images += ap.images.len() as u64; va.txs += 1 }
                        Some(Err(e)) => va.err = e,
                        None => va.err = "Tx 先于 Page".into(),
                    }
                    va.applied_seq = seq; ack = Some(seq);
                }
                _ => {}
            }
            drop(g);
            if let Some(s) = ack { c.on_ack(s) }
        }
    }).expect("va 线程");
    core.up_event(format!("{{\"ev\":\"connected\",\"conn\":{id},\"peer\":\"virtual-A\"}}"));
}

pub fn detach(core: &Arc<Core>) {
    *core.conn.lock().unwrap() = None; *core.va.lock().unwrap() = None;
    core.up_event("{\"ev\":\"disconnected\",\"conn\":0}".into());
}

/// 状态 + 与 B 当前帧的差异（排除有损像素）+ 每行 hash（有损像素置 0 后算，场景里比较两次画面是否一致用）
pub fn state(core: &Arc<Core>, rows: bool) -> serde_json::Value {
    let why = core.idle_why(); let idle = why.is_empty();
    let mut g = core.va.lock().unwrap();
    let Some(va) = g.as_mut() else { return serde_json::json!({"attached": false}) };
    let mut j = serde_json::json!({"attached": true, "idle": idle, "idle_why": why, "rx_bytes": va.rx_bytes, "txs": va.txs, "applied_seq": va.applied_seq,
        "tx_seq": core.tx_seq.load(Ordering::Relaxed), "verify_fail": va.verify_fail, "last_vf": va.last_vf, "images": va.images, "err": va.err});
    let (rx_bytes_unused, _) = (0, 0); let _ = rx_bytes_unused;
    if let Some(cv) = va.canvas.as_mut() {
        // Scroll 画布：A 的视口跟随采集头，合成后再和 B 帧比
        let head = cv.scroll.as_ref().map(|s| s.head);
        let scroll_rect = cv.scroll.as_ref().map(|s| s.rect);
        cv.compose(head.unwrap_or(0));   // 画布 + 视口处的滚动行 + Overlay：A 上实际看到的
        let (w, h) = (cv.w, cv.h); let rb = w * 4;
        let sbw = if scroll_rect.is_some() { cv.sb } else { 0 };
        let (px, layer): (&[u8], &[u8]) = (&cv.view, &cv.view_layer);
        let f = core.frame.lock().unwrap();
        let comparable = f.w == w && f.h == h;
        let (mut diff, mut lit) = (0u64, 0u64); let mut bb = [usize::MAX, usize::MAX, 0usize, 0usize];
        let mut hs: Vec<String> = Vec::new(); let full = Mask::full(w as u32);
        let first = &px[..4];
        let blank = px.chunks_exact(4).all(|p| p == first);
        for y in 0..h {
            let row = &px[y * rb..(y + 1) * rb]; let lay = &layer[y * w..(y + 1) * w];
            let in_sc = scroll_rect.map_or(false, |r| y >= r.y as usize && y < (r.y + r.h) as usize);
            let xend = if in_sc { w - sbw } else { w };   // 滚动区右缘窄带不比（用左邻列延伸，不是 B 的像素）
            let has_lit = lay.iter().any(|&l| l == LAYER_LIT);
            if has_lit { lit += lay.iter().filter(|&&l| l == LAYER_LIT).count() as u64 }
            if comparable {
                let fr = &f.buf[y * rb..(y + 1) * rb];
                if row != fr { for x in 0..xend { if lay[x] != LAYER_LIT && row[x * 4..x * 4 + 4] != fr[x * 4..x * 4 + 4] { diff += 1; bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x + 1), bb[3].max(y + 1)] } } }
            }
            if rows {
                if has_lit { let mut r = row.to_vec(); for x in 0..w { if lay[x] == LAYER_LIT { r[x * 4..x * 4 + 4].fill(0) } } hs.push(format!("{:x}", row_hash_masked(&r, &full))) }
                else { hs.push(format!("{:x}", row_hash_masked(row, &full))) }
            }
        }
        if let Some(sc) = cv.scroll.as_ref() { j["scroll"] = serde_json::json!({"head": sc.head, "y_min": sc.y_min, "y_max": sc.y_max, "at_top": sc.at_top, "at_bottom": sc.at_bottom}) }
        j["w"] = w.into(); j["h"] = h.into(); j["diff_px"] = diff.into(); j["diff_bbox"] = serde_json::json!(bb); j["lit_px"] = lit.into(); j["blank"] = blank.into(); j["comparable"] = comparable.into();
        if rows { j["rows"] = hs.into() }
    }
    j
}
