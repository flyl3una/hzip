//! 用系统程序打开 / 在文件管理器中显示（reveal）（02 §5.3.7）。
//!
//! | 平台 | `open_with_default_app` | `reveal_in_file_manager` |
//! |---|---|---|
//! | Windows | `ShellExecuteW`（目录 verb `explore`） | `explorer.exe /select,"path"` 或 `explorer.exe dir` |
//! | macOS | `Command::new("open")` | `open -R path`；`select=false` → `open 目录`（Finder 精确选中用 objc NSWorkspace 为增强分支） |
//! | Linux | `xdg-open` | 优先 dbus `org.freedesktop.FileManager1` `ShowItems`（dbus-send/busctl 探测），回退 `xdg-open 父目录` |
//!
//! # 「打开方式…」= 系统原生询问框（2026-09-28 第二轮修正；用户明确要资源管理器那样）
//!
//! 第一轮误判「系统通道全不可用」而自实现了候选程序列表；用户拒绝自实现、要系统询问
//! 窗口。复查后找到真正的判据：`SHOpenWithDialog` **不是**不可用，而是**必须从「非属主
//! 窗口线程」调用**（属主线程要继续泵消息，询问框才建得起来）：
//!
//! | 调用线程 | 结果 |
//! |---|---|
//! | = 属主窗口线程（调用时自己没法泵消息） | 立即 `ERROR_CANCELLED`；`openas` / `OpenWith.exe` / `OpenAs_RunDLL` 一并无效 |
//! | **≠ 属主窗口线程**（属主线程继续泵消息） | **`S_OK` + 系统询问框出现**（实测标题 = 文件名、类 `WinUIDesktopWin32WindowClass`） |
//!
//! hzip 的打开流程天然是后者（`hzip-open` 工作线程 + gpui 主线程泵消息），故
//! [`open_with_picker`] = `SHOpenWithDialog(属主窗口, OAIF_EXEC)`。自实现的候选列表与
//! 「选 exe 路径」选择器已删除。
//!
//! # 属主窗口（[`shell::owner_window`]）为什么必须给
//!
//! 同一批实测：**无属主**时残缺关联（`.jpg`：ProgID「照片」在册、可执行命令缺失）的
//! `open` 从 GUI 进程发出**可能什么都不发生**（用户报「输入密码后没继续打开，重新双击
//! 才行」）；**带属主**后同一调用**稳定弹出系统询问框**（连测 2 次，1s 内出现），且询问
//! 框挂在主窗口之上（正确的从属关系）。
//!
//! 「打开」/双击仍走 [`open_with_default_app`]：该扩展名**没有可用默认程序**时，
//! Windows 自己会弹系统询问框——正是用户口径 2026-09-28 要的「不知道打开方式就问」。

use std::io;
use std::path::Path;

