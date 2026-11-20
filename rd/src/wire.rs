//! wire 类型（50 §2）。postcard 不自描述：**枚举变体只在末尾追加，字段不增删不改序**；不兼容就升 `VER`。
//! 同一批类型另导出 JSON（外部标签：`{"Tap":{"page":1,"at":{...}}}`），只用于日志、录制、脚本和调试口，不上线。
//! 消息 = u32 LE 长度前缀 + postcard（`frame` / `Deframer`）。

use serde::{Deserialize, Serialize};

pub const VER: u16 = 1;

pub type PageId = u64;
pub type PgRev = u32;
pub type Hash = u64;

/// Scroll 画布的 y 原点：Dst.y 是 u32，而向上滚会得到“比起点更小”的内容坐标，所以起点放在 2^20（画布按分块稀疏存储，不占内存）
pub const ORIGIN: u32 = 1 << 20;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct Geom { pub w: u32, pub h: u32, pub dpi: u32 }

/// 区域内坐标。M1 整屏一个 `Fixed` 区域（region 0），x、y 就是虚拟屏像素。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct Pt { pub region: u16, pub x: i32, pub y: i32 }

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Scope { Global, App, Page }

/// A→B
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Up {
    // ctl
    Hello { ver: u16, token: String, cache_id: String, cache_rev: u64 },
    Inventory { pages: Vec<(PageId, PgRev)>, sprites: Vec<Hash> },
    Open { pkg: String, geom: Option<Geom> },
    Close,
    Tune { scope: Scope, key: String, val: f64 },
    // up
    View { page: PageId, region: u16, y: i32, vy: f32 },
    Tap { page: PageId, at: Pt },
    LongPress { page: PageId, at: Pt },
    /// phase：0 按下 1 移动 2 抬起；dt = 距上一个采样的毫秒数
    Touch { page: PageId, phase: u8, at: Pt, dt: u32 },
    Wheel { page: PageId, at: Pt, dx: f32, dy: f32 },
    Back,
    Key { code: u32, down: bool, meta: u32 },
    Text { s: String },
    ClipSet { s: String },
    ClipGet,
    Ack { seq: u64 },
    Persisted { page: PageId, rev: PgRev, cache_rev: u64 },
    Forget { page: PageId },
    Resync { page: PageId },
    VerifyFail { page: PageId, region: u16, y0: u32, y1: u32 },
    // —— 以下为后加（只追加）——
    Ping { t: u64 },
    /// 网络探针 P5：让 B 另开一条单向流写 `bytes` 字节，A 量吞吐
    Probe { bytes: u32 },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum CacheMode { Keep, NeedInventory, Reset }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AppInfo { pub pkg: String, pub name: String, pub icon: Option<Vec<u8>> }

/// 参数表里的一项（50 §6）。A 的 l 面板照表画控件。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Param { pub key: String, pub label: String, pub group: String, pub val: f64, pub min: f64, pub max: f64, pub step: f64, pub scope: Scope }

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum FaultCode { BadVersion, BadToken, OffSession, AppGone, BlankFrame, Stale, Busy }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Stats {
    /// 各层字节（bg / lit / fg）：会话累计
    pub layers: Vec<(String, u64)>,
    /// 各省流机制省下的字节（估算）
    pub saved: Vec<(String, u64)>,
    /// 其它数值：t_ms、tx_bytes_total、frames、fps、enc_ms 等
    pub extra: Vec<(String, f64)>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum PageOp { Push, Pop, Replace }

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Rect { pub x: u32, pub y: u32, pub w: u32, pub h: u32 }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum RegionKind { Fixed { anchor: u8, z: u8 }, Scroll, Video }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Region { pub id: u16, pub kind: RegionKind, pub rect: Rect, pub start_y: i32 }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Modal { pub rect: Rect, pub dim: [u8; 4] }

/// B→A
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Down {
    // ctl
    Welcome { ver: u16, cache: CacheMode, apps: Vec<AppInfo>, params: Vec<Param> },
    Purge { pages: Vec<PageId> },
    Opened { pkg: String, geom: Geom },
    Focus { editable: bool },
    Clip { s: String },
    Stats(Stats),
    Params { params: Vec<Param> },
    Fault { code: FaultCode, msg: String },
    Replaced,
    // tx（一条有序流，带递增 seq）
    Page { seq: u64, id: PageId, op: PageOp, temp: bool, modal: Option<Modal>, at_root: bool, regions: Vec<Region> },
    Regions { seq: u64, page: PageId, regions: Vec<Region> },
    Tx { seq: u64, page: PageId, rev: PgRev, ops: Vec<Op> },
    PageReset { seq: u64, page: PageId },
    // —— 以下为后加（只追加）——
    Pong { t: u64 },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Dst { pub region: u16, pub x: u32, pub y: u32, pub w: u32, pub h: u32 }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Src { Canvas { page: PageId, region: u16, x: u32, y: u32 }, Sprite { hash: Hash, x: u32, y: u32 } }

/// Lit 的编码：
/// - Palette：`[n u16 LE][esc u8][调色板 RGB × n][索引平面 w*h 字节][转义像素 RGB…]`，整体过 zstd。n=256 无转义；否则 n=255，索引 255 表示“转义”，按光栅序从尾部取 RGB。调色板按亮度排序
/// - Residual：RGB 去相关（G, R−G, B−G）后每通道平面做 Paeth 预测，三个平面依次拼接，过 zstd
/// - Raw：RGBA 原样，过 zstd
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum LitEnc { Palette, Residual, Raw }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Op {
    Fill { dst: Dst, color: [u8; 4] },
    Lit { dst: Dst, enc: LitEnc, dict: Option<Src>, data: Vec<u8> },
    /// 有损图片：data 是 JPEG（A 用 createImageBitmap 解）。mask 非空时为 1 位/像素的“与底色相同”掩码（M4 才用）
    Image { dst: Dst, sprite: Option<Hash>, q: u8, mask: Vec<u8>, data: Vec<u8> },
    SpriteDef { hash: Hash, w: u32, h: u32, enc: LitEnc, data: Vec<u8> },
    SpriteDrop { hash: Hash },
    Copy { src: Src, dst: Dst },
    Overlay { id: u32, dst: Dst, ops: Vec<Op> },
    OverlayDrop { id: u32 },
    Commit { id: u32 },
    Clear { region: u16 },
    Known { region: u16, y_min: i32, y_max: i32, at_top: bool, at_bottom: bool, total_h: Option<u32> },
    Verify { region: u16, y0: u32, y1: u32, fold: Hash },
    // —— 以下为后加（只追加）——
    /// 采集头位置：该 Scroll 区域屏幕第一行对应的画布 y。A 在“跟随”状态下据此摆视口
    Head { region: u16, y: i32 },
}

impl Op {
    /// 统计用的层标签（40 §9）：bg = Fill / Lit / 画布间 Copy；lit = 精灵、图片；fg = 视频区（M6）
    pub fn layer(&self) -> &'static str {
        match self {
            Op::Image { .. } | Op::SpriteDef { .. } => "lit",
            Op::Copy { src: Src::Sprite { .. }, .. } => "lit",
            Op::Overlay { ops, .. } => ops.first().map_or("bg", |o| o.layer()),
            _ => "bg",
        }
    }
    pub fn dst(&self) -> Option<Dst> {
        match self {
            Op::Fill { dst, .. } | Op::Lit { dst, .. } | Op::Image { dst, .. } | Op::Copy { dst, .. } | Op::Overlay { dst, .. } => Some(*dst),
            _ => None,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Op::Fill { .. } => "Fill", Op::Lit { enc: LitEnc::Palette, .. } => "Lit.Palette", Op::Lit { enc: LitEnc::Residual, .. } => "Lit.Residual",
            Op::Lit { .. } => "Lit.Raw", Op::Image { .. } => "Image", Op::SpriteDef { .. } => "SpriteDef", Op::SpriteDrop { .. } => "SpriteDrop",
            Op::Copy { .. } => "Copy", Op::Overlay { .. } => "Overlay", Op::OverlayDrop { .. } => "OverlayDrop", Op::Commit { .. } => "Commit",
            Op::Clear { .. } => "Clear", Op::Known { .. } => "Known", Op::Verify { .. } => "Verify", Op::Head { .. } => "Head",
        }
    }
}

// —— 编解码 ——

pub fn to_postcard<T: Serialize>(m: &T) -> Vec<u8> { postcard::to_stdvec(m).expect("postcard 编码") }
pub fn from_postcard<'a, T: Deserialize<'a>>(b: &'a [u8]) -> Result<T, String> { postcard::from_bytes(b).map_err(|e| e.to_string()) }
pub fn to_json<T: Serialize>(m: &T) -> String { serde_json::to_string(m).expect("json 编码") }
pub fn from_json<T: for<'a> Deserialize<'a>>(s: &str) -> Result<T, String> { serde_json::from_str(s).map_err(|e| e.to_string()) }

/// 长度前缀（u32 LE）+ postcard
pub fn frame<T: Serialize>(m: &T) -> Vec<u8> {
    let body = to_postcard(m);
    let mut v = Vec::with_capacity(4 + body.len());
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(&body);
    v
}

/// 字节流 → 一条条消息体（长度前缀切分）。单条上限 `MAX_MSG` 防止被恶意长度撑爆内存。
pub const MAX_MSG: usize = 64 << 20;
#[derive(Default)]
pub struct Deframer { buf: Vec<u8> }
impl Deframer {
    pub fn push(&mut self, b: &[u8]) { self.buf.extend_from_slice(b) }
    /// 取下一条完整消息体；长度非法返回 Err
    pub fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.buf.len() < 4 { return Ok(None) }
        let n = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if n > MAX_MSG { return Err(format!("消息过长 {n}")) }
        if self.buf.len() < 4 + n { return Ok(None) }
        let m = self.buf[4..4 + n].to_vec();
        self.buf.drain(..4 + n);
        Ok(Some(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples_up() -> Vec<Up> {
        vec![
            Up::Hello { ver: VER, token: "t".into(), cache_id: "c".into(), cache_rev: 7 },
            Up::Open { pkg: "com.android.settings".into(), geom: Some(Geom { w: 1080, h: 2640, dpi: 420 }) },
            Up::Tap { page: 1, at: Pt { region: 0, x: 403, y: 2573 } },
            Up::Touch { page: 1, phase: 1, at: Pt { region: 0, x: 1, y: -2 }, dt: 8 },
            Up::Key { code: 4, down: true, meta: 0 }, Up::Text { s: "你好".into() }, Up::Back, Up::Ack { seq: 9 },
            Up::Tune { scope: Scope::Global, key: "img_q".into(), val: 70.0 }, Up::Ping { t: 5 },
        ]
    }
    fn samples_down() -> Vec<Down> {
        let d = Dst { region: 0, x: 1, y: 2, w: 3, h: 4 };
        vec![
            Down::Welcome { ver: VER, cache: CacheMode::Reset, apps: vec![AppInfo { pkg: "p".into(), name: "n".into(), icon: None }], params: vec![] },
            Down::Opened { pkg: "p".into(), geom: Geom { w: 1, h: 2, dpi: 3 } },
            Down::Page { seq: 1, id: 2, op: PageOp::Replace, temp: true, modal: None, at_root: true,
                regions: vec![Region { id: 0, kind: RegionKind::Fixed { anchor: 0, z: 0 }, rect: Rect { x: 0, y: 0, w: 1080, h: 2640 }, start_y: 0 }] },
            Down::Tx { seq: 3, page: 2, rev: 1, ops: vec![Op::Fill { dst: d, color: [1, 2, 3, 255] }, Op::Lit { dst: d, enc: LitEnc::Palette, dict: None, data: vec![9; 5] },
                Op::Image { dst: d, sprite: None, q: 70, mask: vec![], data: vec![1, 2] }] },
            Down::Fault { code: FaultCode::OffSession, msg: "x".into() }, Down::Stats(Stats::default()), Down::Replaced, Down::Pong { t: 1 },
        ]
    }

    #[test] fn up_roundtrip_postcard_and_json() {
        for m in samples_up() {
            assert_eq!(from_postcard::<Up>(&to_postcard(&m)).unwrap(), m);
            assert_eq!(from_json::<Up>(&to_json(&m)).unwrap(), m);
        }
    }
    #[test] fn down_roundtrip_postcard_and_json() {
        for m in samples_down() {
            assert_eq!(from_postcard::<Down>(&to_postcard(&m)).unwrap(), m);
            assert_eq!(from_json::<Down>(&to_json(&m)).unwrap(), m);
        }
    }
    #[test] fn json_shape_is_externally_tagged() {
        assert_eq!(to_json(&Up::Back), "\"Back\"");
        assert_eq!(to_json(&Up::Tap { page: 1, at: Pt { region: 0, x: 2, y: 3 } }), r#"{"Tap":{"page":1,"at":{"region":0,"x":2,"y":3}}}"#);
    }
    /// 变体顺序即线格式：一旦发布，这几个字节就不能变（追加变体除外）
    #[test] fn variant_index_is_stable() {
        assert_eq!(to_postcard(&Up::Back)[0], 10);
        assert_eq!(to_postcard(&Up::Ping { t: 0 })[0], 20);
        assert_eq!(to_postcard(&Down::Replaced)[0], 8);
        assert_eq!(to_postcard(&Down::Pong { t: 0 })[0], 13);
    }
    #[test] fn deframer_splits_across_chunks() {
        let a = frame(&Up::Back); let b = frame(&Up::Ping { t: 300 });
        let all: Vec<u8> = a.iter().chain(b.iter()).cloned().collect();
        let mut d = Deframer::default(); let mut out = vec![];
        for c in all.chunks(3) { d.push(c); while let Some(m) = d.next().unwrap() { out.push(from_postcard::<Up>(&m).unwrap()) } }
        assert_eq!(out, vec![Up::Back, Up::Ping { t: 300 }]);
    }
    #[test] fn deframer_rejects_huge_length() {
        let mut d = Deframer::default(); d.push(&[0xff, 0xff, 0xff, 0x7f]); assert!(d.next().is_err());
    }
}
