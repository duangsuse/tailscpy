//! rd：两端（B 的 .so、A 的 WASM）与 bench 共用的核心。
//!
//! 规则（doc/90-tasks.md §0 第 4 条）：**rd 里不读墙钟、不做 I/O**。时间从帧和输入的时间戳来，这是 bench 能重放的前提。
//! 区域 / 语义侦测放 `region.rs`，启发式 / 规则集合放 `heuristics.rs`（M2 起），各自独立成单文件。

/// 空壳版本号：构建链路（.so / wasm）冒烟用。
pub fn version() -> &'static str { concat!("rd ", env!("CARGO_PKG_VERSION")) }

/// 每行 hash（M0 只给 bench 的朴素基线和 host 的变化检测对账用；列掩码参数 M1 起加）。
/// FNV-1a 风格，对 RGBA 行字节。
pub fn row_hash(row: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in row { h = (h ^ b as u64).wrapping_mul(0x100000001b3); }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn version_nonempty() { assert!(version().starts_with("rd ")); }
    #[test] fn hash_differs() { assert_ne!(row_hash(&[1, 2, 3]), row_hash(&[1, 2, 4])); }
}