/// Windows `ShellExecute*` 薄封装（verb 调用 / 属主窗口 / COM 套间）。
///
/// 收口三件事：① 返回码语义（`ShellExecuteW` ≤ 32 是 **SE_ERR_\***，不是 Win32
/// 错误码——旧实现拿 `last_os_error` 当原因，实测会打印无关的 1155 掩盖真因）；
/// ② 调用必带**属主窗口**（见模块文档：不给属主时系统询问框可能根本不出现）；
/// ③ 调用线程的 COM 套间（shell 的 ID-list/`IContextMenu` 通道要求）。
#[cfg(windows)]
mod shell {
    use std::io;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetWindow, GetWindowPlacement, GetWindowRect,
        GetWindowThreadProcessId, IsWindowVisible, SW_SHOWNORMAL, WINDOWPLACEMENT,
    };
    use windows_sys::core::BOOL;

    /// `ShellExecuteW` 失败：返回值 ≤ 32 即 **SE_ERR_\*** 码。
    #[derive(Debug, Clone, Copy)]
    pub(super) struct ShellErr(pub usize);

    impl ShellErr {
        /// 转 `io::Error`：**保留 SE_ERR 码**（旧实现只打印 `last_os_error`，
        /// 把无关的 1155 当原因，掩盖真因——用户实测留痕即为此）。
        pub(super) fn into_io(self) -> io::Error {
            let last = io::Error::last_os_error();
            io::Error::new(
                last.kind(),
                format!("ShellExecute 失败（SE_ERR {}；系统错误 {last}）", self.0),
            )
        }
    }

    /// 本进程「主窗口」句柄（交给 shell 作属主）；无窗口进程 → **空句柄**。
    ///
    /// 为什么必须给（模块文档有实测表）：无属主时，残缺关联类型的 `open` 从 GUI
    /// 进程发出**可能什么都不发生**（用户报「输入密码后没继续打开」）；带属主后同一
    /// 调用**稳定弹出系统询问框**，且询问框挂在主窗口之上（正确从属关系）。
    pub(super) fn owner_window() -> HWND {
        largest_ownerless_window(std::process::id())
    }

    /// 指定进程的「主窗口」＝**可见、无属主、顶层**窗口中面积最大者。
    ///
    /// 两条踩过的坑（2026-09-28 对真实 hzip-gui 实例实测，非推测）：
    ///
    /// 1. **不能按标题过滤**：hzip 主窗口标题为空（gpui 未设标题），按「有标题」
    ///    过滤会一个都选不到 → 属主又变回空句柄，修复失效。
    /// 2. **不能直接用 `GetWindowRect` 比面积**：主窗口最小化时它只有任务栏占位
    ///    尺寸（实测 160×28），会被任何小对话框压过去——故用
    ///    `GetWindowPlacement(...).rcNormalPosition`（还原尺寸，实测 1196×768）。
    ///
    /// `pid` 作参数（而非直接取 `std::process::id()`）是为了**能被测试**：可用真实
    /// 运行实例的 pid 校验选择规则（见 `owner_window_tests`）。
    pub(super) fn largest_ownerless_window(pid: u32) -> HWND {
        struct Pick {
            best: HWND,
            area: i64,
            pid: u32,
        }
        extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
            let st = unsafe { &mut *(l as *mut Pick) };
            if unsafe { IsWindowVisible(h) } == 0 {
                return 1;
            }
            let mut wpid: u32 = 0;
            unsafe { GetWindowThreadProcessId(h, &mut wpid) };
            if wpid != st.pid {
                return 1;
            }
            // 有属主的窗（提示窗/进度窗/对话框）排除，只留顶层主窗口
            if !unsafe { GetWindow(h, GW_OWNER) }.is_null() {
                return 1;
            }
            // 还原尺寸优先：最小化窗口的 GetWindowRect 是任务栏占位（见文档）
            let mut wp: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
            wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
            let (w, hgt) = if unsafe { GetWindowPlacement(h, &mut wp) } != 0 {
                (
                    wp.rcNormalPosition.right - wp.rcNormalPosition.left,
                    wp.rcNormalPosition.bottom - wp.rcNormalPosition.top,
                )
            } else {
                let mut r: RECT = unsafe { std::mem::zeroed() };
                if unsafe { GetWindowRect(h, &mut r) } == 0 {
                    return 1;
                }
                (r.right - r.left, r.bottom - r.top)
            };
            let area = w as i64 * hgt as i64;
            if area > st.area {
                st.best = h;
                st.area = area;
            }
            1
        }
        let mut st = Pick {
            best: std::ptr::null_mut(),
            area: 0,
            pid,
        };
        // SAFETY: 回调只写 `st`（其生命周期覆盖整个调用）；`lpData` 指向它。
        unsafe { EnumWindows(Some(cb), &mut st as *mut _ as LPARAM) };
        st.best
    }

    /// verb 打开路径（`open` = 默认程序；目录用 `explore`）。
    pub(super) fn exec(verb: &str, path: &Path) -> Result<(), ShellErr> {
        ensure_com_initialized();
        let v: Vec<u16> = crate::winutil::wide_str(verb);
        let w: Vec<u16> = crate::winutil::wide_path(path);
        // SAFETY: 宽字符串 NUL 终止；返回值 ≤ 32 表示失败（SE_ERR_*）。
        let r = unsafe {
            ShellExecuteW(
                owner_window(),
                v.as_ptr(),
                w.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if r as usize <= 32 {
            Err(ShellErr(r as usize))
        } else {
            Ok(())
        }
    }

    /// 用**指定程序**打开目标（lpFile = 程序、lpParameters = 目标）。
    pub(super) fn exec_program(program: &str, target: &Path) -> Result<(), ShellErr> {
        ensure_com_initialized();
        let prog: Vec<u16> = crate::winutil::wide_str(program);
        let t: Vec<u16> = crate::winutil::wide_path(target);
        // SAFETY: 宽字符串 NUL 终止；返回值 ≤ 32 表示失败（SE_ERR_*）。
        let r = unsafe {
            ShellExecuteW(
                owner_window(),
                std::ptr::null(), // 默认 verb
                prog.as_ptr(),
                t.as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if r as usize <= 32 {
            Err(ShellErr(r as usize))
        } else {
            Ok(())
        }
    }

    /// （调用方已按别的套间模型初始化）不影响后续调用，故忽略返回值。
    /// 系统「打开方式」询问框（`SHOpenWithDialog`；**必须从非属主线程调用**）。
    ///
    /// 实测（Win10 19045，2026-09-28，两条对照都跑过）：
    ///
    /// | 调用线程 | 结果 |
    /// |---|---|
    /// | = 属主窗口线程（调用时自己没法泵消息） | 立即 `ERROR_CANCELLED`，什么都不出现 |
    /// | **≠ 属主窗口线程**（属主线程继续泵消息） | **`S_OK` + 询问框出现**（标题=文件名，类 `WinUIDesktopWin32WindowClass`） |
    ///
    /// hzip 的打开流程正好是后者：`run_open_flow` 把重活放 `hzip-open` 工作线程，
    /// gpui 主线程（属主窗口所在线程）持续泵消息。
    ///
    /// 语义：**阻塞**到用户选定或取消。`OAIF_EXEC` → 选定即由 shell 执行该程序；
    /// 取消 → `HRESULT_FROM_WIN32(ERROR_CANCELLED)`，归一为 `Ok(())`（用户取消不是错误）。
    pub(super) fn open_with_dialog(path: &Path) -> io::Result<()> {
        use windows_sys::Win32::UI::Shell::{
            OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OPENASINFO, SHOpenWithDialog,
        };
        let file = crate::winutil::wide_path(path);
        let info = OPENASINFO {
            pcszFile: file.as_ptr(),
            pcszClass: std::ptr::null(),
            oaifInFlags: OAIF_EXEC | OAIF_ALLOW_REGISTRATION,
        };
        // SAFETY: 结构体按文档填充（NUL 终止路径 + 真实属主窗口）。
        let hr = unsafe { SHOpenWithDialog(owner_window(), &info) };
        if hr >= 0 {
            Ok(())
        } else if hr as u32 == 0x8007_04C7 {
            // HRESULT_FROM_WIN32(ERROR_CANCELLED)：用户关闭询问框 = 放弃本次打开
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "系统「打开方式」询问框失败（HRESULT 0x{:08X}）",
                hr as u32
            )))
        }
    }

    pub(crate) fn ensure_com_initialized() {
        use std::cell::Cell;
        use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        thread_local! {
            static DONE: Cell<bool> = const { Cell::new(false) };
        }
        DONE.with(|d| {
            if !d.get() {
                // SAFETY: 仅当前线程首次调用；结果忽略（见本函数文档）。
                unsafe {
                    CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
                }
                d.set(true);
            }
        });
    }
}

