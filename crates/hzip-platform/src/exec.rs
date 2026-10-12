//! 外部查看器/编辑器进程派生与脱离（02 §5.3.7）。
//!
//! | 平台 | 实现 |
//! |---|---|
//! | Windows | `creation_flags(DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP)` |
//! | Unix | `pre_exec` 中 `libc::setsid()`，三路 stdio 均 `Stdio::null()` |
//!
//! 语义：不等待退出、句柄不继承（编辑器长寿，hzip 退出不杀子进程）。

use std::ffi::OsStr;
use std::io;

/// 脱离当前进程派生外部程序（查看/编辑器，02 §5.3.7）。
///
/// 不等待退出；返回 `Ok(())` 表示派生成功。子进程与 hzip 生命周期解耦。
pub fn spawn_detached(cmd: &OsStr, args: &[&OsStr]) -> io::Result<()> {
    let mut command = std::process::Command::new(cmd);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid 仅设置会话，无内存操作
        unsafe {
            command.pre_exec(|| {
                let r = libc::setsid();
                if r == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    let child = command.spawn()?;
    // 不 wait：句柄 Drop 即脱离（unix waitpid 不调用；win 句柄关闭）
    drop(child);
    Ok(())
}

#[cfg(test)]
#[path = "tests/exec.rs"]
mod tests;
