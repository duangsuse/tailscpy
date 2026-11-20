//! bench：帧重放 + 朴素基线编码 + 网络模型 + 报表 + x264 对照（80 §6、§7）。
//!
//! 用法：`cargo run --release -p bench -- <trace目录>... [--bw 400KB/s] [--rtt 40ms] [--jitter 10ms] [--seed N] [--out bench/out]`
//! 免真机：加 `--sample` 用合成 trace。

mod encoder;
mod netmodel;
mod report;
mod run;
mod trace;
mod x264;

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "活镜评测：对录制的 trace 重放，输出体积 / 耗时 / 正确性报表")]
struct Cli {
    /// trace 目录（manifest.json、events.jsonl、frames.bin、frames.idx）
    dirs: Vec<PathBuf>,
    /// 用合成 trace（免真机）
    #[arg(long)]
    sample: bool,
    /// 下行带宽，如 400KB/s、3.2Mbps、500000（字节/秒）
    #[arg(long, default_value = "400KB/s", value_parser = parse_bw)]
    bw: u64,
    /// 往返延迟
    #[arg(long, default_value = "40ms", value_parser = parse_dur)]
    rtt: u64,
    /// 抖动上限（单向，均匀分布 0..jitter）
    #[arg(long, default_value = "10ms", value_parser = parse_dur)]
    jitter: u64,
    /// 抖动 PRNG 种子
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// 编码器：rd（M1，默认）或 naive（朴素基线）；rd 时报表里附带对朴素基线的对比
    #[arg(long, default_value = "rd", value_parser = ["rd", "naive"])]
    encoder: String,
    /// 朴素基线 zstd level
    #[arg(long, default_value_t = 3)]
    zstd: i32,
    /// 脏带合并：相隔不超过这么多行就并成一个矩形
    #[arg(long, default_value_t = 8)]
    merge_gap: usize,
    /// 用影子编码（只统计、不真正编码）代替朴素基线；不做逐像素比对
    #[arg(long)]
    shadow: bool,
    /// 比对时忽略的行带 y0-y1（闭区间，可重复），如走马灯区域
    #[arg(long, value_parser = parse_band)]
    ignore: Vec<(usize, usize)>,
    /// x264 码率封顶 kbps，默认等于 --bw
    #[arg(long)]
    x264_kbps: Option<u32>,
    /// x264 对照的固定取样帧率
    #[arg(long, default_value_t = 30)]
    x264_fps: u32,
    /// 不跑 x264 对照
    #[arg(long)]
    no_x264: bool,
    /// 报表根目录
    #[arg(long, default_value = "bench/out")]
    out: PathBuf,
    /// 报表目录名里的日期（YYYYMMDD）；默认读环境变量 BENCH_DATE，再默认今天
    #[arg(long)]
    date: Option<String>,
}

/// 带宽 → 字节/秒。KB = 1000。
fn parse_bw(s: &str) -> Result<u64, String> {
    let t = s.trim().to_lowercase().replace(' ', "");
    let (num, mul) = [("kb/s", 1e3), ("mb/s", 1e6), ("kbps", 125.0), ("mbps", 125e3), ("b/s", 1.0)].iter()
        .find_map(|(u, m)| t.strip_suffix(u).map(|n| (n.to_string(), *m))).unwrap_or((t.clone(), 1.0));
    let v: f64 = num.parse().map_err(|_| format!("带宽格式不对: {s}（例 400KB/s、3.2Mbps）"))?;
    if v <= 0.0 { return Err("带宽须为正".into()); }
    Ok((v * mul) as u64)
}

/// 时长 → 微秒。
fn parse_dur(s: &str) -> Result<u64, String> {
    let t = s.trim().to_lowercase();
    let (num, mul) = if let Some(n) = t.strip_suffix("ms") { (n, 1e3) } else if let Some(n) = t.strip_suffix('s') { (n, 1e6) } else { (t.as_str(), 1e3) };
    let v: f64 = num.trim().parse().map_err(|_| format!("时长格式不对: {s}（例 40ms、0.04s）"))?;
    if v < 0.0 { return Err("时长不能为负".into()); }
    Ok((v * mul) as u64)
}

fn parse_band(s: &str) -> Result<(usize, usize), String> {
    let (a, b) = s.split_once('-').ok_or("格式 y0-y1")?;
    let (a, b): (usize, usize) = (a.parse().map_err(|_| "y0 不是数字")?, b.parse().map_err(|_| "y1 不是数字")?);
    if a > b { return Err("y0 > y1".into()); }
    Ok((a, b))
}

fn git_short() -> String {
    std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|| "nogit".into())
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = real_main(cli) { eprintln!("bench: {e}"); std::process::exit(1); }
}

