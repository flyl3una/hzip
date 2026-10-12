//! 进程独占文件锁（T42 补丁：GUI 单实例 `hzip.lock` 载体；02 §11.2 B）。
//!
//! # 动机
//!
//! GUI 单实例需「二次打开失败 = 已有实例」的**排他锁**语义。早期 T42 方案以
//! 「锁文件 + pid 探活」近似，但 Windows `is_alive` 对已死进程判活
//! （不可证死则判活，02 §5.3.8 语义）会使陈旧锁永不失效——必须用 OS 级
//! 排他锁（句柄持有即锁，进程退出自动释放，无陈旧锁问题）。
//!
//! # 实现
//!
//! - Windows：`CreateFileW` + `LockFileEx`（排他、不阻塞）；句柄持有期间
//!   二次 `LockFileEx` 失败 `ERROR_LOCK_VIOLATION` → 已有实例；
//! - unix：`flock(LOCK_EX | LOCK_NB)`。
//!
//! 锁文件内容写 `pid created_ms`（诊断/孤儿辅助；锁本身以 OS 语义为准，
//! 内容不参与判定）。

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OPEN_REPARSE_POINT, LockFileEx, OPEN_ALWAYS, UnlockFileEx,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_DATA,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY};

/// 排他锁守卫：持有 = 持锁；Drop 释放（进程退出亦自动释放）。
pub struct FileLock {
    #[cfg(windows)]
    handle: HANDLE,
    #[cfg(not(windows))]
    file: std::fs::File,
    path: PathBuf,
}

/// 尝试获取排他锁（非阻塞）。
///
/// `Ok(guard)` = 获取成功（本进程为首实例）；`Err` = 已有实例持有锁
/// （或 IO 错误，语义由调用方按「非 Ok 即二实例」处理）。
pub fn try_lock_exclusive(path: &Path) -> io::Result<FileLock> {
    #[cfg(windows)]
    {
        try_lock_win(path)
    }
    #[cfg(not(windows))]
    {
        try_lock_unix(path)
    }
}

#[cfg(windows)]
fn try_lock_win(path: &Path) -> io::Result<FileLock> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_DATA | FILE_WRITE_DATA,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_ALWAYS,
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // 锁前写持有者信息（诊断；独立句柄，避免锁区域读写互斥）
    let _ = write_owner(path);
    // 排他锁整个文件（非阻塞）
    let mut overlapped =
        unsafe { std::mem::zeroed::<windows_sys::Win32::System::IO::OVERLAPPED>() };
    let ok = unsafe {
        LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if ok == 0 {
        let err = io::Error::last_os_error();
        unsafe { CloseHandle(handle) };
        return Err(err);
    }
    Ok(FileLock {
        handle,
        path: path.to_path_buf(),
    })
}

#[cfg(windows)]
fn write_owner(path: &Path) -> io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.set_len(0)?;
    f.seek(SeekFrom::Start(0))?;
    writeln!(
        f,
        "{} {}",
        std::process::id(),
        crate::proc::self_created_ms()
    )
}

#[cfg(not(windows))]
fn try_lock_unix(path: &Path) -> io::Result<FileLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
    // flock 排他非阻塞
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // 持锁后才写持有者信息（诊断）：flock 无字节区间互斥，写回自持 fd 即可；
    // 也因此不会像「锁前写」那样让抢锁失败方覆盖真实持有者。
    let _ = write_owner(path);
    Ok(FileLock {
        file,
        path: path.to_path_buf(),
    })
}

#[cfg(not(windows))]
fn write_owner(path: &Path) -> io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.set_len(0)?;
    f.seek(SeekFrom::Start(0))?;
    writeln!(
        f,
        "{} {}",
        std::process::id(),
        crate::proc::self_created_ms()
    )
}

impl Drop for FileLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            let mut overlapped =
                unsafe { std::mem::zeroed::<windows_sys::Win32::System::IO::OVERLAPPED>() };
            unsafe {
                UnlockFileEx(self.handle, 0, u32::MAX, u32::MAX, &mut overlapped);
                CloseHandle(self.handle);
            }
        }
        #[cfg(not(windows))]
        {
            let fd = std::os::unix::io::AsRawFd::as_raw_fd(&self.file);
            unsafe { libc::flock(fd, libc::LOCK_UN) };
        }
        let _ = &self.path;
    }
}

/// 锁路径访问（诊断）。
impl FileLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
#[path = "tests/file_lock.rs"]
mod tests;