/// 供 crate 内其它 shell 调用点复用（T-ICON：`SHGetFileInfoW` 前须初始化套间）。
#[cfg(windows)]
pub(crate) use shell::ensure_com_initialized;

/// 用系统默认程序打开路径（02 §5.3.7）。
///
/// - **win**：`ShellExecuteW` verb `open`（目录 verb `explore`），`SW_SHOWNORMAL`，
///   属主 = [`shell::owner_window`]。该扩展名**没有可用默认程序**时，系统自己弹
///   「你要如何打开这个文件？」询问框（用户口径 2026-09-28：不知道打开方式就问）。
/// - **macOS**：`Command::new("open").arg(path)`。
/// - **Linux**：`xdg-open`（未安装时错误信息含「未安装 xdg-utils」提示）。
pub fn open_with_default_app(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let verb = if path.is_dir() { "explore" } else { "open" };
        shell::exec(verb, path).map_err(shell::ShellErr::into_io)
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        cmd.arg(path);
        cmd.status()?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if !which("xdg-open") {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "未安装 xdg-utils（xdg-open 缺失），无法用系统默认程序打开（02 §5.3.7）",
            ));
        }
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(path);
        let st = cmd.status()?;
        if st.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "xdg-open 失败：{}",
                st.code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "无退出码".into())
            )))
        }
    }
}

