//! 统一属性（mtime/权限/隐藏/只读/系统/可执行）读写与语义映射（02 §5.3.2；TC-WIN-011）。
//!
//! 内部模型（档案内存储 `mode` unix 视角 + `attrs` win 视角双字段，解压按目标平台
//! 语义恢复）：
//!
//! | 内部模型 | Windows（windows-sys） | Unix（libc） |
//! |---|---|---|
//! | `read_only` | `FILE_ATTRIBUTE_READONLY` | `mode & 0o222 == 0` |
//! | `hidden` | `FILE_ATTRIBUTE_HIDDEN` | 名以 `.` 开头（约定，读取时判定，写入无操作） |
//! | `system` | `FILE_ATTRIBUTE_SYSTEM` | 无（恒 false） |
//! | `executable` | 无（按扩展名 .exe/.bat/.cmd 推断） | `mode & 0o111 != 0` |
//! | `mtime` | FILETIME(100 ns) ↔ Unix ms（1601→1970 偏移 11644473600 s） | std `MetadataExt::{mtime,mtime_nsec}` 纳秒；恢复 `utimensat` |

use std::io;
use std::path::Path;

use time::OffsetDateTime;

/// 平台无关的属性模型（02 §5.3.2 内部模型逐字段）。
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileMeta {
    /// 只读：win `FILE_ATTRIBUTE_READONLY`；unix `mode & 0o222 == 0`
    pub read_only: bool,
    /// 隐藏：win `FILE_ATTRIBUTE_HIDDEN`；unix 名以 `.` 开头（约定）
    pub hidden: bool,
    /// 系统：win `FILE_ATTRIBUTE_SYSTEM`；unix 恒 false
    pub system: bool,
    /// 可执行：win 按扩展名 `.exe/.bat/.cmd` 推断；unix `mode & 0o111 != 0`
    pub executable: bool,
    /// 修改时间（UTC；win 精度 ms，unix 纳秒）
    pub mtime: Option<OffsetDateTime>,
}

/// 读取路径属性为统一模型（02 §5.3.2）。
pub fn read_meta(path: &Path) -> io::Result<FileMeta> {
    #[cfg(windows)]
    {
        win::read_meta_win(path)
    }
    #[cfg(not(windows))]
    {
        unix_impl::read_meta_unix(path)
    }
}

/// 按统一模型恢复属性（02 §5.3.2；unix `hidden`/`system` 无对应位，写入无操作）。
pub fn apply_meta(path: &Path, meta: &FileMeta) -> io::Result<()> {
    #[cfg(windows)]
    {
        win::apply_meta_win(path, meta)
    }
    #[cfg(not(windows))]
    {
        unix_impl::apply_meta_unix(path, meta)
    }
}

