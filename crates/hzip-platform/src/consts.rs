//! 编译期常量（02 附录 B.2，本 crate 全量清单与出处）。

/// 编辑变更去抖窗口（02 §5.3.6 / 附录 B.2 `EDIT_DEBOUNCE_MS`；TC-CORE-019）。
pub const EDIT_DEBOUNCE_MS: u64 = 500;

/// 会话临时目录名（02 附录 B.2 `TEMP_SESSION_DIR` = `{tmp}/hzip/{uuid}`：
/// 本常量即其中 `hzip` 目录名）。
pub const TEMP_SESSION_DIR: &str = "hzip";

/// 档案路径哈希目录名的截断长度（02 附录 B.2 `TEMP_ENTRY_HASH_LEN`：
/// `{root}/{档案路径 sha256 前 8 字符}/{条目安全名}`）。
pub const TEMP_ENTRY_HASH_LEN: usize = 8;

/// stdin 是否为交互终端（02 §10.2 F 节点：密码/冲突交互通道判 TTY 用）。
///
/// 平台收口（红线 5）：CLI/GUI 一律经本函数判定，不直接触碰平台 crate。
/// Windows：`GetConsoleMode` 于 stdin 句柄（成功即控制台）；unix：
/// `isatty(0)`。
pub fn stdin_is_tty() -> bool {
    #[cfg(windows)]
    {
        use std::io::IsTerminal as _;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Console::GetStdHandle;
        use windows_sys::Win32::System::Console::{GetConsoleMode, STD_INPUT_HANDLE};
        std::io::stdin().is_terminal()
            && unsafe {
                let mut mode: u32 = 0;
                let h = GetStdHandle(STD_INPUT_HANDLE);
                h != INVALID_HANDLE_VALUE && GetConsoleMode(h, &mut mode) != 0
            }
    }
    #[cfg(not(windows))]
    {
        use std::io::IsTerminal as _;
        std::io::stdin().is_terminal()
    }
}
