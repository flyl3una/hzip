//! 过滤器 id 常量与「已交付 id」清单。
//!
//! T-STD 2026-10-08：自 `filter/mod.rs` 原样迁出（`mod.rs` 只留模块声明与再导出）；
//! 门面 `filter::{FILTER_BCJ, FILTER_BCJ2, supported_ids}` 由 `mod.rs` 再导出保持
//! 调用方不变。

use crate::traits::CodecId;

/// BCJ（x86 单流）过滤器 id。
pub const FILTER_BCJ: &str = "bcj";
/// BCJ2（四流）过滤器 id。
pub const FILTER_BCJ2: &str = "bcj2";

/// 本层已交付的过滤器 id（`bcj` 族其余：arm/armt/ppc/sparc 等声明 Unsupported，
/// 02 §4.9；TC-X-001 的 ppmd 语义同类）。
pub fn supported_ids() -> Vec<CodecId> {
    vec![CodecId::from(FILTER_BCJ), CodecId::from(FILTER_BCJ2)]
}
