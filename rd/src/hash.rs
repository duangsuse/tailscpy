//! 行 hash（40 §2）。**带列掩码**：只对有效列区间计算，被掩掉的列（右缘滚动条窄带、压在滚动区上的浮层）不进 hash。
//! 两端（B 的 .so、A 的 wasm）共用这一个定义，`Verify` 才对得上。

/// 有效列区间（半开，升序、不重叠）。
#[derive(Clone, Debug, PartialEq)]
pub struct Mask { pub ranges: Vec<(u32, u32)> }

impl Mask {
    pub fn full(w: u32) -> Mask { Mask { ranges: vec![(0, w)] } }
    /// 从“被掩掉的列区间”求有效区间
    pub fn excluding(w: u32, masked: &[(u32, u32)]) -> Mask {
        let mut m: Vec<(u32, u32)> = masked.iter().map(|&(a, b)| (a.min(w), b.min(w))).filter(|&(a, b)| a < b).collect();
        m.sort();
        let mut out = vec![]; let mut x = 0;
        for (a, b) in m { if a > x { out.push((x, a)) } x = x.max(b) }
        if x < w { out.push((x, w)) }
        Mask { ranges: out }
    }
}

const K: u64 = 0x9E37_79B9_7F4A_7C15;

#[inline] fn mix(h: u64, v: u64) -> u64 { let h = (h ^ v).wrapping_mul(K); h ^ (h >> 29) }

/// `row` 是一行 RGBA（w*4 字节）。
pub fn row_hash_masked(row: &[u8], mask: &Mask) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &(a, b) in &mask.ranges {
        let s = &row[a as usize * 4..b as usize * 4];
        let mut it = s.chunks_exact(8);
        for c in &mut it { h = mix(h, u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])) }
        let r = it.remainder();
        if r.len() == 4 { h = mix(h, u32::from_le_bytes([r[0], r[1], r[2], r[3]]) as u64) }
        h = mix(h, (b - a) as u64);   // 区间边界进 hash，避免相邻区间拼接后碰撞
    }
    h
}

/// 一段行 hash 的折叠（`Verify.fold`）
pub fn fold(hs: &[u64]) -> u64 { hs.iter().fold(0x1234_5678_9abc_def0u64, |a, &h| a.rotate_left(7) ^ h.wrapping_mul(K)) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn mask_ignores_masked_columns() {
        let m = Mask::excluding(8, &[(6, 8)]);
        assert_eq!(m.ranges, vec![(0, 6)]);
        let mut a = vec![7u8; 32]; let b = a.clone();
        a[28] = 99;   // 第 7 列
        assert_eq!(row_hash_masked(&a, &m), row_hash_masked(&b, &m));
        a[4] = 1;     // 第 1 列
        assert_ne!(row_hash_masked(&a, &m), row_hash_masked(&b, &m));
    }
    #[test] fn excluding_merges_and_clips() {
        assert_eq!(Mask::excluding(10, &[(8, 20), (2, 4), (3, 5)]).ranges, vec![(0, 2), (5, 8)]);
        assert_eq!(Mask::excluding(10, &[]).ranges, vec![(0, 10)]);
    }
}
