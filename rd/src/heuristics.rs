//! 启发式 / 规则集合（M2 起）：位移检测（稀有度加权投票 + 已知预期位移验证）、滚轮因子估计。
//! 所有阈值由调用方从参数表传入；这里不读墙钟、不做 I/O。
use std::collections::HashMap;

/// 位移检测结果。约定：`cur[r] == prev[r + dy]`（dy > 0 = 内容上移 = 往下滚）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shift { pub dy: i32, pub matched: u32, pub total: u32 }

#[derive(Clone, Copy, Debug)]
pub struct ShiftCfg {
    /// 重叠的有信息量行里，至少这个比例对得上才认
    pub min_ratio: f64,
    /// 至少这么多行对得上
    pub min_rows: u32,
    /// 同一个 hash 在上一帧出现超过这么多次就不投票（重复图案 / 分隔线）
    pub max_dup: usize,
    /// 第一名票数须是第二名的这么多倍
    pub margin: f64,
    /// 预期位移附近尝试的半径
    pub near: i32,
}
impl Default for ShiftCfg { fn default() -> Self { ShiftCfg { min_ratio: 0.6, min_rows: 8, max_dup: 8, margin: 1.5, near: 3 } } }

fn score(prev: &[u64], cur: &[u64], info: &[bool], dy: i32) -> (u32, u32) {
    let (mut m, mut t) = (0u32, 0u32);
    for r in 0..cur.len() as i32 {
        let p = r + dy;
        if p < 0 || p >= prev.len() as i32 || !info[r as usize] { continue }
        t += 1;
        if cur[r as usize] == prev[p as usize] { m += 1 }
    }
    (m, t)
}

/// 检测 `cur` 相对 `prev` 的竖直位移（两者都是滚动区内逐行 hash，等长）。
/// `info[r]`：cur 第 r 行有信息量（非纯色行）——纯色行和周期性分隔线会产生大量假匹配，不拿它们当证据（40 §2）。
/// `expected`：刚注入了滚动时的预期位移，先验证它附近的候选；否则 / 不成立时投票。
/// 返回 None = 没有位移（或认不出来）。
pub fn detect_shift(prev: &[u64], cur: &[u64], info: &[bool], expected: Option<i32>, cfg: &ShiftCfg) -> Option<Shift> {
    let n = cur.len().min(prev.len());
    if n < 4 { return None }
    let (prev, cur, info) = (&prev[..n], &cur[..n], &info[..n]);
    let ok = |m: u32, t: u32| t > 0 && m >= cfg.min_rows && m as f64 >= cfg.min_ratio * t as f64;
    // 1. 已知预期位移：验证它附近的候选，取匹配最多的
    if let Some(e) = expected {
        let mut best: Option<Shift> = None;
        for d in e - cfg.near..=e + cfg.near {
            if d == 0 || d.unsigned_abs() as usize >= n { continue }
            let (m, t) = score(prev, cur, info, d);
            if ok(m, t) && best.map_or(true, |b| m > b.matched) { best = Some(Shift { dy: d, matched: m, total: t }) }
        }
        if best.is_some() { return best }
    }
    // 2. 投票：每个有信息量的行，在上一帧里找 hash 相同的行，给 dy 投票，权重 = 1 / 该 hash 在上一帧的出现次数（稀有度）
    let mut pos: HashMap<u64, Vec<u32>> = HashMap::new();
    for (i, &h) in prev.iter().enumerate() { pos.entry(h).or_default().push(i as u32) }
    let mut votes: HashMap<i32, f64> = HashMap::new();
    for r in 0..n {
        if !info[r] { continue }
        let Some(ps) = pos.get(&cur[r]) else { continue };
        if ps.len() > cfg.max_dup { continue }
        let w = 1.0 / ps.len() as f64;
        for &p in ps { let d = p as i32 - r as i32; if d != 0 { *votes.entry(d).or_insert(0.0) += w } }
    }
    let mut v: Vec<(i32, f64)> = votes.into_iter().collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.abs().cmp(&b.0.abs())));
    let (d1, s1) = *v.first()?;
    if let Some(&(_, s2)) = v.get(1) { if s1 < cfg.margin * s2 { return None } }
    let (m, t) = score(prev, cur, info, d1);
    if ok(m, t) { Some(Shift { dy: d1, matched: m, total: t }) } else { None }
}

