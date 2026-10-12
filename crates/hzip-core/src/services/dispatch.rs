//! `JobKind` → 服务实现的分派（02 §9.3 worker 主循环 L 节点）。
//!
//! T-STD 2026-10-08：自 `services/mod.rs` 原样迁出（`mod.rs` 只留模块声明与
//! 再导出）；门面 `services::execute` 由 `mod.rs` 再导出保持调用方不变。

use hzip_progress::Outcome;

use super::{compress, convert, edit, extract};
use crate::jobs::{JobCtx, JobKind, JobSpec};

/// 分派：JobKind → 各服务（02 §9.3 worker 主循环 L 节点）。
///
/// Extract/Test 臂 T32、Compress/Convert/EditSave 臂 T33 接真实现。
/// browse/preview（T34）非 JobKind（02 §9.10 同步轻量），不在此分派。
pub(crate) fn execute(kind: &JobKind, ctx: &JobCtx, spec: &JobSpec) -> Outcome {
    match kind {
        JobKind::Extract => extract::run_extract(ctx, spec),
        JobKind::Test => extract::run_test(ctx, spec),
        JobKind::Compress => compress::run_compress(ctx, spec),
        JobKind::Convert => convert::run_convert(ctx, spec),
        JobKind::EditSave => edit::run_edit_save(ctx, spec),
        JobKind::EditBatch => edit::run_edit_batch(ctx, spec),
    }
}
