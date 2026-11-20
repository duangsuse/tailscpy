//! 语料录制目录（80 §3，host 的 Recorder.kt 写出）的读取与帧重放，以及合成 trace 生成器
//! （`--sample` 与单元测试用）。时间一律取自录制的时间戳，不读墙钟。

use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;

/// frames.bin 的一条记录（载荷保持 zlib 压缩，重放时才解，省内存）。
pub struct Rec {
    pub seq: u32,
    pub t: u32,
    /// 0 整帧 RGBA；1 增量（若干 [y0 u16 BE, n u16 BE, n 行 RGBA]）
    pub kind: u8,
    pub raw_len: usize,
    pub z: Vec<u8>,
}

/// events.jsonl 的一行（只留 bench 用得到的字段）。
pub struct Event {
    pub t: u32,
    pub ty: String,
    /// step 的动作（act，兼容 ev）：tap/back/swipe/open...
    pub act: String,
    /// mark 的名字
    pub name: String,
    /// a11y_tree 的文件（相对录制目录）
    pub file: String,
}

pub struct Trace {
    pub name: String,
    /// 录制目录；合成 trace 为 None
    pub dir: Option<std::path::PathBuf>,
    pub manifest: Value,
    pub w: usize,
    pub h: usize,
    pub events: Vec<Event>,
    pub recs: Vec<Rec>,
}

fn rd_u32(b: &[u8], o: usize) -> u32 { u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) }

impl Trace {
    pub fn load(dir: &Path) -> Result<Trace, String> {
        let e = |what: &str, x: &dyn std::fmt::Display| format!("{}: {what}: {x}", dir.display());
        let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).map_err(|x| e("读 manifest.json", &x))?)
            .map_err(|x| e("解析 manifest.json", &x))?;
        let dim = |k: &str| manifest[k].as_u64().filter(|&v| v > 0).map(|v| v as usize).ok_or_else(|| e("manifest 缺字段", &k));
        let (w, h) = (dim("w")?, dim("h")?);
        let mut events = Vec::new();
        let text = std::fs::read_to_string(dir.join("events.jsonl")).map_err(|x| e("读 events.jsonl", &x))?;
        for (i, l) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(l).map_err(|x| e(&format!("events.jsonl 第 {} 行", i + 1), &x))?;
            let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
            let act = if v["act"].is_string() { s("act") } else { s("ev") };
            events.push(Event { t: v["t"].as_u64().unwrap_or(0) as u32, ty: s("type"), act, name: s("name"), file: s("file") });
        }
        let idx = std::fs::read(dir.join("frames.idx")).map_err(|x| e("读 frames.idx", &x))?;
        let bin = std::fs::read(dir.join("frames.bin")).map_err(|x| e("读 frames.bin", &x))?;
        let mut recs = Vec::new();
        for c in idx.as_chunks::<16>().0 {
            let (seq, t) = (rd_u32(&c[..], 0), rd_u32(&c[..], 4));
            let off = u64::from_le_bytes(c[8..16].try_into().unwrap()) as usize;
            if off + 13 > bin.len() { return Err(e("frames.idx 偏移越界", &off)); }
            let (kind, raw_len, zl) = (bin[off + 4], rd_u32(&bin, off + 5) as usize, rd_u32(&bin, off + 9) as usize);
            if rd_u32(&bin, off) != seq || off + 13 + zl > bin.len() { return Err(e("frames.bin 记录与索引不符 seq", &seq)); }
            recs.push(Rec { seq, t, kind, raw_len, z: bin[off + 13..off + 13 + zl].to_vec() });
        }
        let name = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "trace".into());
        Ok(Trace { name, dir: Some(dir.to_path_buf()), manifest, w, h, events, recs })
    }

    /// 录制结束时刻（ms）：最后一个事件 / 帧的时间戳。
    pub fn end_ms(&self) -> u32 {
        self.events.iter().map(|e| e.t).chain(self.recs.iter().map(|r| r.t)).max().unwrap_or(0)
    }

    /// manifest 里声明的忽略行带（可选字段 `ignore: [[y0, y1], ...]`，闭区间）。
    pub fn manifest_ignore(&self) -> Vec<(usize, usize)> {
        let a = self.manifest["ignore"].as_array().cloned().unwrap_or_default();
        a.iter().filter_map(|p| Some((p[0].as_u64()? as usize, p[1].as_u64()? as usize))).collect()
    }

    /// 读 a11y_tree 事件指向的节点树，返回节点数（解析失败 / 缺文件为 None）。
    pub fn tree_nodes(&self, file: &str) -> Option<usize> {
        fn count(v: &Value) -> usize { 1 + v["kids"].as_array().map_or(0, |k| k.iter().map(count).sum()) }
        let v: Value = serde_json::from_slice(&std::fs::read(self.dir.as_ref()?.join(file)).ok()?).ok()?;
        Some(v["windows"].as_array()?.iter().filter(|w| w["root"].is_object()).map(|w| count(&w["root"])).sum())
    }
}

