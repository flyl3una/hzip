//! Windows 宽字符串工具（仅 `cfg(windows)` 编译）。
//!
//! 所有传给 Win32 API 的 UTF-16 字符串必须带 NUL 终止符；`encode_wide().collect()`
//! 不自动追加，需经本模块 helper 统一处理（避免堆上读越界）。

use std::path::Path;

/// `&Path` → 带 NUL 终止符的 UTF-16 缓冲区（用于 `PCWSTR` 参数）。
pub fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// `&str` → 带 NUL 终止符的 UTF-16 缓冲区（用于 verb 等 `PCWSTR` 参数）。
pub fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
