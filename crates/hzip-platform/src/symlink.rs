//! 符号链接/硬链接创建、读取、类型判定（02 §5.3.3；TC-SEC-009 支撑）。
//!
//! 平台差异：
//!
//! | 平台 | 创建 | 降级 |
//! |---|---|---|
//! | Windows | `CreateSymbolicLinkW`（需特权/开发者模式） | 特权错误 → 按 [`SymlinkPolicy`] 降级复制或拒绝 |
//! | Unix | `libc::symlink` / `libc::link` | 无（失败返回 Io） |
//!
//! 降级判定只认 `ERROR_PRIVILEGE_NOT_HELD`，其他错误（目标不存在等）按 Io 上抛，
//! 不得误降级（02 §5.3.3）。

use std::io;
use std::path::{Path, PathBuf};

use hzip_error::HzipError;

/// 符号链接创建失败（特权不足）时的策略（02 §5.3.3 降级链）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymlinkPolicy {
    /// 降级方式
    pub fallback: Fallback,
}

/// 降级方式（02 §5.3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// 复制目标内容为普通文件 + warning（`SymlinkOutcome::DegradedToCopy`）
    Copy,
    /// 拒绝并提示开启开发者模式（`Err(HzipError::Unsupported)`）
    Reject,
}

/// 创建结果（02 §5.3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkOutcome {
    /// 真实符号链接已创建
    Created,
    /// 特权不足，按 `Fallback::Copy` 降级为普通文件复制
    DegradedToCopy,
}

/// 路径的链接类型判定结果（02 §5.3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// 符号链接，目标是文件
    File,
    /// 符号链接，目标是目录
    Dir,
    /// 符号链接，目标不存在（悬空）
    Broken,
    /// 非符号链接
    NotLink,
}

/// 创建符号链接（02 §5.3.3 降级链）。
///
/// - **Windows**：`CreateSymbolicLinkW`，flags 带 `SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE`
///   （目录目标加 `SYMBOLIC_LINK_FLAG_DIRECTORY`）。失败 `ERROR_PRIVILEGE_NOT_HELD` →
///   按策略分支（[`Fallback::Copy`] 复制内容为普通文件 + warning /
///   [`Fallback::Reject`] 返回 `Err(Unsupported)` 附「开启开发者模式」提示）。
///   其他错误按 Io 上抛，不误降级。
/// - **Unix**：`libc::symlink`，失败返回 Io（不降级）。
pub fn create_symlink(
    target: &Path,
    link: &Path,
    pol: &SymlinkPolicy,
) -> Result<SymlinkOutcome, HzipError> {
    #[cfg(windows)]
    {
        create_symlink_win(target, link, pol)
    }
    #[cfg(not(windows))]
    {
        let _ = pol;
        let t = cstr(target)?;
        let l = cstr(link)?;
        // SAFETY: 两个 CString 均无内嵌 NUL，指针在调用期间有效。
        let r = unsafe { libc::symlink(t.as_ptr(), l.as_ptr()) };
        if r != 0 {
            return Err(HzipError::from(io::Error::last_os_error()));
        }
        Ok(SymlinkOutcome::Created)
    }
}

/// 创建硬链接（02 §5.3.3；unix 失败返回 Io）。
pub fn create_hardlink(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::CreateHardLinkW;
        let t: Vec<u16> = crate::winutil::wide_path(target);
        let l: Vec<u16> = crate::winutil::wide_path(link);
        // SAFETY: 宽字符串 NUL 终止；security attributes 可空。
        let r = unsafe { CreateHardLinkW(l.as_ptr(), t.as_ptr(), std::ptr::null()) };
        if r == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let t = cstr(target)?;
        let l = cstr(link)?;
        // SAFETY: 两个 CString 均无内嵌 NUL。
        let r = unsafe { libc::link(t.as_ptr(), l.as_ptr()) };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// 读取符号链接目标（02 §5.3.3；win 含 junction/reparse point，经 std 封装）。
pub fn read_link(path: &Path) -> io::Result<PathBuf> {
    std::fs::read_link(path)
}

/// 判定路径的链接类型（02 §5.3.3）。
///
/// - **Windows**：`GetFileAttributesW` & `FILE_ATTRIBUTE_REPARSE_POINT` 快路径，
///   需要精确 target 时 `read_link` + 目标存在性判定（`Broken`）。
/// - **Unix**：`lstat`（`S_IFLNK`），target 读取 `readlink`。
pub fn symlink_kind(path: &Path) -> io::Result<LinkKind> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, GetFileAttributesW,
        };
        let w: Vec<u16> = crate::winutil::wide_path(path);
        // SAFETY: 宽字符串 NUL 终止。
        let attrs = unsafe { GetFileAttributesW(w.as_ptr()) };
        if attrs == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            return Ok(LinkKind::NotLink);
        }
        let target = std::fs::read_link(path)?;
        kind_of_target(&target)
    }
    #[cfg(not(windows))]
    {
        let md = std::fs::symlink_metadata(path)?;
        if md.file_type().is_symlink() {
            let target = std::fs::read_link(path)?;
            kind_of_target(&target)
        } else {
            Ok(LinkKind::NotLink)
        }
    }
}