fn real_main(cli: Cli) -> Result<(), String> {
    let traces = if cli.sample { trace::sample() } else {
        if cli.dirs.is_empty() { return Err("没有 trace 目录；给目录或加 --sample".into()); }
        cli.dirs.iter().map(|d| trace::Trace::load(d)).collect::<Result<Vec<_>, _>>()?
    };
    let date = cli.date.or_else(|| std::env::var("BENCH_DATE").ok().filter(|s| !s.is_empty())).unwrap_or_else(report::today);
    let dir = cli.out.join(format!("{date}-{}", git_short()));
    std::fs::create_dir_all(&cli.out).map_err(|e| format!("创建 {}: {e}", cli.out.display()))?;
    let p = run::Params {
        bw: cli.bw, rtt_us: cli.rtt, jitter_us: cli.jitter, seed: cli.seed, zstd: cli.zstd, merge_gap: cli.merge_gap,
        ignore: cli.ignore, shadow: cli.shadow, encoder: cli.encoder.clone(), x264: !cli.no_x264,
        x264_kbps: cli.x264_kbps.unwrap_or((cli.bw * 8 / 1000).max(1) as u32), x264_fps: cli.x264_fps.max(1),
        tmp: cli.out.clone(),
    };
    let (rep, timing, csv) = run::run_all(&traces, &p)?;
    report::write_all(&dir, &cli.out, &rep, &timing, &csv)?;
    let s = &rep.summary;
    println!("{} 条 trace，{} 帧，下行 {:.1} KB，{:.1} KB/min；首访 p90 {:.1} KB（预算 90）；正确性 {}/{} 通过；x264 {} KB",
        s.traces, s.frames, s.bytes_total as f64 / 1000.0, s.kb_per_min, s.first_visit_p90_bytes as f64 / 1000.0,
        s.correctness_checks - s.correctness_failed, s.correctness_checks, s.x264_bytes_total as f64 / 1000.0);
    println!("报表：{}", dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(tmp: PathBuf) -> run::Params {
        run::Params { bw: 400_000, rtt_us: 40_000, jitter_us: 10_000, seed: 1, zstd: 3, merge_gap: 8, ignore: vec![],
            shadow: false, encoder: "rd".into(), x264: false, x264_kbps: 3200, x264_fps: 30, tmp }
    }

    #[test]
    fn parse_units() {
        assert_eq!(parse_bw("400KB/s").unwrap(), 400_000);
        assert_eq!(parse_bw("3.2Mbps").unwrap(), 400_000);
        assert_eq!(parse_bw("500000").unwrap(), 500_000);
        assert_eq!(parse_dur("40ms").unwrap(), 40_000);
        assert_eq!(parse_dur("0.5s").unwrap(), 500_000);
        assert_eq!(parse_band("834-878").unwrap(), (834, 878));
        assert!(parse_bw("x").is_err());
    }

    #[test]
    fn report_json_byte_identical_and_correct() {
        let tr = vec![trace::synth("a", 64, 96), trace::synth("b", 48, 64)];
        let p = params(std::env::temp_dir());
        let json = || {
            let (r, _, _) = run::run_all(&tr, &p).unwrap();
            assert_eq!(r.summary.correctness_failed, 0);
            assert!(r.summary.correctness_checks >= 6);
            serde_json::to_string_pretty(&r).unwrap()
        };
        let (a, b) = (json(), json());
        assert_eq!(a, b);
        assert!(a.contains("\"oracle_dedupe_bytes\": null"));
        assert!(!a.contains("timing") && !a.contains("encode_ms"));
    }

    #[test]
    fn classes_and_latency() {
        let (r, _, _) = run::run_all(&[trace::synth("c", 64, 96)], &params(std::env::temp_dir())).unwrap();
        let c: Vec<u64> = r.summary.classes.iter().map(|c| c.bytes).collect();
        assert!(c[0] > 0 && c[2] > 0 && c[4] > 0, "{c:?}");
        assert_eq!((c[1], c[3]), (0, 0));
        assert_eq!(c.iter().sum::<u64>(), r.summary.bytes_total);
        assert_eq!(r.traces[0].steps, 5);
        assert!(r.summary.latency.n >= 4 && r.summary.latency.p50_ms.unwrap() > 20.0);
    }

    #[test]
    fn shadow_skips_correctness() {
        let mut p = params(std::env::temp_dir());
        p.shadow = true;
        let (r, _, _) = run::run_all(&[trace::synth("s", 64, 96)], &p).unwrap();
        assert!(!r.traces[0].correctness.checked && r.summary.bytes_total > 0);
    }

    #[test]
    fn end_to_end_from_disk_and_prev_compare() {
        let root = std::env::temp_dir().join(format!("bench-test-{}-e2e", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tdir = root.join("tr");
        trace::write_dir(&trace::synth("disk", 64, 96), &tdir);
        let tr = vec![trace::Trace::load(&tdir).unwrap()];
        let p = params(root.clone());
        let (r, t, csv) = run::run_all(&tr, &p).unwrap();
        let (d1, d2) = (root.join("out/20000101-aaa"), root.join("out/20000102-bbb"));
        report::write_all(&d1, &root.join("out"), &r, &t, &csv).unwrap();
        assert!(std::fs::read_to_string(d1.join("report.md")).unwrap().contains("无上一份"));
        report::write_all(&d2, &root.join("out"), &r, &t, &csv).unwrap();
        assert!(std::fs::read_to_string(d2.join("report.md")).unwrap().contains("上一份：`20000101-aaa`"));
        assert_eq!(std::fs::read(d1.join("report.json")).unwrap(), std::fs::read(d2.join("report.json")).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn x264_reproducible_when_ffmpeg_present() {
        if !x264::available() { return; }
        let tr = trace::synth("x", 64, 96);
        let go = || x264::run(&tr, 400, 30, &[], &[1000, 3000, 8500], &std::env::temp_dir()).unwrap();
        let (a, b) = (go(), go());
        assert_eq!(a.status, "ok");
        assert!(a.bytes > 0 && a.checks == 3 && a.px > 0);
        assert_eq!((a.bytes, a.px_diff), (b.bytes, b.px_diff));
    }
}
