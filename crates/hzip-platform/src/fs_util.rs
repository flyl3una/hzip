//! 文件系统工具（02 §5.3.4；TC-SEC-009/011、TC-CONF-009 支撑）。
//!
//! | 函数 | 平台实现 |
//! |---|---|
//! | `available_bytes` | win `GetDiskFreeSpaceExW`（配额感知）；unix `statvfs` |
//! | `is_sparse` | win `DeviceIoControl(FSCTL_QUERY_ALLOCATED_RANGES)`；unix `st_blocks*512 < st_size` |
//! | `is_case_sensitive` | 探测式实测（对撞文件），按 dev id 进程内缓存 |
//! | `atomic_replace` | 同卷 rename；跨卷 copy+fsync+rename（`EXDEV`/`ERROR_NOT_SAME_DEVICE` 触发） |
//! | `remove_all_no_follow` | 自实现遍历，符号链接只 unlink 不深入（防逃逸删除） |

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

// 稀疏写支持（02 §5.3.4 / T22 §4.7 独占新增）：`set_sparse` + 句柄式稀疏检测。
// 实现在 `fs_util/sparse.rs`；此处 re-export 便于 `hzip_platform::fs_util::set_sparse`
// 直调（T22 独占 API）。
pub mod sparse;

/// 把已打开的 [`std::fs::File`] 标记为稀疏（best-effort；02 §5.3.4 / T22 §4.7）。
///
/// 实现与语义见 [`sparse::set_sparse`]（win `FSCTL_SET_SPARSE`；unix 天然稀疏直接
/// 成功）。返回 `false`/`Err` 时调用方应降级密集写（字节正确性不受影响）。
pub use sparse::set_sparse;

/// 目录可用字节数（02 §5.3.4；win 取 `lpFreeBytesAvailableToCaller`，配额感知）。
pub fn available_bytes(path: &Path) -> io::Result<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let w: Vec<u16> = crate::winutil::wide_path(path);
        let mut avail: u64 = 0;
        let mut total: u64 = 0;
        let mut free: u64 = 0;
        // SAFETY: 宽字符串 NUL 终止；三个输出缓冲有效。
        let r = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
        if r == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(avail)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径含 NUL"))?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: CString 无内嵌 NUL；输出结构由内核填充。
        let r = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        // f_bavail * f_frsize：对非特权用户可用块数。
        // `f_bavail` 宽度平台不一（macOS `fsblkcnt_t = u32`、Linux `u64`）——统一到
        // u64 再乘，避免 32 位中间结果溢出。
        #[cfg(target_os = "macos")]
        let avail = u64::from(st.f_bavail).saturating_mul(st.f_frsize);
        #[cfg(not(target_os = "macos"))]
        let avail = st.f_bavail.saturating_mul(st.f_frsize);
        Ok(avail)
    }
}

/// 稀疏文件检测（02 §5.3.4）。
///
/// - **win**：`DeviceIoControl(FSCTL_QUERY_ALLOCATED_RANGES)` 查询已分配区间，
///   存在「未分配区间」→ 稀疏。
/// - **unix**：`lstat` 比较 `st_blocks * 512 < st_size`。
///
/// **APFS 注**：macOS 会把小于约 16 MiB 的洞在关闭文件时重新密集化，故小洞文件
/// 在 `st_blocks` 口径下可能判为「不稀疏」；这是文件系统分配策略，不是检测错误。
/// 断言稀疏性的用例请使用足够大的洞（≥ 32 MiB）。
pub fn is_sparse(path: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    {
        is_sparse_win(path)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::MetadataExt;
        let md = std::fs::symlink_metadata(path)?;
        Ok(md.blocks() * 512 < md.size())
    }
}

#[cfg(windows)]
fn is_sparse_win(path: &Path) -> io::Result<bool> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
    };

    let w: Vec<u16> = crate::winutil::wide_path(path);
    // SAFETY: 宽字符串 NUL 终止；以只读属性方式打开（目录不可稀疏，跳过）。
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 句柄有效；调用后关闭。
    let result = query_sparse(h);
    unsafe { CloseHandle(h) };
    result
}

