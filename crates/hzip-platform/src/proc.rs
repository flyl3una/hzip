//! 进程存活检测（PID + 创建时间双判定，02 §5.3.8；孤儿清理支撑）。
//!
//! 孤儿清理不能只看 PID 存在：比对 `(pid, 进程创建时间戳)` 二元组防 PID 复用。
//!
//! | 平台 | 实现 |
//! |---|---|
//! | Windows | `OpenProcess` + `NtQueryInformationProcess(ProcessTimes)`（FILETIME → Unix ms） |
//! | Linux | `/proc/{pid}/stat` 第 22 字段 `starttime` + `/proc/stat` `btime` + `sysconf(_SC_CLK_TCK)` |
//! | macOS | `libc::proc_pidinfo(PROC_PIDTBSDINFO)` → `pbi_start_tvsec` |
//!
//! # Windows 实现注记（T11 修订）
//!
//! 设计文档 02 §5.3.8 指定 `GetProcessTimes`；但该 API 在部分沙箱/远程执行环境
//! 实测触发 `STATUS_ACCESS_VIOLATION`（调用即崩，无法捕获），而语义等价的
//! `NtQueryInformationProcess(ProcessTimes)` 稳定可用。故 Windows 侧以
//! `ntdll!NtQueryInformationProcess` 实现（本 crate 为唯一平台收口层，直接 extern
//! ntdll 符合红线 5——平台细节不外泄）；PID+创建时间双判定语义不变。

/// 双判定：pid 存在 且 该进程创建时间 == created_ms（02 §5.3.8）。
///
/// **不可证死则判活**（任务卡 §7.3）：权限不足等模糊态返回 `true`（孤儿清理宁可
/// 漏删一个目录，不可误删活跃会话——漏删由后续启动重试）。
pub fn is_alive(pid: u32, created_ms: u64) -> bool {
    #[cfg(windows)]
    {
        is_alive_win(pid, created_ms)
    }
    #[cfg(target_os = "linux")]
    {
        is_alive_linux(pid, created_ms)
    }
    #[cfg(target_os = "macos")]
    {
        is_alive_macos(pid, created_ms)
    }
}

/// 本进程创建时刻的 Unix 毫秒（供 temp_area 写 pid 文件，02 §5.3.8）。
pub fn self_created_ms() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let pid = unsafe { GetCurrentProcessId() };
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h == INVALID_HANDLE_VALUE {
            return 0;
        }
        let ms = query_creation_ms(h);
        unsafe { CloseHandle(h) };
        ms
    }
    #[cfg(target_os = "linux")]
    {
        starttime_to_created_ms(std::process::id())
    }
    #[cfg(target_os = "macos")]
    {
        starttimesec_to_created_ms(std::process::id())
    }
}

#[cfg(windows)]
fn is_alive_win(pid: u32, created_ms: u64) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ACCESS_DENIED, INVALID_HANDLE_VALUE},
        System::Threading::{
            OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };
    // 先以最小权限打开；ACCESS_DENIED 时改用全查询权限重试
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h == INVALID_HANDLE_VALUE {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
            let h2 = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid) };
            if h2 == INVALID_HANDLE_VALUE {
                // 权限不足 → 不可证死则判活
                return true;
            }
            return created_ms_eq(h2, created_ms);
        }
        // 其余错误（ERROR_INVALID_PARAMETER 等）→ 进程不存在
        return false;
    }
    let alive = created_ms_eq(h, created_ms);
    unsafe { CloseHandle(h) };
    alive
}