/// 依据链接目标的存在性判定 File / Dir / Broken（目标相对时以链接所在目录为基准）。
fn kind_of_target(target: &Path) -> io::Result<LinkKind> {
    let md = std::fs::metadata(target);
    match md {
        Ok(m) if m.is_dir() => Ok(LinkKind::Dir),
        Ok(_) => Ok(LinkKind::File),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(LinkKind::Broken),
        Err(e) => Err(e),
    }
}

#[cfg(windows)]
fn create_symlink_win(
    target: &Path,
    link: &Path,
    pol: &SymlinkPolicy,
) -> Result<SymlinkOutcome, HzipError> {
    use windows_sys::Win32::Storage::FileSystem::{
        CreateSymbolicLinkW, FILE_ATTRIBUTE_DIRECTORY, GetFileAttributesW,
        SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE, SYMBOLIC_LINK_FLAG_DIRECTORY,
    };

    let is_dir = {
        let t: Vec<u16> = crate::winutil::wide_path(target);
        // SAFETY: 宽字符串 NUL 终止。
        let attrs = unsafe { GetFileAttributesW(t.as_ptr()) };
        attrs != u32::MAX && attrs & FILE_ATTRIBUTE_DIRECTORY != 0
    };
    let mut flags = SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
    if is_dir {
        flags |= SYMBOLIC_LINK_FLAG_DIRECTORY;
    }
    let t: Vec<u16> = crate::winutil::wide_path(target);
    let l: Vec<u16> = crate::winutil::wide_path(link);
    // SAFETY: 宽字符串 NUL 终止。
    let ok = unsafe { CreateSymbolicLinkW(l.as_ptr(), t.as_ptr(), flags) };
    if ok {
        return Ok(SymlinkOutcome::Created);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() != Some(1314) {
        // ERROR_PRIVILEGE_NOT_HELD 之外一律上抛，不误降级
        return Err(HzipError::from(err));
    }
    match pol.fallback {
        Fallback::Copy => {
            if is_dir {
                copy_dir_tree(target, link)?;
            } else {
                std::fs::copy(target, link)?;
            }
            tracing::warn!(target = %target.display(), link = %link.display(),
                "符号链接特权不可用，降级为复制（Fallback::Copy，02 §5.3.3）");
            Ok(SymlinkOutcome::DegradedToCopy)
        }
        Fallback::Reject => Err(HzipError::Unsupported {
            what: "创建符号链接需要特权：请在 Windows 设置中开启开发者模式，或调整符号链接策略（02 §5.3.3）".into(),
        }),
    }
}

/// 复制目录树（降级路径专用；不跟随符号链接防逃逸）。
#[cfg(windows)]
fn copy_dir_tree(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for ent in std::fs::read_dir(src)? {
        let ent = ent?;
        let ft = ent.file_type()?;
        let from = ent.path();
        let to = dst.join(ent.file_name());
        if ft.is_dir() {
            copy_dir_tree(&from, &to)?;
        } else if ft.is_symlink() {
            // 降级路径不跟随链接（防逃逸），跳过并记录
            tracing::warn!(path = %from.display(), "降级复制跳过符号链接");
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn cstr(p: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径含 NUL"))
}

#[cfg(test)]
#[path = "tests/symlink.rs"]
mod tests;
