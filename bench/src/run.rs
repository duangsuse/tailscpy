//! 单条 trace 的重放：按录制时间戳依次喂帧、节点树、步骤 → 编码器 → 网络模型 → A 画布，收集全部指标。

use crate::encoder::{Encoder, Naive, RdEnc, Shadow};
use crate::netmodel::{peak_window_bytes, Net};
use crate::report::*;
use crate::trace::{Replay, Trace};
use crate::x264;
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::Instant;

#[derive(Serialize, Clone)]
pub struct Params {
    /// 带宽，字节/秒
    pub bw: u64,
    pub rtt_us: u64,
    pub jitter_us: u64,
    pub seed: u64,
    pub zstd: i32,
    pub merge_gap: usize,
    /// 正确性比对 / x264 比对时排除的行带（闭区间）
    pub ignore: Vec<(usize, usize)>,
    /// 用影子编码（只统计）代替朴素基线
    pub shadow: bool,
    /// "rd"（M1 编码器）或 "naive"（朴素基线）
    pub encoder: String,
    pub x264: bool,
    pub x264_kbps: u32,
    pub x264_fps: u32,
    /// x264 临时文件目录
    #[serde(skip)]
    pub tmp: std::path::PathBuf,
}

pub struct TraceRun {
    pub encoder: &'static str,
    pub rep: TraceReport,
    pub timing: TraceTiming,
    /// 全部帧的 latency 样本不在这里；这里只给 frames.csv 的行
    pub csv: String,
    /// 本 trace 每步的耗时（汇总分位数用）
    pub lat: Vec<f64>,
}

struct Seg { label: String, t: u32, class: Class, frames: u32, bytes: u64 }

/// A 与 B 逐像素比（排除 ignore 行带），返回不同像素数。
fn diff_px(a: &[u8], b: &[u8], w: usize, ignore: &[(usize, usize)]) -> u64 {
    let rb = w * 4;
    let mut n = 0;
    for y in (0..a.len() / rb).filter(|y| !ignore.iter().any(|&(lo, hi)| (lo..=hi).contains(y))) {
        let (ra, rbw) = (&a[y * rb..(y + 1) * rb], &b[y * rb..(y + 1) * rb]);
        if ra != rbw { n += ra.as_chunks::<4>().0.iter().zip(rbw.as_chunks::<4>().0).filter(|(p, q)| p != q).count() as u64; }
    }
    n
}

