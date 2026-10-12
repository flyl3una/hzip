//! 键盘修饰键硬件态查询（T-SEL 走查 #1 兜底层）。
//!
//! 背景：gpui-pre-windows 0.3.8 的鼠标 `MouseDownEvent.modifiers` 仍取自
//! `GetKeyState`（`events.rs:503/1071` 填 `current_modifiers()`，其实现即
//! `events.rs:1755` 的 `GetKeyState`）（**线程消息队列语义**——调用线程须已
//! 读取对应 WM_KEYDOWN 才可见）。焦点切换/输入队列竞态下修饰键可能取空
//! （表现为 Ctrl/Shift+点击多选失效）。`GetAsyncKeyState` 是
//! **物理硬件态**（线程无关），作为鼠标修饰键的兜底信号源。

/// 当前物理修饰键状态（线程无关硬件态）。
///
/// 返回 `(ctrl, shift, alt)`；非 Windows 平台返回全 false
/// （该兜底仅针对 Windows 队列语义）。
#[cfg(windows)]
pub fn async_key_modifiers() -> (bool, bool, bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT,
    };
    // SAFETY: GetAsyncKeyState 是纯查询（无句柄/缓冲区）
    unsafe {
        (
            GetAsyncKeyState(VK_CONTROL as i32) < 0,
            GetAsyncKeyState(VK_SHIFT as i32) < 0,
            GetAsyncKeyState(VK_MENU as i32) < 0,
        )
    }
}

#[cfg(not(windows))]
pub fn async_key_modifiers() -> (bool, bool, bool) {
    (false, false, false)
}

#[cfg(test)]
#[path = "tests/kb.rs"]
mod tests;
