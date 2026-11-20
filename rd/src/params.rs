//! 参数表（50 §6）。B 发布，A 的 l 面板照表画控件。阈值与开关一律进这里，不写死在代码里。
use crate::wire::{Param, Scope};
use std::collections::BTreeMap;

fn p(key: &str, label: &str, group: &str, val: f64, min: f64, max: f64, step: f64) -> Param {
    Param { key: key.into(), label: label.into(), group: group.into(), val, min, max, step, scope: Scope::Global }
}

/// M1 用到的参数（其余参数随对应功能在 M2 起加入）
pub fn defaults() -> Vec<Param> {
    vec![
        p("zstd_level", "zstd 压缩级别", "编码", 3.0, 1.0, 19.0, 1.0),
        p("img_lossy", "图片有损", "图片", 1.0, 0.0, 1.0, 1.0),
        p("img_q", "JPEG 质量", "图片", 70.0, 10.0, 95.0, 1.0),
        p("img_min_colors", "判为照片的最少颜色数", "图片", 400.0, 16.0, 5000.0, 16.0),
        p("img_min_area", "图片最小面积(px²)", "图片", 3600.0, 256.0, 1000000.0, 256.0),
        p("img_defer_ms", "疑似图片等 A11y 确认的上限(ms)", "图片", 1000.0, 0.0, 3000.0, 50.0),
        p("strip_rows", "条带行数", "编码", 64.0, 8.0, 256.0, 8.0),
        p("sb_w", "右缘滚动条窄带宽度(px)", "区域", 0.0, 0.0, 64.0, 1.0),
        p("verify", "附带校验", "调试", 0.0, 0.0, 1.0, 1.0),
        p("coalesce_ms", "合并等待：画面安静多久再编(ms)", "链路", 40.0, 0.0, 500.0, 5.0),
        p("coalesce_max_ms", "合并等待上限(ms)", "链路", 200.0, 0.0, 2000.0, 10.0),
        p("inflight_kb", "在途未确认字节上限(KB)", "链路", 1024.0, 64.0, 16384.0, 64.0),
        p("detach_keep_s", "断线后保留会话(秒)", "链路", 60.0, 5.0, 3600.0, 5.0),
    ]
}

/// 当前取值（全局覆盖）。M1 只有全局一层；App / 页覆盖随 M2 的页面一起做。
#[derive(Clone, Debug, Default)]
pub struct Params { vals: BTreeMap<String, f64> }

impl Params {
    pub fn new() -> Params { let mut v = BTreeMap::new(); for q in defaults() { v.insert(q.key, q.val); } Params { vals: v } }
    pub fn get(&self, k: &str) -> f64 { *self.vals.get(k).unwrap_or(&0.0) }
    /// 返回是否接受（键存在且值在范围内会被夹到范围）
    pub fn set(&mut self, k: &str, v: f64) -> bool {
        match defaults().into_iter().find(|q| q.key == k) { Some(q) => { self.vals.insert(k.into(), v.clamp(q.min, q.max)); true } None => false }
    }
    pub fn table(&self) -> Vec<Param> { defaults().into_iter().map(|mut q| { q.val = self.get(&q.key); q }).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn set_clamps_and_rejects_unknown() {
        let mut p = Params::new();
        assert!(p.set("img_q", 200.0)); assert_eq!(p.get("img_q"), 95.0);
        assert!(!p.set("nope", 1.0));
        assert_eq!(p.table().iter().find(|q| q.key == "img_q").unwrap().val, 95.0);
    }
}