pub fn run_trace(tr: &Trace, p: &Params) -> Result<TraceRun, String> {
    let (w, h) = (tr.w, tr.h);
    let mut enc: Box<dyn Encoder> = if p.shadow { Box::new(Shadow::new(w, h, p.merge_gap)) } else if p.encoder == "rd" { Box::new(RdEnc::new(w, h)) } else { Box::new(Naive::new(w, h, p.zstd, p.merge_gap)) };
    let mut net = Net::new(p.bw, p.rtt_us, p.jitter_us, p.seed);
    let mut rp = Replay::new(tr);
    let mut a = vec![0u8; w * h * 4];
    let mut ign = p.ignore.clone();
    ign.extend(tr.manifest_ignore());

    // 步骤 / 标记 / 节点树事件（rec_start / rec_stop 只用来定终点）；按时间稳定排序
    let mut evs: Vec<&crate::trace::Event> = tr.events.iter()
        .filter(|e| matches!(e.ty.as_str(), "step" | "mark" | "a11y_tree") && !(e.ty == "mark" && e.name.starts_with("rec_"))).collect();
    evs.sort_by_key(|e| e.t);

    let mut segs = vec![Seg { label: "(start)".into(), t: 0, class: Class::FirstVisit, frames: 0, bytes: 0 }];
    // 每个 step：(序号, act, t, 最后一个有字节的帧到达 A 的时刻 µs)
    let mut steps: Vec<(String, u32, Option<u64>)> = Vec::new();
    let (mut marks, mut trees, mut tree_nodes) = (0u32, 0u32, 0u64);
    let mut class_bytes = [0u64; 5];
    let mut ops: BTreeMap<&'static str, (u64, u64)> = BTreeMap::new();
    let (mut frames, mut sent_n, mut bytes_total, mut first_frame_bytes) = (0u32, 0u32, 0u64, 0u64);
    let (mut peak_queue, mut last_arrive) = (0u64, 0u64);
    let mut sends: Vec<(u64, u64)> = Vec::new();
    let mut csv = String::new();
    let mut corr = Correctness { checked: !p.shadow, ..Default::default() };
    let mut ends: Vec<u32> = Vec::new();
    let (mut enc_ns, mut dec_ns) = (0u128, 0u128);
    let mut ei = 0;

    // 一个步骤段结束：段内有帧就比一次 A / B，并记下段末时刻（x264 比对点）
    let finish = |segs: &[Seg], corr: &mut Correctness, ends: &mut Vec<u32>, a: &[u8], b: &[u8], t: u32| {
        let s = segs.last().unwrap();
        if s.frames == 0 { return; }
        ends.push(t);
        if !corr.checked { return; }
        let d = diff_px(a, b, w, &ign);
        corr.checks += 1;
        corr.diff_px_max = corr.diff_px_max.max(d);
        if d > 0 {
            corr.failed += 1;
            if corr.failures.len() < 20 { corr.failures.push((s.label.clone(), d)); }
        }
    };

    loop {
        let (ft, et) = (rp.peek_t(), evs.get(ei).map(|e| e.t));
        if ft.is_none() && et.is_none() { break; }
        if let Some(t) = et.filter(|&t| ft.is_none_or(|f| t <= f)) {
            // 事件先于同一时刻的帧
            let e = evs[ei];
            ei += 1;
            match e.ty.as_str() {
                "a11y_tree" => { trees += 1; tree_nodes += tr.tree_nodes(&e.file).unwrap_or(0) as u64; }
                ty => {
                    finish(&segs, &mut corr, &mut ends, &a, &rp.cur, t);
                    let cls = segs.last().unwrap().class;
                    if ty == "step" {
                        let c = Class::of_act(&e.act);
                        segs.push(Seg { label: format!("step {} {}", steps.len(), e.act), t, class: c, frames: 0, bytes: 0 });
                        steps.push((e.act.clone(), t, None));
                    } else {
                        marks += 1;
                        segs.push(Seg { label: format!("mark {}", e.name), t, class: cls, frames: 0, bytes: 0 });
                    }
                }
            }
            continue;
        }
        let (seq, t) = rp.advance()?.unwrap();
        let t0 = Instant::now();
        let e = enc.encode_at(&rp.cur, t as u64);
        enc_ns += t0.elapsed().as_nanos();
        if let Some(wire) = &e.wire {
            let t1 = Instant::now();
            enc.decode(&mut a, w, h, wire)?;
            dec_ns += t1.elapsed().as_nanos();
        }
        let seg = segs.last_mut().unwrap();
        let class = if frames == 0 { Class::FirstVisit } else { seg.class };
        let arrive = if e.bytes > 0 {
            let t_us = t as u64 * 1000;
            let s = net.send(t_us, e.bytes);
            sends.push((t_us, e.bytes));
            peak_queue = peak_queue.max(s.queue_bytes);
            last_arrive = last_arrive.max(s.arrive_us);
            sent_n += 1;
            bytes_total += e.bytes;
            class_bytes[class as usize] += e.bytes;
            seg.bytes += e.bytes;
            for (n, b, c) in &e.ops { let o = ops.entry(n).or_default(); o.0 += b; o.1 += *c as u64; }
            if let Some(st) = steps.last_mut() { st.2 = Some(s.arrive_us); }
            Some(s.arrive_us)
        } else { None };
        if frames == 0 { first_frame_bytes = e.bytes; }
        seg.frames += 1;
        frames += 1;
        csv.push_str(&format!("{},{seq},{t},{},{}\n", tr.name, e.bytes, arrive.map_or(String::new(), |u| format!("{:.3}", u as f64 / 1000.0))));
    }
    let end = tr.end_ms();
    finish(&segs, &mut corr, &mut ends, &a, &rp.cur, end);

    let one_way = net.one_way_us();
    let step_latency: Vec<StepLat> = steps.iter().enumerate().map(|(i, (act, t, arr))| StepLat {
        i: i as u32, act: act.clone(), t_ms: *t,
        latency_ms: arr.map(|u| r3((u + one_way).saturating_sub(*t as u64 * 1000) as f64 / 1000.0)),
    }).collect();
    let lat: Vec<f64> = step_latency.iter().filter_map(|l| l.latency_ms).collect();

    let t_x = Instant::now();
    let x = if !p.x264 { x264::skipped("--no-x264") } else if !x264::available() { x264::skipped("系统没有 ffmpeg") } else {
        x264::run(tr, p.x264_kbps, p.x264_fps, &ign, &ends, &p.tmp).unwrap_or_else(|e| x264::skipped(&e))
    };
    let x264_ms = t_x.elapsed().as_secs_f64() * 1000.0;

    let dur = end.max(1);
    let rep = TraceReport {
        name: tr.name.clone(), w, h, duration_ms: dur, frames, frames_sent: sent_n,
        steps: steps.len() as u32, marks, trees, tree_nodes,
        bytes_total, kb_per_min: kb_per_min(bytes_total, dur as u64), classes: class_rows(&class_bytes, dur as u64),
        first_frame_bytes, first_visit_bytes: class_bytes[0],
        ops: ops.into_iter().map(|(n, (b, c))| OpRow { name: n.into(), bytes: b, count: c }).collect(),
        segments: segs.iter().map(|s| SegRow { label: s.label.clone(), t_ms: s.t, class: CLASSES[s.class as usize].1, frames: s.frames, bytes: s.bytes }).collect(),
        latency: lat_stats(lat.clone()), step_latency,
        peak_window_bytes_1s: peak_window_bytes(&sends), peak_queue_bytes: peak_queue,
        last_arrival_ms: r3(last_arrive as f64 / 1000.0),
        correctness: corr, x264: x,
    };
    let timing = TraceTiming { name: tr.name.clone(), frames, encode_ms: enc_ns as f64 / 1e6, decode_ms: dec_ns as f64 / 1e6, x264_ms };
    Ok(TraceRun { encoder: enc.name(), rep, timing, csv, lat })
}

