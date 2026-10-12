//! 契约红线 4「任何输入不 panic」的 catch_unwind 包裹（02 §7.3；TC-CONF-008）。
//!
//! zip crate 解析/解码畸形输入时可能 panic（`panic!`/`unreachable!`/索引越界等
//! 历史实例）；实现层在**全部读路径入口**包裹本模块，把 panic 一律转为
//! `Format{BadHeader}`（HZ-2004）返回，绝不让 unwind 逃出 `FormatHandler` 边界。
//!
//! 使用 `AssertUnwindSafe`（而非要求 `F: UnwindSafe`）：解码闭包内持 `Mutex` 守卫
//! 等非 `UnwindSafe` 借用，本模块只负责「把 panic 归一为错误」，不参与共享状态
//! 毒性判定——`Mutex` 中毒（poison）由各持锁点 `unwrap_or_else(into_inner)` 自行
//! 恢复，此处无须展开诊断。

use std::panic::AssertUnwindSafe;

use hzip_error::{FormatErrorKind, HzipError};

use crate::handler;

/// 包裹返回 `Result<T, HzipError>` 的闭包：panic → `Format{BadHeader}`，
/// 正常返回原样透传。
pub(crate) fn guard<T>(f: impl FnOnce() -> Result<T, HzipError>) -> Result<T, HzipError> {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => Err(badheader_of(payload)),
    }
}

/// 包裹返回普通值 `T` 的闭包（zip crate 解析返回 `ZipResult` 等非 HzipError 型）：
/// panic → `Format{BadHeader}`；正常返回 `Ok(T)`（映射由调用方继续）。
pub(crate) fn guard_ok<T>(f: impl FnOnce() -> T) -> Result<T, HzipError> {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Ok(v),
        Err(payload) => Err(badheader_of(payload)),
    }
}

fn badheader_of(payload: Box<dyn std::any::Any + Send>) -> HzipError {
    let msg = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
        .unwrap_or("(非字符串 panic)");
    tracing::warn!(panic = %msg, "zip 内部 panic 已拦截（红线 4）");
    HzipError::Format {
        format: handler::fmt(),
        kind: FormatErrorKind::BadHeader,
    }
}

#[cfg(test)]
#[path = "tests/panic_boundary.rs"]
mod tests;