/// 用**用户指定程序**打开目标（2026-09-26；`[ui].image_viewer`/`[ui].editor`
/// 非空时取代 [`open_with_default_app`]——GUI 不直接持 winutil，平台收口）。
/// 「打开方式…」当场选定的程序也走本入口。
///
/// - **win**：`ShellExecuteW`，文件 = 程序路径、parameters = 目标（与 verb
///   `open` 打开目标同机制），属主 = [`shell::owner_window`]。
/// - **macOS**：`open -a <程序> <目标>`。
/// - **Linux**：直接 exec `<程序> <目标>`（显式指定，不走 xdg 探测）。
pub fn open_with_program(target: &Path, program: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        shell::exec_program(program, target).map_err(shell::ShellErr::into_io)
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        cmd.arg("-a").arg(program).arg(target);
        let st = cmd.status()?;
        if st.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "open -a 失败：{}",
                st.code().map(|c| c.to_string()).unwrap_or_default()
            )))
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut cmd = std::process::Command::new(program);
        cmd.arg(target);
        let st = cmd.status()?;
        if st.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{program} 失败：{}",
                st.code().map(|c| c.to_string()).unwrap_or_default()
            )))
        }
    }
}

/// 在文件管理器中显示路径（02 §5.3.7）。
///
/// - **win**：`select=true` → `explorer.exe /select,"path"`；否则 `explorer.exe dir`。
/// - **macOS**：`select=true` → `open -R path`；否则 `open 目录`（首层实现；
///   Finder 精确选中用 objc `NSWorkspace activateFileViewerSelectingURLs:` 为增强分支）。
/// - **Linux**：优先 dbus `org.freedesktop.FileManager1` `ShowItems`（dbus-send/busctl
///   探测调用），回退 `xdg-open 父目录`。
pub fn reveal_in_file_manager(path: &Path, select: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            UI::Shell::ShellExecuteW, UI::WindowsAndMessaging::SW_SHOWNORMAL,
        };
        if select {
            let params = format!("/select,\"{}\"", path.display());
            let p: Vec<u16> = crate::winutil::wide_str(&params);
            let exe: Vec<u16> = crate::winutil::wide_str("explorer.exe");
            // SAFETY: 宽字符串 NUL 终止；返回值 ≤ 32 表示错误。
            let r = unsafe {
                ShellExecuteW(
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    exe.as_ptr(),
                    p.as_ptr(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                )
            };
            if r as usize <= 32 {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let w: Vec<u16> = crate::winutil::wide_path(dir);
        let verb: Vec<u16> = crate::winutil::wide_str("explore");
        // SAFETY: 宽字符串 NUL 终止；explorer.exe 打开目录。
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                w.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if r as usize <= 32 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        // open -R：在 Finder 中定位并选中。
        // `select=false` → 打开所在目录（与 win `explore` / linux `xdg-open 父目录`
        // 同语义：定位到目录而不选中条目）。
        let mut cmd = std::process::Command::new("open");
        if select {
            cmd.arg("-R").arg(path);
        } else {
            let dir = if path.is_dir() {
                path
            } else {
                path.parent().unwrap_or(path)
            };
            cmd.arg(dir);
        }
        let st = cmd.status()?;
        if st.success() {
            return Ok(());
        }
        // 增强分支（objc NSWorkspace activateFileViewerSelectingURLs:）阶段 B 实现
        Err(io::Error::other("open 失败（Finder 定位失败）"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if reveal_via_dbus(path) {
            return Ok(());
        }
        let parent = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        if !which("xdg-open") {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "未安装 xdg-utils（xdg-open 缺失），无法在文件管理器中显示（02 §5.3.7）",
            ));
        }
        let st = std::process::Command::new("xdg-open")
            .arg(parent)
            .status()?;
        if st.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "xdg-open 失败：{}",
                st.code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "无退出码".into())
            )))
        }
    }
}

/// 系统「打开方式」询问框（用户口径 2026-09-28：要资源管理器那样的**系统原生询问
/// 窗口**，不要自实现的程序列表）。
///
/// - **win**：`SHOpenWithDialog`，属主 = [`shell::owner_window`]，**在调用线程上
///   阻塞**到用户选定/取消（本函数由 `hzip-open` 工作线程调用——见
///   [`shell::open_with_dialog`] 的线程模型实测表：同属主线程调用会失败）。
/// - **macOS/Linux**：无等价系统询问框 → 返回明确错误（不静默退化，调用方据此提示）。
pub fn open_with_picker(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        shell::open_with_dialog(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(io::Error::other(
            "本平台没有系统级「打开方式」询问框（请在设置 ▸ 查看器里指定程序）",
        ))
    }
}