/// 跑全部 trace，组装 Report（不落盘）。
pub fn run_all(traces: &[Trace], p: &Params) -> Result<(Report, Timing, String), String> {
    let t0 = Instant::now();
    let (mut reps, mut tims, mut lat) = (Vec::new(), Vec::new(), Vec::new());
    let mut csv = String::from("trace,seq,t_ms,bytes,arrive_ms\n");
    let mut encoder = "";
    for tr in traces {
        let r = run_trace(tr, p)?;
        csv += &r.csv;
        encoder = r.encoder;
        lat.extend(r.lat);
        reps.push(r.rep);
        tims.push(r.timing);
    }
    let summary = summarize(&reps, lat);
    // 对照：同一批 trace、同一网络参数跑朴素基线（不跑 x264，不重复比对）
    let baseline = if p.encoder == "rd" && !p.shadow {
        let mut bp = p.clone(); bp.encoder = "naive".into(); bp.x264 = false;
        let (mut breps, mut blat) = (Vec::new(), Vec::new());
        for tr in traces { let r = run_trace(tr, &bp)?; blat.extend(r.lat); breps.push(r.rep); }
        let b = summarize(&breps, blat);
        Some(Baseline { encoder: "naive-zstd".into(), bytes_total: b.bytes_total, kb_per_min: b.kb_per_min, first_visit_p90_bytes: b.first_visit_p90_bytes,
            latency_p90_ms: b.latency.p90_ms, peak_window_bytes_1s: b.peak_window_bytes_1s })
    } else { None };
    let report = Report {
        schema: 1,
        encoder: encoder.into(),
        params: p.clone(), summary, traces: reps, placeholders: Placeholders::default(), baseline,
    };
    let timing = Timing {
        note: "墙钟测量，每次运行都可能不同；不属于 report.json",
        traces: tims, total_wall_ms: t0.elapsed().as_secs_f64() * 1000.0,
    };
    Ok((report, timing, csv))
}
