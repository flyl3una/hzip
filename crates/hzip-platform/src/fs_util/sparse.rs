//! 稀疏文件写支持（02 §5.3.4 稀疏；T22 §4.7 归本任务独占新增）。
//!
//! | 函数 | 平台实现 |
//! |---|---|
//! | [`set_sparse`] | win `DeviceIoControl(FSCTL_SET_SPARSE)`；unix 天然稀疏，直接成功 |
//! | [`is_sparse_file`] | win `DeviceIoControl(FSCTL_QUERY_ALLOCATED_RANGES)`（GENERIC_READ 打开）；unix `fstat` 比较 `st_blocks` |
//!
//! 稀疏性为 **best-effort**——[`set_sparse`] 返回 `false`/`Err` 时调用方应降级为
//! 密集写入（字节正确性不受影响），对应 02 §8.4「Windows 失败降级密集写 +
//! warning」。
//!
//! # 与既有 `fs_util::is_sparse` 的关系
//!
//! [`super::is_sparse`] 为 T11 既有**路径式**检测（Windows 以 `FILE_READ_ATTRIBUTES`
//! 打开后查询——该 ioctl 实际要求 `FILE_READ_DATA`/`GENERIC_READ`，Windows 上恒
//! 失败，属 T11 遗留缺陷；`fs_util.rs` 冻结不可就地修改）。本模块的
//! [`is_sparse_file`] 为**句柄式**正确实现（消费方已持有写句柄，天然满足访问
//! 权限），供本任务稀疏落盘自验与 parity 测试使用；既有 `is_sparse` 的修复挂
//! 后续登记项。

use std::io;

/// 把已打开的 [`std::fs::File`] 标记为稀疏（best-effort）。
///
/// - **win**：`DeviceIoControl(FSCTL_SET_SPARSE)`（句柄须以写方式打开）。
///   失败返回 `Err`；某些 FS（FAT/exFAT）返回 `Ok(false)` 表示不支持。
/// - **unix**：`Ok(true)`——seek 即洞，文件天然稀疏。
///
/// 调用时机须在**扩展文件之前**（NTFS 只对标记后新增的洞生效）。标记成功后
/// 建议以 [`is_sparse_file`] 复核。
pub fn set_sparse(file: &std::fs::File) -> io::Result<bool> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::INVALID_HANDLE_VALUE, System::IO::DeviceIoControl,
            System::Ioctl::FSCTL_SET_SPARSE,
        };

        let h = file.as_raw_handle();
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "非法文件句柄"));
        }
        // SAFETY: 句柄来自有效 File；FSCTL_SET_SPARSE 无输入/输出缓冲。
        let r = unsafe {
            DeviceIoControl(
                h,
                FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if r == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(true)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Ok(true)
    }
}

/// 稀疏性检测（句柄式，跨平台正确）。
///
/// - **win**：`DeviceIoControl(FSCTL_QUERY_ALLOCATED_RANGES)` 查询已分配区间，
///   存在「未分配区间」→ 稀疏（输入区间 = 整个文件）。**句柄须具备读访问**
///   （ioctl 要求 `FILE_READ_DATA`/`GENERIC_READ`；`File::create` 的写句柄
///   不满足——请以 `File::open` 读句柄查询）。
/// - **unix**：`fstat` 比较 `st_blocks * 512 < st_size`（读句柄即可）。
pub fn is_sparse_file(file: &std::fs::File) -> io::Result<bool> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            System::IO::DeviceIoControl,
            System::Ioctl::{FILE_ALLOCATED_RANGE_BUFFER, FSCTL_QUERY_ALLOCATED_RANGES},
        };

        let size = file.metadata()?.len();
        if size == 0 {
            return Ok(false);
        }
        #[repr(C)]
        struct Query {
            offset: i64,
            length: i64,
        }
        let query = Query {
            offset: 0,
            length: size as i64,
        };
        let mut out = [FILE_ALLOCATED_RANGE_BUFFER {
            FileOffset: 0,
            Length: 0,
        }; 64];
        let mut returned: u32 = 0;
        // SAFETY: 输入/输出缓冲尺寸匹配；DeviceIoControl 由内核填充。
        let r = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_QUERY_ALLOCATED_RANGES,
                &query as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<Query>() as u32,
                out.as_mut_ptr() as *mut core::ffi::c_void,
                std::mem::size_of_val(&out) as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if r == 0 {
            return Err(io::Error::last_os_error());
        }
        let count = (returned as usize) / std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>();
        let allocated: i64 = out[..count].iter().map(|r| r.Length).sum();
        Ok(allocated < size as i64)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::MetadataExt;
        let md = file.metadata()?;
        Ok(md.blocks() * 512 < md.size())
    }
}

#[cfg(test)]
#[path = "tests/sparse.rs"]
mod tests;
