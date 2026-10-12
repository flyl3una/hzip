//! 载入/调用追踪（**仅 `shell-trace` feature 开启时编译**）。
//!
//! # 为什么需要它
//!
//! 「右键菜单里没有我们的项」有两种截然不同的成因，而它们从外部看起来一模一样：
//!
//! | 成因 | 现象 | 排查方向 |
//! |---|---|---|
//! | shell **从未**枚举我们的命令 | 无任何调用 | 包注册 / 清单 / 是否需重新登录 |
//! | shell 枚举了，但我们的 `GetState` 把项**隐藏**了 | 有调用、无项 | 本 DLL 逻辑 / `shell-menu.json` |
//!
//! 记录「谁在什么时候调用过哪个方法」即可一刀切开。附带记录**宿主进程名**
//! （`explorer.exe` = 经典注册的进程内路径；`dllhost.exe` = 稀疏包的代理路径），
//! 从而同时回答「哪条注册路径真的生效了」。
//!
//! # 生产零开销
//!
//! 未开 feature 时 [`trace`] 编译为空函数（无分支、无 I/O）。追踪会把
//! **每一次菜单构建**都写盘，绝不能在发布版启用。

/// 追踪文件路径（`%TEMP%\hzip-shell-trace.log`）。
#[cfg(feature = "shell-trace")]
fn trace_path() -> Option<std::path::PathBuf> {
    let tmp = std::env::var_os("TEMP").or_else(|| std::env::var_os("TMP"))?;
    Some(std::path::PathBuf::from(tmp).join("hzip-shell-trace.log"))
}

/// 记录一条追踪（`what` 为方法名/事件；失败静默——追踪本身绝不能影响功能）。
#[cfg(feature = "shell-trace")]
pub fn trace(what: &str) {
    use std::io::Write as _;
    let Some(path) = trace_path() else { return };
    let host = host_exe_name();
    let line = format!("pid={} host={} {}\n", std::process::id(), host, what);
    // 追加写；多进程（explorer / dllhost）并发写同一文件，单行 write 足够原子
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// 当前进程的可执行文件名（判断「谁加载了我们」的关键信息）。
#[cfg(feature = "shell-trace")]
fn host_exe_name() -> String {
    use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
    let mut buf = [0u16; 260];
    // SAFETY: 传 NULL 模块句柄 = 当前进程的可执行文件；buf 有效且长度已知
    let len = unsafe { GetModuleFileNameW(None, &mut buf) };
    if len == 0 {
        return "?".to_string();
    }
    let s = String::from_utf16_lossy(&buf[..len as usize]);
    s.rsplit(['\\', '/']).next().unwrap_or("?").to_string()
}

/// 记录一条追踪（发布版：编译为空）。
#[cfg(not(feature = "shell-trace"))]
#[inline(always)]
pub fn trace(_what: &str) {}
