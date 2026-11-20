//! 区域 / 语义侦测（M1 只有列掩码；页面与区域侦测在 M2 加进这里）。
use crate::hash::Mask;

/// 区域的有效列：掩掉右缘滚动条窄带 `sb_w`，以及压在滚动区之上的 `Fixed` 浮层所占列（`occluders` 为列区间）。
/// M1 整屏一个 `Fixed` 区域，`sb_w = 0`、无浮层 → 整行有效。
pub fn column_mask(w: u32, sb_w: u32, occluders: &[(u32, u32)]) -> Mask {
    let mut m: Vec<(u32, u32)> = occluders.to_vec();
    if sb_w > 0 { m.push((w.saturating_sub(sb_w), w)) }
    Mask::excluding(w, &m)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn m1_default_is_full_row() { assert_eq!(column_mask(1080, 0, &[]).ranges, vec![(0, 1080)]); }
    #[test] fn sb_and_fab_are_masked() { assert_eq!(column_mask(100, 8, &[(40, 60)]).ranges, vec![(0, 40), (60, 92)]); }
}
