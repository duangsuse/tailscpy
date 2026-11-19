//! x264 对照（80 §7）：同一批帧离线喂 ffmpeg/libx264，统计总字节，并把解码后的画面与原帧比。
//!
//! 取舍（简单、可复现）：按录制时间戳以固定 fps 取样，输出帧 k 取 t ≤ k*1000/fps ms 的最新 B 帧（不足则重复）；
//! rawvideo RGBA 管道 → libx264 veryfast + zerolatency、threads=1、码率封顶；raw H.264 字节数即总字节。
//! 比对点：每个步骤段末尾（下一次输入前一刻）的输出帧，那时画面已静止，看 x264 收敛到什么程度。

use crate::trace::{Replay, Trace};
use serde::Serialize;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

#[derive(Serialize, Default, Clone)]
pub struct X264Report {
    /// ok / skipped
    pub status: String,
    pub reason: String,
    pub fps: u32,
    pub kbps_cap: u32,
    pub out_frames: u32,
    pub bytes: u64,
    /// 比对点个数
    pub checks: u32,
    /// 比对像素总数 / 与原图不同（任一 RGB 通道不同）的个数
    pub px: u64,
    pub px_diff: u64,
    /// 其中“文字 / 边缘像素”（原图里与左邻或上邻颜色不同）
    pub edge_px: u64,
    pub edge_diff: u64,
    pub diff_ratio: f64,
    pub edge_diff_ratio: f64,
}

pub fn skipped(why: &str) -> X264Report { X264Report { status: "skipped".into(), reason: why.into(), ..Default::default() } }

pub fn available() -> bool {
    Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

/// 固定 fps 的“保持”帧源：第 k 帧 = t ≤ k*1000/fps 的最新 B 帧。
struct Hold<'a> { rp: Replay<'a>, pending: Option<u32>, fps: u32 }

impl<'a> Hold<'a> {
    fn new(tr: &'a Trace, fps: u32) -> Self {
        let rp = Replay::new(tr);
        Hold { pending: rp.peek_t(), rp, fps }
    }
    fn frame(&mut self, k: u32) -> Result<&[u8], String> {
        let tk = (k as u64 * 1000 / self.fps as u64) as u32;
        while self.pending.is_some_and(|t| t <= tk) {
            self.rp.advance()?;
            self.pending = self.rp.peek_t();
        }
        Ok(&self.rp.cur)
    }
}

/// ignore：不参与比对的行带（闭区间）；ends_ms：各步骤段的结束时刻（比对点）。
pub fn run(tr: &Trace, kbps: u32, fps: u32, ignore: &[(usize, usize)], ends_ms: &[u32], tmp: &std::path::Path) -> Result<X264Report, String> {
    let (w, h) = (tr.w, tr.h);
    if w % 2 != 0 || h % 2 != 0 { return Ok(skipped("宽高不是偶数，yuv420 无法直接编")); }
    let n = (tr.end_ms() as u64 * fps as u64 / 1000) as u32 + 1;
    let h264 = tmp.join(format!(".x264-{}.h264", std::process::id()));
    let r = run_inner(tr, kbps, fps, ignore, ends_ms, n, &h264);
    let _ = std::fs::remove_file(&h264);
    r
}

fn run_inner(tr: &Trace, kbps: u32, fps: u32, ignore: &[(usize, usize)], ends_ms: &[u32], n: u32, h264: &std::path::Path) -> Result<X264Report, String> {
    let (w, h) = (tr.w, tr.h);
    let k = format!("{kbps}k");
    let mut enc = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{w}x{h}"), "-framerate", &fps.to_string(), "-i", "-"])
        .args(["-threads", "1", "-filter_threads", "1", "-fflags", "+bitexact", "-flags:v", "+bitexact"])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency", "-x264-params", "threads=1:sliced-threads=0"])
        .args(["-b:v", &k, "-maxrate", &k, "-bufsize", &k, "-pix_fmt", "yuv420p", "-f", "h264"])
        .arg(h264).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| format!("启动 ffmpeg: {e}"))?;
    {
        let mut si = enc.stdin.take().unwrap();
        let mut hold = Hold::new(tr, fps);
        for i in 0..n { si.write_all(hold.frame(i)?).map_err(|e| format!("写 ffmpeg 管道: {e}"))?; }
    }
    let out = enc.wait_with_output().map_err(|e| format!("等 ffmpeg: {e}"))?;
    if !out.status.success() { return Err(format!("ffmpeg 编码失败: {}", String::from_utf8_lossy(&out.stderr))); }
    let bytes = std::fs::metadata(h264).map_err(|e| e.to_string())?.len();

    // 比对点：段末前一刻的输出帧
    let mut check_k: Vec<u32> = ends_ms.iter().map(|&e| (e.saturating_sub(1) as u64 * fps as u64 / 1000) as u32).map(|k| k.min(n - 1)).collect();
    check_k.sort_unstable();
    check_k.dedup();
    let mut dec = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-threads", "1", "-i"]).arg(h264)
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
        .map_err(|e| format!("启动 ffmpeg 解码: {e}"))?;
    let mut so = dec.stdout.take().unwrap();
    let mut hold = Hold::new(tr, fps);
    let mut buf = vec![0u8; w * h * 4];
    let mut rep = X264Report { status: "ok".into(), fps, kbps_cap: kbps, out_frames: n, bytes, ..Default::default() };
    let mut ci = 0;
    for i in 0..n {
        so.read_exact(&mut buf).map_err(|e| format!("读解码帧 {i}/{n}: {e}"))?;
        if ci < check_k.len() && check_k[ci] == i {
            ci += 1;
            compare(hold.frame(i)?, &buf, w, h, ignore, &mut rep);
        }
    }
    drop(so);
    let _ = dec.wait();
    let r = |a: u64, b: u64| if b == 0 { 0.0 } else { (a as f64 / b as f64 * 1e6).round() / 1e6 };
    rep.diff_ratio = r(rep.px_diff, rep.px);
    rep.edge_diff_ratio = r(rep.edge_diff, rep.edge_px);
    Ok(rep)
}

fn compare(orig: &[u8], dec: &[u8], w: usize, h: usize, ignore: &[(usize, usize)], rep: &mut X264Report) {
    rep.checks += 1;
    for y in (0..h).filter(|y| !ignore.iter().any(|&(a, b)| (a..=b).contains(y))) {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let diff = orig[o..o + 3] != dec[o..o + 3];
            let edge = (x > 0 && orig[o..o + 3] != orig[o - 4..o - 1]) || (y > 0 && orig[o..o + 3] != orig[o - w * 4..o - w * 4 + 3]);
            rep.px += 1;
            rep.px_diff += diff as u64;
            if edge { rep.edge_px += 1; rep.edge_diff += diff as u64; }
        }
    }
}