#[cfg(windows)]
fn query_sparse(h: windows_sys::Win32::Foundation::HANDLE) -> io::Result<bool> {
    use windows_sys::Win32::{
        System::IO::DeviceIoControl,
        System::Ioctl::{FILE_ALLOCATED_RANGE_BUFFER, FSCTL_QUERY_ALLOCATED_RANGES},
    };
    // 输入：查询区间 = 整个文件（offset 0, length = 文件大小）
    let size = {
        use windows_sys::Win32::Storage::FileSystem::GetFileSizeEx;
        let mut sz: i64 = 0;
        // SAFETY: 句柄有效；sz 为输出缓冲。
        let r = unsafe { GetFileSizeEx(h, &mut sz) };
        if r == 0 {
            return Err(io::Error::last_os_error());
        }
        sz
    };
    if size <= 0 {
        return Ok(false); // 空文件无稀疏可言
    }
    #[repr(C)]
    struct QueryRanges {
        offset: i64,
        length: i64,
    }
    let query = QueryRanges {
        offset: 0,
        length: size,
    };
    let mut out = [FILE_ALLOCATED_RANGE_BUFFER {
        FileOffset: 0,
        Length: 0,
    }; 64];
    let mut returned: u32 = 0;
    // SAFETY: 输入/输出缓冲尺寸匹配；DeviceIoControl 由内核填充。
    let r = unsafe {
        DeviceIoControl(
            h,
            FSCTL_QUERY_ALLOCATED_RANGES,
            &query as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<QueryRanges>() as u32,
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
    // 已分配区间之和 < 文件大小 → 存在未分配区间 → 稀疏
    Ok(allocated < size)
}

/// 大小写敏感性探测（02 §5.3.4 探测算法；TC-SEC-011 支撑）。
///
/// 必须实测而非按 FS 类型推断（macOS 卷可配置、Linux 挂 NTFS）。探测失败或不确定
/// 一律回退保守值 `true`（大小写敏感侧——core 冲突检测宁可误报不可漏报）。
/// 结果按（目录的 dev id）进进程内缓存（[`OnceLock<Mutex<HashMap<u64, bool>>>`]）。
pub fn is_case_sensitive(dir: &Path) -> bool {
    let dev = volume_id(dir);
    let cached = CASE_CACHE
        .get_or_init(Default::default)
        .lock()
        .ok()
        .and_then(|g| g.get(&dev).copied());
    if let Some(cached) = cached {
        return cached;
    }
    let probe = probe_case_sensitivity(dir);
    if let Some(result) = probe {
        if let Ok(mut g) = CASE_CACHE.get_or_init(Default::default).lock() {
            g.insert(dev, result);
        }
        result
    } else {
        true // 探测 IO 错误 → 保守 true
    }
}

/// 进程内缓存：dev id → 探测结果。
static CASE_CACHE: OnceLock<Mutex<HashMap<u64, bool>>> = OnceLock::new();

/// 目录所在卷的 dev id（win：卷序列号；unix：`stat().st_dev`；失败回退 0）。
fn volume_id(dir: &Path) -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
            Storage::FileSystem::{
                CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
                FILE_SHARE_WRITE, GetVolumeInformationByHandleW, OPEN_EXISTING,
            },
        };
        let w: Vec<u16> = crate::winutil::wide_path(dir);
        // SAFETY: 打开目录句柄（BACKUP_SEMANTICS 允许目录），失败返回 0。
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return 0;
        }
        let mut serial: u32 = 0;
        // SAFETY: 句柄有效；其余输出缓冲可空。
        let r = unsafe {
            GetVolumeInformationByHandleW(
                h,
                std::ptr::null_mut(),
                0,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        unsafe { CloseHandle(h) };
        if r == 0 { 0 } else { serial as u64 }
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = match std::ffi::CString::new(dir.as_os_str().as_bytes()) {
            Ok(c) => c,
            Err(_) => return 0,
        };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: CString 无内嵌 NUL。
        let r = unsafe { libc::stat(c.as_ptr(), &mut st) };
        if r != 0 {
            return 0;
        }
        // `st_dev` 宽度平台不一（macOS `dev_t = i32`、Linux `u64`）——统一为 u64
        // 缓存键。macOS 侧按位扩展（主设备号可置符号位，不做符号扩展）。
        #[cfg(target_os = "macos")]
        let dev = u64::from(st.st_dev as u32);
        #[cfg(not(target_os = "macos"))]
        let dev = st.st_dev;
        dev
    }
}

/// 探测实现：写入探针 A，尝试以大小写变体 B 打开读取比对（02 §5.3.4 算法）。
/// 返回 `None` 表示探测因 IO 错误失败（调用方回退保守 true）。
fn probe_case_sensitivity(dir: &Path) -> Option<bool> {
    let pid = std::process::id();
    let a_name = format!("hzip-.case-probe-{pid}");
    let b_name = a_name.to_ascii_uppercase();
    let probe_a = dir.join(&a_name);
    let probe_b = dir.join(&b_name);

    // 关键顺序：清残迹必须在写 A 之前——在大小写不敏感卷上 remove(probe_b)
    // 会连同 probe_a 一起删除（两名指同一文件），导致后续 read(probe_b) 必 ENOENT
    // 而误判「敏感」。
    let _ = std::fs::remove_file(&probe_a);
    let _ = std::fs::remove_file(&probe_b);

    // marker：16 字节伪随机（时间+pid 混合，够用于对撞比对）
    let marker = pseudo_marker();
    if std::fs::write(&probe_a, &marker[..]).is_err() {
        return None;
    }
    let result = match std::fs::read(&probe_b) {
        Ok(data) => Some(data != marker),
        Err(_) => Some(true), // read(probe_b) ENOENT → 大小写敏感
    };
    let _ = std::fs::remove_file(&probe_a);
    let _ = std::fs::remove_file(&probe_b);
    result
}

