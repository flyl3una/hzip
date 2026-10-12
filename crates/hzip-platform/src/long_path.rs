//! Windows 长路径前缀处理（02 §5.3.1；TC-WIN-001 / TC-SEC-010）。
//!
//! 平台差异：
//!
//! | 平台 | `to_extended` | `from_extended` | `max_path_len` |
//! |---|---|---|---|
//! | Windows | `\\?\` 注入（含 `\\?\UNC\`），feature `win-long-path` 关闭时恒等 | `\\?\UNC\` → `\\`；`\\?\` 剥离 | 32767（feature on）/ 260（off） |
//! | Unix | 恒等 | 恒等 | `pathconf(_PC_PATH_MAX)`，失败回退 4096 |
//!
//! 纪律（02 §5.3.1）：注入只发生在「即将传给文件系统 API」的边界
//! （open/create/rename/metadata 调用点，由 fmt 层负责）；显示、列表输出、日志一律
//! 经 [`from_extended`] 还原。本模块不在通用路径转换中全局注入。

use std::path::{Path, PathBuf};

/// 将路径转换为 Windows 扩展长度前缀形式（02 §5.3.1）。
///
/// - **Windows**：已 `\\?\` 开头 → 原样返回（幂等）；UNC（`\\server\share\...`）→
///   `\\?\UNC\server\share\...`；盘符绝对路径 → `\\?\C:\...`；相对路径 → 原样返回
///   （由调用方保证绝对，见 02 §5.3.1）。注入前校验：路径不含内嵌 NUL、注入后总长
///   ≤ 32767；不满足时原样返回（由上层文件系统 API 明确报错，TC-SEC-010 off 语义）。
///   无 `\\?\` 时每段仍受 255 组件限制——本函数不绕过组件限制，只解除 260 总长限制。
/// - **Unix**：恒等返回。
/// - feature `win-long-path`（02 §2.2）默认关闭时恒等返回（TC-SEC-010 off 路径
///   「明确报错不截断」由上层配合）。
#[must_use]
pub fn to_extended(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if !cfg!(feature = "win-long-path") {
            return p.to_path_buf();
        }
        let os = p.as_os_str();
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide: Vec<u16> = os.encode_wide().collect();
        const BS: u16 = b'\\' as u16;
        const Q: u16 = b'?' as u16;
        const COLON: u16 = b':' as u16;
        // 已扩展：原样返回（幂等）
        if wide.len() >= 4 && wide[0] == BS && wide[1] == BS && wide[2] == Q && wide[3] == BS {
            return p.to_path_buf();
        }
        // 校验：不含内嵌 NUL、注入后总长 ≤ 32767
        if wide.contains(&0) || wide.len() + 4 > 32767 {
            return p.to_path_buf();
        }
        // UNC：\\server\share\... → \\?\UNC\server\share\...
        if wide.len() >= 2 && wide[0] == BS && wide[1] == BS {
            let mut out = Vec::with_capacity(wide.len() + 8);
            out.extend_from_slice(&[BS, BS, Q, BS, b'U' as u16, b'N' as u16, b'C' as u16, BS]);
            out.extend_from_slice(&wide[2..]);
            return PathBuf::from(std::ffi::OsString::from_wide(&out));
        }
        // 盘符绝对路径：C:\... → \\?\C:\...
        if wide.len() >= 3
            && (wide[0] as u8).is_ascii_alphabetic()
            && wide[1] == COLON
            && (wide[2] == BS || wide[2] == b'/' as u16)
        {
            let mut out = Vec::with_capacity(wide.len() + 4);
            out.extend_from_slice(&[BS, BS, Q, BS]);
            out.extend_from_slice(&wide);
            return PathBuf::from(std::ffi::OsString::from_wide(&out));
        }
        p.to_path_buf() // 相对路径：原样返回（调用方保证绝对）
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        p.to_path_buf()
    }
}

/// 还原扩展长度前缀，用于显示与日志（02 §5.3.1；TC-WIN-001「列表/日志无前缀污染」）。
///
/// - **Windows**：`\\?\UNC\` 前缀 → `\\`；`\\?\` 前缀剥离；其余原样。
/// - **Unix**：恒等返回。
#[must_use]
pub fn from_extended(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let os = p.as_os_str();
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide: Vec<u16> = os.encode_wide().collect();
        const BS: u16 = b'\\' as u16;
        const Q: u16 = b'?' as u16;
        // \\?\UNC\... → \\...
        let unc = [BS, BS, Q, BS, b'U' as u16, b'N' as u16, b'C' as u16, BS];
        if wide.len() >= unc.len() && wide[..unc.len()] == unc {
            let mut out = Vec::with_capacity(wide.len() - 6);
            out.extend_from_slice(&[BS, BS]);
            out.extend_from_slice(&wide[unc.len()..]);
            return PathBuf::from(std::ffi::OsString::from_wide(&out));
        }
        // \\?\... → ...
        if wide.len() >= 4 && wide[0] == BS && wide[1] == BS && wide[2] == Q && wide[3] == BS {
            return PathBuf::from(std::ffi::OsString::from_wide(&wide[4..]));
        }
        p.to_path_buf()
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        p.to_path_buf()
    }
}

/// 路径总长上限（02 §5.3.1；TC-SEC-010）。
///
/// - **Windows**：feature `win-long-path` 开启时 32767，关闭时 260（TC-SEC-010 两层行为）。
/// - **Unix**：`libc::pathconf(_PC_PATH_MAX)`（以 `/` 查询），失败回退 4096。
#[must_use]
pub fn max_path_len() -> usize {
    #[cfg(windows)]
    {
        if cfg!(feature = "win-long-path") {
            32767
        } else {
            260
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let c = std::ffi::CString::new("/").unwrap_or_default();
        let v = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_PATH_MAX) };
        if v > 0 { v as usize } else { 4096 }
    }
    #[cfg(target_os = "macos")]
    {
        let c = std::ffi::CString::new("/").unwrap_or_default();
        let v = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_PATH_MAX) };
        if v > 0 { v as usize } else { 4096 }
    }
}

#[cfg(test)]
#[path = "tests/long_path.rs"]
mod tests;
