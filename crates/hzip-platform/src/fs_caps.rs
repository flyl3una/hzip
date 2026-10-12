//! 文件系统能力探测（02 §5.1 / §5.3.4；TC-SEC-011 / TC-LIN-006 / TC-MAC-004 支撑）。
//!
//! 结果供 core 冲突检测（A.txt vs a.txt，02 §9.8）与净化（保留字符集）使用。

use std::path::Path;

use crate::fs_util;
use crate::long_path;

/// 文件系统能力（02 §5.1）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsCaps {
    /// 大小写敏感性（实测，非按 FS 类型推断；02 §5.3.4）
    pub case_sensitive: bool,
    /// 路径保留字符集（禁止出现在文件名中；02 §5.1）
    pub reserved_chars: &'static str,
    /// 路径总长上限（win 260/32767 · unix pathconf）
    pub max_path_len: usize,
    /// 单段（组件）长度上限（win 255 · unix pathconf）
    pub max_component_len: usize,
}

/// 探测目录所在文件系统的能力（02 §5.1）。
///
/// - `case_sensitive`：复用 [`fs_util::is_case_sensitive`] 实测（对撞文件）；
/// - `reserved_chars`：win `"<>:\"|?*"` + 控制字符 1–31（NTFS 语义层）；unix `"/"` + NUL；
/// - `max_path_len` / `max_component_len`：win 260（或开 `win-long-path` 后 32767）/ 255；
///   unix `pathconf(_PC_PATH_MAX)` / `pathconf(_PC_NAME_MAX)`。
pub fn probe(dir: &Path) -> FsCaps {
    FsCaps {
        case_sensitive: fs_util::is_case_sensitive(dir),
        reserved_chars: reserved_chars(),
        max_path_len: long_path::max_path_len(),
        max_component_len: max_component_len(),
    }
}

/// 保留字符集（02 §5.1：win NTFS 语义层 / unix 语义层）。
fn reserved_chars() -> &'static str {
    #[cfg(windows)]
    {
        // NTFS 语义层：<>:"|?* + 控制字符 1–31（NUL 由路径 API 层拒绝，不在此表）
        "<>:\"|?*"
    }
    #[cfg(not(windows))]
    {
        // unix：/ 与 NUL
        "/"
    }
}

/// 单段长度上限。
fn max_component_len() -> usize {
    #[cfg(windows)]
    {
        255
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let c = std::ffi::CString::new("/").unwrap_or_default();
        let v = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_NAME_MAX) };
        if v > 0 { v as usize } else { 255 }
    }
    #[cfg(target_os = "macos")]
    {
        let c = std::ffi::CString::new("/").unwrap_or_default();
        let v = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_NAME_MAX) };
        if v > 0 { v as usize } else { 255 }
    }
}

#[cfg(test)]
#[path = "tests/fs_caps.rs"]
mod tests;
