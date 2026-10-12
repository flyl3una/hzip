//! 控制台附着：Windows GUI 子系统程序从终端被调用时的输出通路。
//!
//! `hzip-gui` 编为 `windows_subsystem = "windows"` 后自己**不持有**控制台——
//! 这正是不再闪出「命令行窗口」的原因，但 `println!` 也就没有了去处：从终端
//! 跑 `hzip-gui --help/--version` 会变成静默无输出。本模块用
//! `AttachConsole(ATTACH_PARENT_PROCESS)` 挂到调用者（cmd / PowerShell /
//! Windows Terminal）的控制台上，再往 `CONOUT$` 写出，使「双击启动不闪窗」
//! 与「终端启动看得到输出」同时成立。
//!
//! 返回 `None` 是**正常路径**而非异常：
//!
//! - 资源管理器 / dllhost 拉起（双击、右键菜单）时父进程没有控制台，
//!   `AttachConsole` 返回 0；
//! - 进程已持有控制台时同样返回 0（一个进程只能有一个控制台）。
//!
//! 调用方在 `None` 时静默降级：既然没有输出目标，就**不得**为了报错再
//! `AllocConsole` 弹一个黑窗——那正是本次要消灭的东西。
//!
//! 写出走 `WriteConsoleW` 而非 `WriteFile`：控制台输出代码页不一定是 UTF-8
//! （中文 Windows 常见 936），`WriteFile` 按字节直送会乱码；`WriteConsoleW`
//! 直收 UTF-16，与代码页无关。句柄则借 `std::fs` 打开 `CONOUT$` 取得，省掉
//! 手写 `CreateFileW` 的共享/属性标志，并由 `File` 的 Drop 负责关闭。

/// 附着父进程控制台后得到的写出句柄。
///
/// 由 [`attach_parent_console`] 构造；无父控制台（资源管理器/双击启动）或
/// 非 Windows 时根本拿不到实例。
#[cfg(windows)]
pub struct ConsoleWriter {
    /// `CONOUT$` 句柄；`File` 持有所有权，Drop 时关闭。
    out: std::fs::File,
}

/// 非 Windows 构建的同名占位（`attach_parent_console` 恒 `None`，不可构造）。
#[cfg(not(windows))]
pub struct ConsoleWriter {
    /// 永不可达；字段以下划线开头，避免 dead_code 噪声。保留它是为了让类型在
    /// 两种目标下同名同形，调用方无需 `cfg`。
    _unreachable: (),
}

impl ConsoleWriter {
    /// 写一行（自动补行尾）。
    pub fn write_line(&self, s: &str) {
        #[cfg(windows)]
        {
            self.raw_write(s);
            self.raw_write("\r\n");
        }
        #[cfg(not(windows))]
        let _ = s;
    }

    /// 原样写（不补换行；文案自带的换行照常生效）。
    pub fn write(&self, s: &str) {
        #[cfg(windows)]
        self.raw_write(s);
        #[cfg(not(windows))]
        let _ = s;
    }

    /// 实际的写出：UTF-16 直送控制台，失败静默。
    #[cfg(windows)]
    fn raw_write(&self, s: &str) {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::System::Console::WriteConsoleW;

        // 控制台 API 不把裸 LF 当换行——LF→CRLF 的文本转换只发生在 WriteFile
        // （ENABLE_PROCESSED_OUTPUT）那条路径上，libuv/CPython 为此都在 TTY 写出
        // 前自行补 CR。而 `--help` 的用法文案自带 `\n` 对齐换行，不补就会呈现
        // 阶梯状右移，故在此统一把「前面不是 CR 的 LF」补成 CRLF。
        let mut buf: Vec<u16> = Vec::with_capacity(s.len() + 8);
        let mut prev_cr = false;
        for ch in s.chars() {
            if ch == '\n' && !prev_cr {
                buf.push(b'\r' as u16);
            }
            prev_cr = ch == '\r';
            let mut unit = [0u16; 2];
            buf.extend_from_slice(ch.encode_utf16(&mut unit));
        }

        let mut offset = 0usize;
        while offset < buf.len() {
            let mut written = 0u32;
            // SAFETY: 句柄来自本进程已附着的控制台的 `CONOUT$`；`buf[offset..]`
            // 是指向本函数栈上缓冲的有效切片；`written` 是有效出参。
            let ok = unsafe {
                WriteConsoleW(
                    self.out.as_raw_handle(),
                    buf[offset..].as_ptr(),
                    (buf.len() - offset) as u32,
                    &mut written,
                    std::ptr::null(),
                )
            };
            // 写不动（句柄失效、控制台被抢占…）：静默放弃。此刻没有别的输出
            // 通路，为一个 --help 的写失败再弹控制台是本末倒置。
            if ok == 0 || written == 0 {
                return;
            }
            offset += written as usize;
        }
    }
}

/// 附着到**父进程**控制台（Windows GUI 子系统程序输出 `--help`/`--version` 用）。
///
/// 无父控制台（资源管理器/双击启动）→ `None`。非 Windows 恒 `None`。
pub fn attach_parent_console() -> Option<ConsoleWriter> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

        // SAFETY: 本调用无指针参数；返回 0 表示失败——无父控制台或进程已有
        // 控制台，两者都是正常路径（见模块文档），静默返回 None。
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
            return None;
        }
        // 附着成功后再打开 CONOUT$：此刻它解析到刚挂上的那个控制台。
        // 打开失败（理论不该发生）同样静默 None——调用方本来就要处理 None。
        let out = std::fs::OpenOptions::new()
            .write(true)
            .open("CONOUT$")
            .ok()?;
        Some(ConsoleWriter { out })
    }
    #[cfg(not(windows))]
    {
        None
    }
}