/// 滚轮因子（像素 / 格）的估计：按“请求了 `notches` 格、实测位移 `dy` 像素”更新。
/// 位移明显小于预期按“到边了”处理，不更新；返回新的估计。
pub fn update_wheel_factor(f_est: f64, notches: f64, requested_px: f64, dy: f64) -> f64 {
    if notches.abs() < 1e-6 || dy.abs() < 4.0 { return f_est }
    if dy.abs() < 0.5 * requested_px.abs() { return f_est }
    let f = dy.abs() / notches.abs();
    if f_est <= 0.0 { f } else { 0.5 * f_est + 0.5 * f }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows(n: usize, seed: u64) -> Vec<u64> { (0..n as u64).map(|i| (i + seed).wrapping_mul(0x9E3779B97F4A7C15) ^ 0x55).collect() }

    #[test] fn finds_scroll_down_by_voting() {
        let all = rows(500, 1); let prev = &all[100..300]; let cur = &all[137..337];   // cur[r] == prev[r+37]
        let s = detect_shift(prev, cur, &vec![true; 200], None, &ShiftCfg::default()).unwrap();
        assert_eq!(s.dy, 37); assert_eq!(s.matched, 163);
    }
    #[test] fn finds_scroll_up_negative_dy() {
        let all = rows(500, 1); let prev = &all[200..400]; let cur = &all[180..380];
        assert_eq!(detect_shift(prev, cur, &vec![true; 200], None, &ShiftCfg::default()).unwrap().dy, -20);
    }
    #[test] fn no_shift_when_content_unrelated() {
        let (a, b) = (rows(200, 1), rows(200, 99999));
        assert!(detect_shift(&a, &b, &vec![true; 200], None, &ShiftCfg::default()).is_none());
    }
    #[test] fn expected_shift_is_verified_and_tolerates_off_by_one() {
        let all = rows(500, 1); let prev = &all[100..300]; let cur = &all[140..340];
        let s = detect_shift(prev, cur, &vec![true; 200], Some(41), &ShiftCfg::default()).unwrap();
        assert_eq!(s.dy, 40);
    }
    #[test] fn uniform_rows_are_not_evidence() {
        // 上一帧和这一帧大部分是纯色行：不能因为纯色行相同就认位移
        let mut prev = vec![7u64; 200]; let mut cur = vec![7u64; 200];
        for i in 0..6 { prev[50 + i] = 1000 + i as u64; cur[40 + i] = 1000 + i as u64 }
        let mut info = vec![false; 200]; for i in 0..6 { info[40 + i] = true }
        assert!(detect_shift(&prev, &cur, &info, None, &ShiftCfg { min_rows: 8, ..Default::default() }).is_none());
    }
    #[test] fn periodic_pattern_is_ambiguous() {
        // 周期 10 的重复图案：多个 dy 得票接近 → 投票不成立（有预期位移时走验证路径）
        let a: Vec<u64> = (0..200).map(|i| (i % 10) as u64 + 5).collect();
        assert!(detect_shift(&a, &a, &vec![true; 200], None, &ShiftCfg::default()).is_none());
    }
    #[test] fn partial_overlap_with_fixed_header_rows() {
        // 上面 30 行是不动的标题，其余整体滚 25 行：多数行对得上就认
        let all = rows(500, 3); let mut prev = all[100..300].to_vec(); let mut cur = all[125..325].to_vec();
        for i in 0..30 { prev[i] = 9000 + i as u64; cur[i] = 9000 + i as u64 }
        let s = detect_shift(&prev, &cur, &vec![true; 200], None, &ShiftCfg::default());
        assert_eq!(s.map(|x| x.dy), Some(25));
    }
    #[test] fn wheel_factor_learns_and_ignores_edge() {
        let f = update_wheel_factor(168.0, 1.0, 168.0, 357.0); assert!(f > 250.0 && f < 270.0);
        assert_eq!(update_wheel_factor(168.0, 1.0, 168.0, 10.0), 168.0);   // 到边：位移远小于预期
    }
}