/// 帧重放：按录制顺序逐条应用，`cur` 始终是 B 当前的整屏 RGBA。
pub struct Replay<'a> {
    tr: &'a Trace,
    pub cur: Vec<u8>,
    next: usize,
}

impl<'a> Replay<'a> {
    pub fn new(tr: &'a Trace) -> Self { Replay { tr, cur: vec![0; tr.w * tr.h * 4], next: 0 } }

    /// 下一帧的时间戳（ms）。
    pub fn peek_t(&self) -> Option<u32> { self.tr.recs.get(self.next).map(|r| r.t) }

    /// 应用下一条记录，返回 (seq, t)。
    pub fn advance(&mut self) -> Result<Option<(u32, u32)>, String> {
        let Some(r) = self.tr.recs.get(self.next) else { return Ok(None) };
        self.next += 1;
        let mut raw = Vec::with_capacity(r.raw_len);
        ZlibDecoder::new(&r.z[..]).read_to_end(&mut raw).map_err(|x| format!("seq {} 解压失败: {x}", r.seq))?;
        if raw.len() != r.raw_len { return Err(format!("seq {} 长度不符", r.seq)); }
        let rb = self.tr.w * 4;
        if r.kind == 0 {
            if raw.len() != self.cur.len() { return Err(format!("seq {} 整帧大小不符", r.seq)); }
            self.cur.copy_from_slice(&raw);
        } else {
            let mut p = 0;
            while p < raw.len() {
                if p + 4 > raw.len() { return Err(format!("seq {} 增量截断", r.seq)); }
                let y0 = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;   // 增量头是大端
                let n = u16::from_be_bytes([raw[p + 2], raw[p + 3]]) as usize;
                p += 4;
                if p + n * rb > raw.len() || (y0 + n) * rb > self.cur.len() { return Err(format!("seq {} 增量越界", r.seq)); }
                self.cur[y0 * rb..(y0 + n) * rb].copy_from_slice(&raw[p..p + n * rb]);
                p += n * rb;
            }
        }
        Ok(Some((r.seq, r.t)))
    }
}

// ---------------------------------------------------------------- 合成 trace

/// 像 Recorder 一样把 cur 相对 prev 编成一条记录（行 diff，关键帧整帧）。无变化返回 None。
pub fn make_rec(prev: Option<&[u8]>, cur: &[u8], w: usize, seq: u32, t: u32, key: bool) -> Option<Rec> {
    let rb = w * 4;
    let (kind, raw) = match prev {
        Some(p) if !key => {
            let mut out = Vec::new();
            let h = cur.len() / rb;
            let mut y = 0;
            while y < h {
                if cur[y * rb..(y + 1) * rb] == p[y * rb..(y + 1) * rb] { y += 1; continue; }
                let mut e = y;
                while e < h && cur[e * rb..(e + 1) * rb] != p[e * rb..(e + 1) * rb] { e += 1; }
                out.extend_from_slice(&(y as u16).to_be_bytes());
                out.extend_from_slice(&((e - y) as u16).to_be_bytes());
                out.extend_from_slice(&cur[y * rb..e * rb]);
                y = e;
            }
            if out.is_empty() { return None; }
            (1, out)
        }
        _ => (0, cur.to_vec()),
    };
    let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
    z.write_all(&raw).unwrap();
    Some(Rec { seq, t, kind, raw_len: raw.len(), z: z.finish().unwrap() })
}

/// SplitMix64：合成内容用的确定性随机数。
pub fn splitmix(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// 画一页“文字页”：白底，每 24 px 一行，行内随机长度的深灰块（带抗锯齿灰边）。scroll 为竖直滚动像素。
fn render(page: u64, scroll: usize, w: usize, h: usize, buf: &mut [u8]) {
    for y in 0..h {
        let ly = y + scroll;
        let (line, r) = (ly / 24, ly % 24);
        let row = &mut buf[y * w * 4..(y + 1) * w * 4];
        row.fill(255);
        if !(6..18).contains(&r) { continue; }
        let mut s = page.wrapping_mul(0x1234567).wrapping_add(line as u64);
        let mut x = 8 + (splitmix(&mut s) % 16) as usize;
        while x + 12 < w {
            let len = 4 + (splitmix(&mut s) % 40) as usize;
            for i in x..(x + len).min(w - 8) {
                let g = if i == x || i + 1 == (x + len).min(w - 8) { 150 } else { 40 };
                row[i * 4..i * 4 + 3].fill(g);
            }
            x += len + 4 + (splitmix(&mut s) % 10) as usize;
        }
    }
}

/// 合成一条典型会话（open → 点进子页 → 返回 → 上滑 → 再点击），格式与真实录制一致。
pub fn synth(name: &str, w: usize, h: usize) -> Trace {
    let mut events = Vec::new();
    let mut recs: Vec<Rec> = Vec::new();
    let ev = |t: u32, ty: &str, act: &str, name: &str| Event { t, ty: ty.into(), act: act.into(), name: name.into(), file: String::new() };
    let mut cur = vec![255u8; w * h * 4];
    let mut prev: Option<Vec<u8>> = None;
    let mut seq = 0u32;
    let mut emit = |t: u32, cur: &[u8], prev: &mut Option<Vec<u8>>, recs: &mut Vec<Rec>, events: &mut Vec<Event>| {
        if let Some(r) = make_rec(prev.as_deref(), cur, w, seq, t, prev.is_none()) {
            recs.push(r);
            events.push(ev(t, "frame", "", ""));
            seq += 1;
        }
        *prev = Some(cur.to_vec());
    };
    events.push(ev(0, "mark", "", "rec_start"));
    events.push(ev(5, "step", "open", ""));
    render(0, 0, w, h, &mut cur);
    emit(60, &cur, &mut prev, &mut recs, &mut events);
    // 点按：按下涟漪（一个灰块）→ 新页面
    events.push(ev(1000, "step", "tap", ""));
    let band = (h / 4, h / 4 + h / 10);
    for (i, g) in [235u8, 245].into_iter().enumerate() {
        for y in band.0..band.1 { for px in cur[y * w * 4..(y + 1) * w * 4].chunks_exact_mut(4) { px[..3].fill(g); } }   // alpha 保持 255（真机采集帧 alpha 恒 255）
        emit(1030 + 33 * i as u32, &cur, &mut prev, &mut recs, &mut events);
    }
    render(1, 0, w, h, &mut cur);
    emit(1130, &cur, &mut prev, &mut recs, &mut events);
    // 返回：回到第 0 页
    events.push(ev(3000, "step", "back", ""));
    render(0, 0, w, h, &mut cur);
    emit(3060, &cur, &mut prev, &mut recs, &mut events);
    // 上滑：每帧 40 px，共 10 帧
    events.push(ev(5000, "step", "swipe", ""));
    events.push(ev(5200, "mark", "", "scroll-mid"));
    for i in 1..=10u32 {
        render(0, 40 * i as usize, w, h, &mut cur);
        emit(5000 + 33 * i, &cur, &mut prev, &mut recs, &mut events);
    }
    // 再点一次（回到同一子页）
    events.push(ev(7000, "step", "tap", ""));
    render(1, 0, w, h, &mut cur);
    emit(7090, &cur, &mut prev, &mut recs, &mut events);
    events.push(ev(8000, "mark", "", "end"));
    events.push(ev(8500, "mark", "", "rec_stop"));
    events.sort_by_key(|e| e.t);
    let manifest = serde_json::json!({"schema": 1, "w": w, "h": h, "dpi": 420, "pkg": "synthetic", "scene": name});
    Trace { name: name.into(), dir: None, manifest, w, h, events, recs }
}

/// `--sample` 用的合成 trace。
pub fn sample() -> Vec<Trace> { vec![synth("sample", 360, 800)] }

/// 把 trace 按录制格式写成目录（单元测试里做往返）。
#[cfg(test)]
pub fn write_dir(tr: &Trace, dir: &Path) {
    std::fs::create_dir_all(dir.join("trees")).unwrap();
    std::fs::write(dir.join("manifest.json"), tr.manifest.to_string()).unwrap();
    let mut ev = String::new();
    for e in &tr.events {
        let mut o = serde_json::json!({"t": e.t, "type": e.ty});
        if !e.act.is_empty() { o["act"] = e.act.clone().into(); }
        if !e.name.is_empty() { o["name"] = e.name.clone().into(); }
        if !e.file.is_empty() { o["file"] = e.file.clone().into(); }
        ev += &format!("{o}\n");
    }
    std::fs::write(dir.join("events.jsonl"), ev).unwrap();
    let (mut bin, mut idx) = (Vec::new(), Vec::new());
    for r in &tr.recs {
        let off = bin.len() as u64;
        bin.extend_from_slice(&r.seq.to_le_bytes());
        bin.push(r.kind);
        bin.extend_from_slice(&(r.raw_len as u32).to_le_bytes());
        bin.extend_from_slice(&(r.z.len() as u32).to_le_bytes());
        bin.extend_from_slice(&r.z);
        idx.extend_from_slice(&r.seq.to_le_bytes());
        idx.extend_from_slice(&r.t.to_le_bytes());
        idx.extend_from_slice(&off.to_le_bytes());
    }
    std::fs::write(dir.join("frames.bin"), bin).unwrap();
    std::fs::write(dir.join("frames.idx"), idx).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bench-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn frames(tr: &Trace) -> Vec<(u32, u32, Vec<u8>)> {
        let mut r = Replay::new(tr);
        let mut v = Vec::new();
        while let Some((s, t)) = r.advance().unwrap() { v.push((s, t, r.cur.clone())); }
        v
    }

    #[test]
    fn roundtrip_through_disk() {
        let tr = synth("rt", 64, 96);
        let d = tmpdir("rt");
        write_dir(&tr, &d);
        std::fs::write(d.join("trees/0000.json"), r#"{"windows":[{"root":{"kids":[{},{"kids":[{}]}]}}]}"#).unwrap();
        let back = Trace::load(&d).unwrap();
        assert_eq!((back.w, back.h), (64, 96));
        assert_eq!(back.events.len(), tr.events.len());
        assert_eq!(back.events.iter().filter(|e| e.ty == "step").count(), 5);
        assert_eq!(back.end_ms(), 8500);
        assert_eq!(frames(&tr), frames(&back));
        assert!(frames(&back).len() >= 8);
        assert_eq!(back.tree_nodes("trees/0000.json"), Some(4));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn delta_is_big_endian_and_applies() {
        // 手工构造：2x4 像素屏，第 1 行变化
        let (w, h) = (2usize, 4usize);
        let a = vec![7u8; w * h * 4];
        let mut b = a.clone();
        b[w * 4..w * 8].fill(9);
        let r = make_rec(Some(&a), &b, w, 1, 10, false).unwrap();
        let mut raw = Vec::new();
        ZlibDecoder::new(&r.z[..]).read_to_end(&mut raw).unwrap();
        assert_eq!(&raw[..4], &[0, 1, 0, 1]);
        assert!(make_rec(Some(&a), &a, w, 2, 20, false).is_none());
    }
}