/// 查询句柄对应进程的创建时间（Unix ms）。失败返回 0。
#[cfg(windows)]
fn query_creation_ms(h: windows_sys::Win32::Foundation::HANDLE) -> u64 {
    // NtQueryInformationProcess(ProcessTimes=4) → KERNEL_USER_TIMES{create,exit,kernel,user}
    // 见模块文档注记（GetProcessTimes 在部分环境调用即崩）。
    #[repr(C)]
    struct KernelUserTimes {
        create_time: i64,
        exit_time: i64,
        kernel_time: i64,
        user_time: i64,
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            handle: windows_sys::Win32::Foundation::HANDLE,
            class: u32,
            info: *mut u8,
            len: u32,
            ret: *mut u32,
        ) -> i32;
    }
    const PROCESS_TIMES: u32 = 4;
    let mut t = KernelUserTimes {
        create_time: 0,
        exit_time: 0,
        kernel_time: 0,
        user_time: 0,
    };
    let mut ret: u32 = 0;
    // SAFETY: t 与 ret 为有效可写缓冲；长度 = 结构体大小；handle 由调用方保证有效。
    let r = unsafe {
        NtQueryInformationProcess(
            h,
            PROCESS_TIMES,
            &mut t as *mut _ as *mut u8,
            std::mem::size_of::<KernelUserTimes>() as u32,
            &mut ret,
        )
    };
    if r != 0 {
        return 0;
    }
    filetime_to_unix_ms_raw(t.create_time as u64)
}

/// FILETIME(100 ns 计数, 1601-01-01) → Unix ms（02 §5.3.2 同一换算）。
#[cfg(windows)]
fn filetime_to_unix_ms_raw(t: u64) -> u64 {
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
    (t.saturating_sub(EPOCH_DIFF_100NS)) / 10_000
}

#[cfg(windows)]
fn created_ms_eq(h: windows_sys::Win32::Foundation::HANDLE, created_ms: u64) -> bool {
    let ms = query_creation_ms(h);
    if ms == 0 {
        // 不可证死则判活
        return true;
    }
    ms == created_ms
}

#[cfg(target_os = "linux")]
fn is_alive_linux(pid: u32, created_ms: u64) -> bool {
    let created = starttime_to_created_ms(pid);
    created != 0 && created == created_ms
}

/// Linux：`/proc/{pid}/stat` 第 22 字段 `starttime`（自开机时钟滴答）
/// → `created_ms = btime_ms + starttime * 1000 / CLK_TCK`（02 §5.3.8）。
#[cfg(target_os = "linux")]
fn starttime_to_created_ms(pid: u32) -> u64 {
    let path = format!("/proc/{pid}/stat");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return 0,
    };
    // comm 字段可能含空格/括号：取最后一个 ')' 之后切分
    let Some(close) = text.rfind(')') else {
        return 0;
    };
    let fields: Vec<&str> = text[close + 1..].split_whitespace().collect();
    // 第 3 个字段（索引 2）即整体第 22 字段 starttime（1 起数；comm 为第 2 字段）
    let Some(starttime) = fields.get(2).and_then(|f| f.parse::<u64>().ok()) else {
        return 0;
    };
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if clk_tck <= 0 {
        return 0;
    }
    let btime_ms = boot_time_ms();
    btime_ms + starttime.saturating_mul(1000) / clk_tck as u64
}

/// Linux：`/proc/stat` 的 `btime` 行（自纪元秒）→ ms。
#[cfg(target_os = "linux")]
fn boot_time_ms() -> u64 {
    let text = match std::fs::read_to_string("/proc/stat") {
        Ok(t) => t,
        Err(_) => return 0,
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            if let Ok(secs) = rest.trim().parse::<u64>() {
                return secs.saturating_mul(1000);
            }
        }
    }
    0
}

#[cfg(target_os = "macos")]
fn is_alive_macos(pid: u32, created_ms: u64) -> bool {
    let created = starttimesec_to_created_ms(pid);
    created != 0 && created == created_ms
}

/// macOS：`proc_pidinfo(PROC_PIDTBSDINFO)` → `pbi_start_tvsec`（秒）* 1000（02 §5.3.8）。
#[cfg(target_os = "macos")]
fn starttimesec_to_created_ms(pid: u32) -> u64 {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            std::mem::size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if r <= 0 {
        0
    } else {
        // Darwin `proc_bsdinfo.pbi_start_tvsec`（启动时刻，自纪元秒；libc 命名，
        // SDK 头文件同名字段）。`* 1000` → Unix ms，与 pid 文件 written 侧同源。
        info.pbi_start_tvsec.saturating_mul(1000)
    }
}

#[cfg(test)]
#[path = "tests/proc.rs"]
mod tests;