/// Linux reveal 优先走 dbus `org.freedesktop.FileManager1.ShowItems`（02 §5.3.7）。
/// 探测 `dbus-send` 或 `busctl`，调用成功返回 true。
#[cfg(all(unix, not(target_os = "macos")))]
fn reveal_via_dbus(path: &Path) -> bool {
    let uri = format!("file://{}", path.display());
    let args = [
        "--session".to_string(),
        "--dest=org.freedesktop.FileManager1".to_string(),
        "--type=method_call".to_string(),
        "/org/freedesktop/FileManager1".to_string(),
        "org.freedesktop.FileManager1.ShowItems".to_string(),
        format!("array:string:{uri}"),
        "string:".to_string(),
    ];
    if which("dbus-send") {
        let st = std::process::Command::new("dbus-send").args(&args).status();
        if matches!(st, Ok(s) if s.success()) {
            return true;
        }
    }
    if which("busctl") {
        let st = std::process::Command::new("busctl")
            .args([
                "--user",
                "call",
                "org.freedesktop.FileManager1",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1",
                "ShowItems",
                "as",
                "s",
                &uri,
                "",
            ])
            .status();
        if matches!(st, Ok(s) if s.success()) {
            return true;
        }
    }
    false
}

/// PATH 探测可执行文件是否存在（不引入 `which` crate）。
#[cfg(all(unix, not(target_os = "macos")))]
fn which(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .any(|p| p.is_file())
}

#[cfg(all(test, windows))]
mod owner_window_tests {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, SW_MINIMIZE, SW_SHOW, ShowWindow, WS_OVERLAPPEDWINDOW,
        WS_VISIBLE,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 建一个顶层窗口（`owner` 非空 = 对话框语义；`visible=false` = 隐藏窗）。
    fn mk(title: &str, w: i32, h: i32, owner: Option<HWND>, visible: bool) -> HWND {
        let cls = wide("STATIC");
        let t = wide(title);
        let style = if visible {
            WS_OVERLAPPEDWINDOW | WS_VISIBLE
        } else {
            WS_OVERLAPPEDWINDOW
        };
        // SAFETY: 预定义窗口类 + NUL 终止字符串；句柄由调用方 DestroyWindow 回收。
        unsafe {
            CreateWindowExW(
                0,
                cls.as_ptr(),
                t.as_ptr(),
                style,
                0,
                0,
                w,
                h,
                owner.unwrap_or(std::ptr::null_mut()),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        }
    }

    /// 属主窗口选择规则（`open.rs` 实测坑的回归锚点）：**可见 + 无属主**的顶层窗中
    /// 取面积最大者 —— 主窗口（大）胜过对话框（小，且有属主），隐藏窗一律不选；
    /// **主窗口最小化后仍须选它**（面积按 `GetWindowPlacement` 的还原尺寸算，
    /// 否则它只有任务栏占位 160×28，会被小对话框压过去）。
    #[test]
    fn largest_ownerless_window_prefers_main_window() {
        use super::shell::largest_ownerless_window;
        let main = mk("hzip-test-main", 800, 500, None, true);
        let dialog = mk("hzip-test-dialog", 200, 100, Some(main), true);
        let hidden = mk("hzip-test-hidden", 1600, 900, None, false);
        assert!(
            !main.is_null() && !dialog.is_null() && !hidden.is_null(),
            "窗口创建失败"
        );
        // SAFETY: 句柄均来自上面的 CreateWindowExW。
        unsafe { ShowWindow(main, SW_SHOW) };
        let pid = std::process::id();
        assert_eq!(
            largest_ownerless_window(pid),
            main,
            "应选中可见、无属主的最大窗口（对话框有属主、隐藏窗不可见，都被排除）"
        );
        // 最小化后仍应选中主窗口（还原尺寸 800×500 ＞ 对话框 200×100）
        // SAFETY: 同上。
        unsafe { ShowWindow(main, SW_MINIMIZE) };
        assert_eq!(
            largest_ownerless_window(pid),
            main,
            "主窗口最小化后仍须是属主（面积按还原尺寸算，不用任务栏占位尺寸）"
        );
        // SAFETY: 回收本用例自建的窗口（无消息泵，销毁即刻生效）。
        for h in [hidden, dialog, main] {
            unsafe { DestroyWindow(h) };
        }
    }
}
