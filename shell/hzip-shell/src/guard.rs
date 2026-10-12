//! FFI 边界守卫。
//!
//! # 为什么必须有
//!
//! 本 DLL 以**进程内**方式载入 `explorer.exe`（经典路径的 `InProcServer32`）或
//! `dllhost.exe`（稀疏包的 `SurrogateServer`）。任何从 Rust 逃逸到 COM 边界的
//! panic 都是未定义行为，最轻的表现是资源管理器崩溃——用户会失去整个桌面外壳。
//!
//! 两道防线：
//!
//! 1. `Cargo.toml` 固定 `panic = "unwind"`（**不可用 `abort`**：abort 会直接
//!    终结宿主进程，连兜底机会都没有）；
//! 2. 本模块的 [`guard`] / [`guard_hr`] 把每个导出的方法体包进 `catch_unwind`，
//!    panic 一律转成 `E_FAIL` / 静默降级。
//!
//! # 静默原则
//!
//! `GetState` 与 `EnumSubCommands` 失败时**不报错，只隐藏菜单**：菜单本身是旁路
//! 功能，让它出错去打扰 Explorer 的 UI 路径（进而是用户）是得不偿失的。

use std::panic::{catch_unwind, AssertUnwindSafe};
use windows::core::HRESULT;
use windows::Win32::Foundation::E_FAIL;

/// 执行 `f`，把 panic 收敛成 `HRESULT`。
///
/// `Ok(hr)` → 返回该 `HRESULT`；`Err(panic)` → 返回 [`E_FAIL`]。
pub fn guard_hr<F>(what: &str, f: F) -> HRESULT
where
    F: FnOnce() -> HRESULT,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(hr) => hr,
        Err(_) => {
            log_err(what, "Rust panic 已被 FFI 边界拦截");
            E_FAIL
        }
    }
}

/// 执行返回值类型的 `f`，panic → `Err(E_FAIL)`（供返回 `Result` 的 COM 方法用）。
///
/// # Safety 语义
///
/// 仅收敛 panic，不改变返回值语义；`out` 指针的写入仍由调用方负责。
pub fn guard_out<T, F>(what: &str, f: F) -> windows::core::Result<T>
where
    F: FnOnce() -> windows::core::Result<T>,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => {
            log_err(what, "Rust panic 已被 FFI 边界拦截");
            Err(windows::core::Error::from(E_FAIL))
        }
    }
}

/// 写日志（**不依赖 hzip-log**：本 crate 与主 workspace 零代码依赖）。
///
/// 用 `OutputDebugStringW`——DebugView / VS 可见，且不会像文件日志那样在
/// Explorer 进程里持有句柄。生产诊断主要靠注册表与配置文件回读。
pub fn log_err(what: &str, msg: &str) {
    let line = format!("[hzip-shell] {what}: {msg}\n");
    let wide: Vec<u16> = line.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: wide 以 NUL 结尾且在本调用期间存活
    unsafe {
        windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(windows::core::PCWSTR(
            wide.as_ptr(),
        ));
    }
}
