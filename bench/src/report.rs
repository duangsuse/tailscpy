//! 报表：report.json（确定性，内容里没有墙钟 / 日期 / 主机名）、report.md、timing.json（墙钟，不确定）、frames.csv。
//! 约定：KB = 1000 字节。

use crate::run::Params;
use crate::x264::X264Report;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// 首屏预算：300 KB/s × 300 ms（80 §6）
pub const BUDGET_BYTES: u64 = 90_000;

pub fn r3(x: f64) -> f64 { (x * 1000.0).round() / 1000.0 }

/// 五类交互（80 §6）。M0 没有页面缓存识别，回访 / 回滚恒为 0。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // Revisit / BackScroll 要等 M2 的页面缓存识别才会产生
pub enum Class { FirstVisit = 0, Revisit, NewScroll, BackScroll, Click }

pub const CLASSES: [(&str, &str); 5] = [("first_visit", "首访"), ("revisit", "回访"), ("new_scroll", "新滚"), ("back_scroll", "回滚"), ("click", "点击")];

impl Class {
    /// step 的动作 → 类别
    pub fn of_act(act: &str) -> Class {
        match act {
            "open" | "launch" | "cold" => Class::FirstVisit,
            "swipe" | "wheel" | "scroll" | "fling" => Class::NewScroll,
            _ => Class::Click,   // tap / back / long / key / type ...
        }
    }
}

#[derive(Serialize, Clone)]
pub struct ClassRow { pub class: &'static str, pub label: &'static str, pub bytes: u64, pub kb_per_min: f64, pub implemented: bool }

pub fn class_rows(bytes: &[u64; 5], duration_ms: u64) -> Vec<ClassRow> {
    (0..5).map(|i| ClassRow {
        class: CLASSES[i].0, label: CLASSES[i].1, bytes: bytes[i], kb_per_min: kb_per_min(bytes[i], duration_ms),
        implemented: !matches!(i, 1 | 3),
    }).collect()
}

pub fn kb_per_min(bytes: u64, duration_ms: u64) -> f64 {
    if duration_ms == 0 { 0.0 } else { r3(bytes as f64 / 1000.0 / (duration_ms as f64 / 60000.0)) }
}

#[derive(Serialize, Clone)]
pub struct OpRow { pub name: String, pub bytes: u64, pub count: u64 }