/// 仅恢复 mtime（02 §5.3.2；T32 授权例外新增——core 解压管道目录 mtime 延后
/// 恢复的调用点）。
///
/// 与 [`apply_meta`] 的区别：**不动权限/属性位**——`apply_meta` 对 unix 目录会
/// 依 `read_only=false`/`executable=false` 默认调整 mode（目录变 0o666 不可
/// 遍历），不适用于纯时间恢复；且 Windows 目录需 `FILE_FLAG_BACKUP_SEMANTICS`
/// 句柄（std `OpenOptions` 不可表达）。实现：win `open_meta_handle(write=true)`
/// + `SetFileTime`；unix `utimensat`（仅 mtime，atime 保持）。
///
/// T11 例外记录：05 §5 平台 API 缺失路径——core 只消费，不自行触碰 windows-sys。
pub fn set_mtime(path: &Path, dt: OffsetDateTime) -> io::Result<()> {
    #[cfg(windows)]
    {
        win::write_mtime(path, &dt)
    }
    #[cfg(not(windows))]
    {
        unix_impl::write_mtime_unix(path, &dt)
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_SYSTEM,
            FILE_FLAGS_AND_ATTRIBUTES, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
            FILE_WRITE_ATTRIBUTES, GetFileAttributesW, GetFileTime, OPEN_EXISTING,
            SetFileAttributesW, SetFileTime,
        },
    };

    /// 1601-01-01 → 1970-01-01 的 100ns 刻度偏移（02 §5.3.2：11644473600 s × 10^7）。
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;

    fn wide(path: &Path) -> Vec<u16> {
        crate::winutil::wide_path(path)
    }

    fn filetime_to_unix_ms(ft: &FILETIME) -> i128 {
        let t = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
        // (t - EPOCH_DIFF) / 10_000 得 Unix ms（02 §5.3.2 公式）
        (t as i128 - EPOCH_DIFF_100NS as i128) / 10_000
    }

    fn unix_ms_to_filetime(ms: i128) -> FILETIME {
        let t = (ms * 10_000 + EPOCH_DIFF_100NS as i128) as u64;
        FILETIME {
            dwLowDateTime: t as u32,
            dwHighDateTime: (t >> 32) as u32,
        }
    }

    fn is_exec_ext(p: &Path) -> bool {
        p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["exe", "bat", "cmd"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        })
    }

    /// 属性位读取（目录同样适用，无需句柄）。
    fn attrs_of(path: &Path) -> io::Result<u32> {
        let w = wide(path);
        let v = unsafe { GetFileAttributesW(w.as_ptr()) };
        if v == u32::MAX {
            Err(io::Error::last_os_error())
        } else {
            Ok(v)
        }
    }

    /// mtime 读写句柄（目录需 BACKUP_SEMANTICS）。
    fn open_meta_handle(
        path: &Path,
        write: bool,
    ) -> io::Result<windows_sys::Win32::Foundation::HANDLE> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
        };
        let w = wide(path);
        let access = if write {
            FILE_WRITE_ATTRIBUTES
        } else {
            FILE_READ_ATTRIBUTES
        };
        // SAFETY: 宽字符串 NUL 终止（winutil）；句柄失败返回 INVALID_HANDLE_VALUE。
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(h)
        }
    }

    fn read_mtime(path: &Path) -> io::Result<Option<OffsetDateTime>> {
        let h = open_meta_handle(path, false)?;
        let mut write = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        // SAFETY: 句柄有效；write 输出缓冲有效。
        let ok = unsafe { GetFileTime(h, std::ptr::null_mut(), std::ptr::null_mut(), &mut write) };
        unsafe { CloseHandle(h) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let ms = filetime_to_unix_ms(&write);
        let dt =
            OffsetDateTime::from_unix_timestamp_nanos(ms * 1_000_000).map_err(io::Error::other)?;
        Ok(Some(dt))
    }

    pub(super) fn write_mtime(path: &Path, dt: &OffsetDateTime) -> io::Result<()> {
        let h = open_meta_handle(path, true)?;
        let ms = dt.unix_timestamp_nanos() / 1_000_000;
        let ft = unix_ms_to_filetime(ms);
        // SAFETY: 句柄有效；ft 为栈上 FILETIME。
        let ok = unsafe { SetFileTime(h, std::ptr::null(), std::ptr::null(), &ft) };
        unsafe { CloseHandle(h) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn read_meta_win(path: &Path) -> io::Result<FileMeta> {
        let attrs = attrs_of(path)?;
        let mtime = read_mtime(path)?;
        Ok(FileMeta {
            read_only: attrs & FILE_ATTRIBUTE_READONLY != 0,
            hidden: attrs & FILE_ATTRIBUTE_HIDDEN != 0,
            system: attrs & FILE_ATTRIBUTE_SYSTEM != 0,
            executable: is_exec_ext(path),
            mtime,
        })
    }

    pub(super) fn apply_meta_win(path: &Path, meta: &FileMeta) -> io::Result<()> {
        let attrs = attrs_of(path)?;
        let mut flags: FILE_FLAGS_AND_ATTRIBUTES = attrs;
        if meta.read_only {
            flags |= FILE_ATTRIBUTE_READONLY;
        } else {
            flags &= !FILE_ATTRIBUTE_READONLY;
        }
        if meta.hidden {
            flags |= FILE_ATTRIBUTE_HIDDEN;
        } else {
            flags &= !FILE_ATTRIBUTE_HIDDEN;
        }
        if meta.system {
            flags |= FILE_ATTRIBUTE_SYSTEM;
        } else {
            flags &= !FILE_ATTRIBUTE_SYSTEM;
        }
        let w = wide(path);
        // SAFETY: 宽字符串 NUL 终止。
        let ok = unsafe { SetFileAttributesW(w.as_ptr(), flags) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if let Some(dt) = meta.mtime {
            write_mtime(path, &dt)?;
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod unix_impl {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    fn cstr(path: &Path) -> io::Result<std::ffi::CString> {
        std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径含 NUL"))
    }

    /// 读取属性用的元数据（`lstat` 语义，不跟随符号链接）。
    ///
    /// 为什么不直接摸 `libc::stat`：其时间字段名按 ABI 而异（macOS/Linux 是
    /// `st_mtime` + `st_mtime_nsec`、BSD 是 `st_mtim`），取错字段**换个目标就编不过**
    /// （CI Ubuntu 实测 E0609）；std 的 `MetadataExt` 是各 unix 目标的统一视图。
    fn lmeta_of(path: &Path) -> io::Result<std::fs::Metadata> {
        std::fs::symlink_metadata(path)
    }

    pub(super) fn read_meta_unix(path: &Path) -> io::Result<FileMeta> {
        let md = lmeta_of(path)?;
        let mode = md.mode();
        let mtime = {
            let (sec, nsec) = (md.mtime(), md.mtime_nsec());
            let dt = OffsetDateTime::from_unix_timestamp_nanos(
                sec as i128 * 1_000_000_000 + nsec as i128,
            )
            .map_err(io::Error::other)?;
            Some(dt)
        };
        let hidden = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.') && n != "." && n != "..");
        Ok(FileMeta {
            read_only: mode & 0o222 == 0,
            hidden,
            system: false,
            executable: mode & 0o111 != 0,
            mtime,
        })
    }

    pub(super) fn write_mtime_unix(path: &Path, dt: &OffsetDateTime) -> io::Result<()> {
        let c = cstr(path)?;
        let sec = dt.unix_timestamp();
        let nsec = (dt.unix_timestamp_nanos() - sec as i128 * 1_000_000_000) as i64;
        let times = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT, // atime 保持
            },
            libc::timespec {
                tv_sec: sec,
                tv_nsec: nsec,
            },
        ];
        // SAFETY: CString 无内嵌 NUL；times 为栈上双元素数组。
        if unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn apply_meta_unix(path: &Path, meta: &FileMeta) -> io::Result<()> {
        let mut mode = lmeta_of(path)?.mode();
        if meta.read_only {
            mode &= !0o222;
        } else {
            mode |= 0o222;
        }
        if meta.executable {
            mode |= 0o111;
        } else {
            mode &= !0o111;
        }
        let c = cstr(path)?;
        // SAFETY: CString 无内嵌 NUL。
        if unsafe { libc::chmod(c.as_ptr(), mode as libc::mode_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if let Some(dt) = meta.mtime {
            let sec = dt.unix_timestamp();
            let nsec = (dt.unix_timestamp_nanos() - sec as i128 * 1_000_000_000) as i64;
            let times = [
                libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_OMIT,
                },
                libc::timespec {
                    tv_sec: sec,
                    tv_nsec: nsec,
                },
            ];
            // SAFETY: CString 无内嵌 NUL；times 为栈上双元素数组。
            if unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/metadata.rs"]
mod tests;
