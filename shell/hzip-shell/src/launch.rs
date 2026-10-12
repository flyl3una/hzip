//! 启动 `hzip-gui`（`Invoke` 的实际动作）。
//!
//! # 为什么不用 `Command::new`
//!
//! 本 DLL 运行在 **Explorer 进程内**（经典路径）或 dllhost 代理（稀疏包路径）。
//! 从这些进程里 `fork/exec` 一个 GUI 应用：子进程会继承 shell 的句柄与
//! 环境，且 `CreateProcessW` 的参数构造需要正确处理引号——这是 Windows
//! 上最容易出错的一环（路径含空格、路径含引号）。
//!
//! 故这里**显式构造命令行字符串**并交给 `CreateProcessW`，创建标志只设
//! `CREATE_NO_WINDOW`（详见 `spawn` 内注释），让 GUI 正常显示自己的窗口。
//!
//! # 为什么要 `AllowSetForegroundWindow`（前台锁）
//!
//! Windows 的前台锁规定：**新进程默认无权**把自己提到前台。于是右键解压/压缩
//! 弹出的输出窗会落在其它窗口后面，用户看到的是「点了没反应」，还要自己去点
//! 任务栏——实测症状。
//!
//! 前台锁有一条出口：**当前前台进程**可以把自己的前台权授给别的进程。用户点
//! 右键菜单时前台正是 Explorer（本 DLL 的宿主），故在 `CreateProcessW` 之前
//! 调用 `AllowSetForegroundWindow(ASFW_ANY)`，把前台权交给即将启动的 GUI——
//! 它那边的 `activate_window()`（`SetForegroundWindow`）才真正生效。
//! 宿主不是前台进程时（如 dllhost 代理路径）该调用失败无害。
//!
//! # 命令行引号规则
//!
//! 按 `argv` 语义（而非 shell 语义）拼接：每个参数都要能被
//! `CommandLineToArgvW` 反解回原值。规则（MS 文档 "Parsing C++ command-line
//! arguments"）：
//!
//! - 无空格/制表符/引号 → 原样；
//! - 否则用双引号包裹，内部的 `"` 前面加反斜杠；
//! - 引号前的连续反斜杠要翻倍。

use std::ffi::OsStr;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;

/// 把参数列表拼成一条 `CreateProcessW` 可接受的命令行。
///
/// 纯函数，便于单测（引号规则错一个字符就会把带空格的路径切碎）。
pub fn build_command_line(exe: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(quote_arg(exe));
    for a in args {
        parts.push(quote_arg(a));
    }
    parts.join(" ")
}

/// 按 `CommandLineToArgvW` 规则引用单个参数。
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| c == ' ' || c == '\t' || c == '"' || c == '\n')
    {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                // 引号前的反斜杠翻倍，再转义引号本身
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push_str("\\\"");
                backslashes = 0;
            }
            other => {
                backslashes = 0;
                out.push(other);
            }
        }
    }
    // 收尾引号前的反斜杠也要翻倍
    for _ in 0..backslashes {
        out.push('\\');
    }
    out.push('"');
    out
}

/// 启动 `hzip-gui`（不等待；GUI 自己管理窗口生命周期）。
pub fn spawn(exe: &str, args: &[String]) -> Result<(), String> {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{
        CreateProcessW, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
    };

    // 把前台权预授给即将启动的 GUI（理由见模块文档「前台锁」）。必须在
    // CreateProcessW 之前调用：授权对象是「接下来要创建的进程」。
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
        // 失败无害（宿主不是前台进程，如 dllhost 代理路径）：GUI 侧仍会尽力
        // activate_window()，只是可能被前台锁挡回。
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    let cmdline = build_command_line(exe, args);
    let mut wide: Vec<u16> = OsStr::new(&cmdline).encode_wide().chain(once(0)).collect();

    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    // SAFETY: 命令行缓冲以 NUL 结尾且在本调用期间存活并可变；
    // 未传环境块（继承 Explorer 的）、未传工作目录（继承）、无继承句柄。
    // CREATE_NO_WINDOW：正常构建下 hzip-gui 是 GUI 子系统（本就没有控制台），
    // 这是兜底——万一跑的是控制台子系统构建，也不会在用户眼前闪一个黑窗。
    let ok = unsafe {
        CreateProcessW(
            None,
            Some(PWSTR(wide.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            None,
            &si,
            &mut pi,
        )
    };
    if ok.is_err() {
        return Err(format!(
            "CreateProcessW 失败: {}",
            windows::core::Error::from_thread()
        ));
    }
    // 立即关闭句柄：我们不等待子进程（GUI 独立存活）
    // SAFETY: 两个句柄均由上面的 CreateProcessW 产出且未被他人关闭
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(pi.hThread);
        let _ = windows::Win32::Foundation::CloseHandle(pi.hProcess);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn plain_args_are_not_quoted() {
        assert_eq!(quote_arg("--extract-here"), "--extract-here");
        assert_eq!(quote_arg("zip"), "zip");
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        assert_eq!(
            quote_arg(r"C:\Program Files\hzip"),
            r#""C:\Program Files\hzip""#
        );
        assert_eq!(quote_arg(r"D:\a b\c.zip"), r#""D:\a b\c.zip""#);
    }

    #[test]
    fn empty_arg_becomes_empty_quotes() {
        assert_eq!(quote_arg(""), r#""""#);
    }

    #[test]
    fn embedded_quote_is_escaped_with_doubled_backslashes() {
        // 结尾反斜杠 + 引号：\' 翻倍后转义引号
        assert_eq!(quote_arg(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_arg(r#"x\"y"#), r#""x\\\"y""#);
    }

    #[test]
    fn trailing_backslashes_before_closing_quote_are_doubled() {
        // 这是最经典的坑：路径以 \ 结尾且含空格时必须翻倍，否则收尾引号被吃掉
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
    }

    #[test]
    fn full_command_line_keeps_single_path_intact() {
        let line = build_command_line(
            r"C:\Program Files\hzip\hzip-gui.exe",
            &v(&["--extract-here", r"D:\my archives\a b.zip"]),
        );
        assert_eq!(
            line,
            r#""C:\Program Files\hzip\hzip-gui.exe" --extract-here "D:\my archives\a b.zip""#
        );
    }

    #[test]
    fn multiple_paths_stay_separate_argv_entries() {
        let line = build_command_line(
            "hzip-gui.exe",
            &v(&[
                "--compress",
                r"C:\a\one.txt",
                r"C:\a\two three.txt",
                "--format",
                "zip",
            ]),
        );
        assert_eq!(
            line,
            r#"hzip-gui.exe --compress C:\a\one.txt "C:\a\two three.txt" --format zip"#
        );
    }
}