#[derive(Serialize, Clone)]
pub struct SegRow { pub label: String, pub t_ms: u32, pub class: &'static str, pub frames: u32, pub bytes: u64 }

#[derive(Serialize, Clone)]
pub struct StepLat { pub i: u32, pub act: String, pub t_ms: u32, pub latency_ms: Option<f64> }

#[derive(Serialize, Clone, Default)]
pub struct LatStats { pub n: u32, pub p50_ms: Option<f64>, pub p90_ms: Option<f64>, pub max_ms: Option<f64> }

pub fn lat_stats(mut v: Vec<f64>) -> LatStats {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pick = |p: f64| if v.is_empty() { None } else { Some(v[((p * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1]) };
    LatStats { n: v.len() as u32, p50_ms: pick(0.5), p90_ms: pick(0.9), max_ms: v.last().copied() }
}

#[derive(Serialize, Clone, Default)]
pub struct Correctness {
    /// 是否做了逐像素比对（影子编码没有线上字节，不做）
    pub checked: bool,
    pub checks: u32,
    pub failed: u32,
    pub diff_px_max: u64,
    /// 失败的检查点（最多 20 个）：段标签与不同像素数
    pub failures: Vec<(String, u64)>,
}

#[derive(Serialize, Clone)]
pub struct TraceReport {
    pub name: String,
    pub w: usize,
    pub h: usize,
    pub duration_ms: u32,
    /// 重放的帧数 / 编码后有字节的帧数
    pub frames: u32,
    pub frames_sent: u32,
    pub steps: u32,
    pub marks: u32,
    pub trees: u32,
    pub tree_nodes: u64,
    pub bytes_total: u64,
    pub kb_per_min: f64,
    pub classes: Vec<ClassRow>,
    pub first_frame_bytes: u64,
    pub first_visit_bytes: u64,
    pub ops: Vec<OpRow>,
    pub segments: Vec<SegRow>,
    pub latency: LatStats,
    pub step_latency: Vec<StepLat>,
    pub peak_window_bytes_1s: u64,
    pub peak_queue_bytes: u64,
    pub last_arrival_ms: f64,
    pub correctness: Correctness,
    pub x264: X264Report,
}

#[derive(Serialize)]
pub struct Summary {
    pub traces: u32,
    pub duration_ms: u64,
    pub frames: u32,
    pub bytes_total: u64,
    pub kb_per_min: f64,
    pub classes: Vec<ClassRow>,
    pub budget_bytes: u64,
    pub first_visit_p90_bytes: u64,
    pub over_budget: bool,
    pub ops: Vec<OpRow>,
    pub latency: LatStats,
    pub peak_window_bytes_1s: u64,
    pub peak_queue_bytes: u64,
    pub correctness_checks: u32,
    pub correctness_failed: u32,
    pub x264_bytes_total: u64,
}

/// M0 占位（M2 起填）
#[derive(Serialize, Default)]
pub struct Placeholders {
    pub offline_truth_score: Option<f64>,
    pub oracle_dedupe_bytes: Option<u64>,
    pub oracle_static_bytes: Option<u64>,
    pub ablation: Option<f64>,
}

#[derive(Serialize)]
pub struct Report {
    pub schema: u32,
    pub encoder: String,
    pub params: Params,
    pub summary: Summary,
    pub traces: Vec<TraceReport>,
    pub placeholders: Placeholders,
}

fn p90(mut v: Vec<u64>) -> u64 {
    v.sort_unstable();
    if v.is_empty() { 0 } else { v[((0.9 * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1] }
}

pub fn summarize(trs: &[TraceReport], lat: Vec<f64>) -> Summary {
    let dur: u64 = trs.iter().map(|t| t.duration_ms as u64).sum();
    let mut cb = [0u64; 5];
    let mut ops: Vec<OpRow> = Vec::new();
    for t in trs {
        for (i, c) in t.classes.iter().enumerate() { cb[i] += c.bytes; }
        for o in &t.ops {
            match ops.iter_mut().find(|x| x.name == o.name) {
                Some(x) => { x.bytes += o.bytes; x.count += o.count; }
                None => ops.push(o.clone()),
            }
        }
    }
    let total: u64 = trs.iter().map(|t| t.bytes_total).sum();
    let fv = p90(trs.iter().map(|t| t.first_visit_bytes).collect());
    Summary {
        traces: trs.len() as u32, duration_ms: dur, frames: trs.iter().map(|t| t.frames).sum(), bytes_total: total,
        kb_per_min: kb_per_min(total, dur), classes: class_rows(&cb, dur), budget_bytes: BUDGET_BYTES,
        first_visit_p90_bytes: fv, over_budget: fv > BUDGET_BYTES, ops, latency: lat_stats(lat),
        peak_window_bytes_1s: trs.iter().map(|t| t.peak_window_bytes_1s).max().unwrap_or(0),
        peak_queue_bytes: trs.iter().map(|t| t.peak_queue_bytes).max().unwrap_or(0),
        correctness_checks: trs.iter().map(|t| t.correctness.checks).sum(),
        correctness_failed: trs.iter().map(|t| t.correctness.failed).sum(),
        x264_bytes_total: trs.iter().map(|t| t.x264.bytes).sum(),
    }
}

// ---------------------------------------------------------------- 墙钟（不进 report.json）

#[derive(Serialize)]
pub struct TraceTiming { pub name: String, pub frames: u32, pub encode_ms: f64, pub decode_ms: f64, pub x264_ms: f64 }

#[derive(Serialize)]
pub struct Timing {
    pub note: &'static str,
    pub traces: Vec<TraceTiming>,
    pub total_wall_ms: f64,
}

// ---------------------------------------------------------------- 输出

fn opt(x: Option<f64>) -> String { x.map_or("-".into(), |v| format!("{v:.1}")) }
fn kb(b: u64) -> String { format!("{:.1}", b as f64 / 1000.0) }

pub fn markdown(r: &Report, prev: &str) -> String {
    use std::fmt::Write;
    let (s, p) = (&r.summary, &r.params);
    let mut m = String::new();
    let w = &mut m;
    let _ = writeln!(w, "# bench 报表\n");
    let _ = writeln!(w, "编码器 `{}`；带宽 {} KB/s，rtt {} ms，抖动 {} ms，seed {}；zstd level {}；KB = 1000 字节。\n",
        r.encoder, p.bw / 1000, p.rtt_us as f64 / 1000.0, p.jitter_us as f64 / 1000.0, p.seed, p.zstd);
    let _ = writeln!(w, "## 主指标：每分钟用法加权字节\n");
    let _ = writeln!(w, "**{:.1} KB/min**（共 {} 条 trace，{:.1} s，下行总字节 {} KB）。M0 的“用法”就是脚本化会话本身，权重 = 各 trace 时长占比。\n",
        s.kb_per_min, s.traces, s.duration_ms as f64 / 1000.0, kb(s.bytes_total));
    let _ = writeln!(w, "| 交互类别 | 字节 | KB/min | 备注 |\n|---|---:|---:|---|");
    for c in &s.classes {
        let _ = writeln!(w, "| {} | {} | {:.1} | {} |", c.label, c.bytes, c.kb_per_min, if c.implemented { "" } else { "M0 未区分（无页面缓存识别），恒为 0" });
    }
    let _ = writeln!(w, "\n归类：首帧及第一个 step 之前归首访；step 的 act 为 tap / back / long 等归点击，swipe / wheel 归新滚；mark 不改变归类。\n");
    let _ = writeln!(w, "## 首屏预算\n");
    let _ = writeln!(w, "首访字节 p90 = **{} KB**，预算线 {} KB（300 KB/s × 300 ms）：{}。\n", kb(s.first_visit_p90_bytes), kb(s.budget_bytes),
        if s.over_budget { "超出" } else { "未超出" });
    let _ = writeln!(w, "## 按 Op 类型\n\n| Op | 个数 | 字节 |\n|---|---:|---:|");
    for o in &s.ops { let _ = writeln!(w, "| {} | {} | {} |", o.name, o.count, o.bytes); }
    let _ = writeln!(w, "\n## 输入 → 画面稳定耗时（经网络模型，含上行 rtt/2）\n");
    let _ = writeln!(w, "n = {}，p50 {} ms，p90 {} ms，max {} ms。\n", s.latency.n, opt(s.latency.p50_ms), opt(s.latency.p90_ms), opt(s.latency.max_ms));
    let _ = writeln!(w, "## 峰值\n\n- 码率峰值（任意 1 s 窗口产生的字节）：{} KB\n- 发送队列峰值：{} KB\n", kb(s.peak_window_bytes_1s), kb(s.peak_queue_bytes));
    let _ = writeln!(w, "## 正确性\n\n逐像素比对（每个步骤段结束时 A 画布 vs B 帧）：{} 次检查，{} 次不一致。\n", s.correctness_checks, s.correctness_failed);
    let _ = writeln!(w, "## 离线真值打分 / oracle / 消融\n\nM2 起填（oracle dedupe、oracle static、位移检测 regret、逐特性消融）。\n");
    let _ = writeln!(w, "## 与上一份报表对比\n\n{prev}\n");
    for t in &r.traces {
        let _ = writeln!(w, "## trace `{}`（{}x{}，{:.1} s）\n", t.name, t.w, t.h, t.duration_ms as f64 / 1000.0);
        let _ = writeln!(w, "- 帧 {}（有字节 {}），step {}，mark {}，节点树 {}（共 {} 节点）", t.frames, t.frames_sent, t.steps, t.marks, t.trees, t.tree_nodes);
        let _ = writeln!(w, "- 下行 {} KB，{:.1} KB/min；首帧 {} KB，首访 {} KB；最后一帧到达 A：{:.1} ms", kb(t.bytes_total), t.kb_per_min, kb(t.first_frame_bytes), kb(t.first_visit_bytes), t.last_arrival_ms);
        let _ = writeln!(w, "- 耗时 p50 {} / p90 {} ms；码率峰值 {} KB/s；队列峰值 {} KB", opt(t.latency.p50_ms), opt(t.latency.p90_ms), kb(t.peak_window_bytes_1s), kb(t.peak_queue_bytes));
        let c = &t.correctness;
        let _ = writeln!(w, "- 正确性：{}", if c.checked { format!("{} 次检查，{} 次不一致，最大差异 {} px", c.checks, c.failed, c.diff_px_max) } else { "影子编码，未比对".into() });
        let x = &t.x264;
        if x.status == "ok" {
            let _ = writeln!(w, "- x264（{} fps，封顶 {} kbps，veryfast+zerolatency，threads=1）：{} 帧，{} KB；{} 个比对点上不同像素占比 {:.2}%，文字 / 边缘像素 {:.2}%",
                x.fps, x.kbps_cap, x.out_frames, kb(x.bytes), x.checks, x.diff_ratio * 100.0, x.edge_diff_ratio * 100.0);
        } else {
            let _ = writeln!(w, "- x264：跳过（{}）", x.reason);
        }
        let _ = writeln!(w, "\n| 段 | t (ms) | 类别 | 帧 | 字节 |\n|---|---:|---|---:|---:|");
        for g in &t.segments { let _ = writeln!(w, "| {} | {} | {} | {} | {} |", g.label, g.t_ms, g.class, g.frames, g.bytes); }
        let _ = writeln!(w, "\n| step | act | t (ms) | 稳定耗时 (ms) |\n|---:|---|---:|---:|");
        for l in &t.step_latency { let _ = writeln!(w, "| {} | {} | {} | {} |", l.i, l.act, l.t_ms, opt(l.latency_ms)); }
        let _ = writeln!(w);
    }
    m
}

/// 与最近一份其他报表目录（按 report.json 修改时间）比主指标；返回 markdown 片段。
pub fn compare_prev(root: &Path, cur: &Path, now: &Report) -> String {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p == cur { continue; }
            if let Ok(mt) = std::fs::metadata(p.join("report.json")).and_then(|m| m.modified()) {
                if best.as_ref().is_none_or(|(t, _)| mt > *t) { best = Some((mt, p)); }
            }
        }
    }
    let Some((_, dir)) = best else { return "无上一份".into() };
    let Some(old) = std::fs::read(dir.join("report.json")).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) else {
        return format!("上一份 `{}` 无法解析", dir.file_name().unwrap().to_string_lossy());
    };
    let (o, s) = (&old["summary"], &now.summary);
    let row = |name: &str, old: Option<f64>, new: f64| match old {
        Some(a) => format!("| {name} | {a:.1} | {new:.1} | {:+.1} |", new - a),
        None => format!("| {name} | - | {new:.1} | - |"),
    };
    let mut t = format!("上一份：`{}`\n\n| 指标 | 上一份 | 本次 | 差值 |\n|---|---:|---:|---:|\n", dir.file_name().unwrap().to_string_lossy());
    for l in [
        row("KB/min", o["kb_per_min"].as_f64(), s.kb_per_min),
        row("下行总字节 (KB)", o["bytes_total"].as_f64().map(|v| v / 1000.0), s.bytes_total as f64 / 1000.0),
        row("首访 p90 (KB)", o["first_visit_p90_bytes"].as_f64().map(|v| v / 1000.0), s.first_visit_p90_bytes as f64 / 1000.0),
        row("稳定耗时 p90 (ms)", o["latency"]["p90_ms"].as_f64(), s.latency.p90_ms.unwrap_or(0.0)),
        row("码率峰值 (KB)", o["peak_window_bytes_1s"].as_f64().map(|v| v / 1000.0), s.peak_window_bytes_1s as f64 / 1000.0),
    ] { t += &l; t.push('\n'); }
    t
}

pub fn write_all(dir: &Path, root: &Path, r: &Report, timing: &Timing, csv: &str) -> Result<(), String> {
    let io = |e: std::io::Error| format!("写 {}: {e}", dir.display());
    std::fs::create_dir_all(dir).map_err(io)?;
    let prev = compare_prev(root, dir, r);
    std::fs::write(dir.join("report.json"), serde_json::to_string_pretty(r).unwrap() + "\n").map_err(io)?;
    std::fs::write(dir.join("report.md"), markdown(r, &prev)).map_err(io)?;
    std::fs::write(dir.join("timing.json"), serde_json::to_string_pretty(timing).unwrap() + "\n").map_err(io)?;
    std::fs::write(dir.join("frames.csv"), csv).map_err(io)?;
    Ok(())
}

/// 今天的日期 YYYYMMDD（UTC，仅用于目录名）。
pub fn today() -> String {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 86400).unwrap_or(0) as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}{m:02}{d:02}")
}