/// 16 字节伪随机 marker（无第三方依赖；时间 + pid + 固定盐经 xorshift 混合）。
fn pseudo_marker() -> [u8; 16] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let mut state = nanos ^ (pid << 32) ^ 0x9e3779b97f4a7c15;
    let mut out = [0u8; 16];
    for chunk in out.chunks_mut(4) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let v = state as u32;
        chunk.copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// 原子替换（02 §5.3.4；TC-CONF-009 编辑原子性依赖）。
///
/// 同卷 `rename`；失败且 errno 为 `EXDEV`（win `ERROR_NOT_SAME_DEVICE`）→ 跨卷回退：
/// copy（64 KiB 块）→ `File::sync_all`（fsync 数据+目录）→ rename 临时名到 dst；
/// 失败中间产物清理。
pub fn atomic_replace(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let s: Vec<u16> = crate::winutil::wide_path(src);
        let d: Vec<u16> = crate::winutil::wide_path(dst);
        // SAFETY: 宽字符串 NUL 终止；MOVEFILE_REPLACE_EXISTING|WRITE_THROUGH 语义。
        let r = unsafe {
            MoveFileExW(
                s.as_ptr(),
                d.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if r != 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(17) {
            // ERROR_NOT_SAME_DEVICE 之外直接失败
            return Err(err);
        }
        replace_cross_volume(src, dst)
    }
    #[cfg(not(windows))]
    {
        match std::fs::rename(src, dst) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => replace_cross_volume(src, dst),
            Err(e) => Err(e),
        }
    }
}

/// 跨卷回退：copy（64 KiB 块）→ fsync → rename（02 §5.3.4）。
fn replace_cross_volume(src: &Path, dst: &Path) -> io::Result<()> {
    const CHUNK: usize = 64 * 1024;
    let dst_dir = dst
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标路径无父目录"))?;
    let tmp = unique_tmp_name(dst_dir);

    let copy_result = (|| -> io::Result<()> {
        let mut r = std::fs::File::open(src)?;
        let mut w = std::fs::File::create(&tmp)?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])?;
        }
        w.sync_all()?; // fsync 数据
        drop(w);
        #[cfg(not(windows))]
        {
            // fsync 目录（win 由 MOVEFILE_WRITE_THROUGH 承担）
            let dir_f = std::fs::File::open(dst_dir)?;
            dir_f.sync_all()?;
        }
        rename_replace(&tmp, dst)
    })();

    if let Err(e) = copy_result {
        let _ = std::fs::remove_file(&tmp); // 失败中间产物清理
        Err(e)
    } else {
        Ok(())
    }
}

/// 生成唯一临时名（pid + 时间戳纳秒 + 原子序号），用于跨卷回退的中间产物。
fn unique_tmp_name(dst_dir: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dst_dir.join(format!(".hzip-tmp-{}-{nanos:x}-{seq}", std::process::id()))
}

/// 同卷内 rename 并允许覆盖 dst。
fn rename_replace(tmp: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let s: Vec<u16> = crate::winutil::wide_path(tmp);
        let d: Vec<u16> = crate::winutil::wide_path(dst);
        // SAFETY: 同卷 rename，MOVEFILE_REPLACE_EXISTING 覆盖目标。
        let r = unsafe {
            MoveFileExW(
                s.as_ptr(),
                d.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if r == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(tmp, dst)
    }
}

/// 安全删除目录树（02 §5.3.4；TC-SEC-009 防逃逸删除）。
///
/// 自实现遍历：`symlink_metadata` 判类型；符号链接只 `unlink` 不深入；目录递归；
/// `.` / `..` 不出现（read_dir 语义保证）。
pub fn remove_all_no_follow(root: &Path) -> io::Result<()> {
    let md = std::fs::symlink_metadata(root)?;
    if md.file_type().is_symlink() || md.is_file() {
        return std::fs::remove_file(root);
    }
    if md.is_dir() {
        for ent in std::fs::read_dir(root)? {
            let ent = ent?;
            remove_all_no_follow(&ent.path())?;
        }
        return std::fs::remove_dir(root);
    }
    Err(io::Error::other("不支持的文件类型"))
}

#[cfg(test)]
#[path = "tests/fs_util.rs"]
mod tests;
