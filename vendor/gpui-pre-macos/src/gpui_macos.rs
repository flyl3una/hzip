#![cfg(target_os = "macos")]
//! macOS platform implementation for GPUI.
//!
//! macOS screens have a y axis that goes up from the bottom of the screen and
//! an origin at the bottom left of the main display.
//!
//! # vendor 补丁：macOS 关窗崩溃（必须重放）
//!
//! 本副本 = crates.io `gpui-pre-macos` **0.3.8 未改动源码 + 我们的一处产物补丁**（只动
//! `window.rs`）。补丁成因：上游用 `accesskit_macos::SubclassingAdapter` 承载无障碍，它
//! 在窗口创建路径里用 `object_setClass` 换掉内容视图（`GPUIView`）的类、Drop 时再换回，
//! 这会破坏 AppKit 触摸栏（`_NSTouchBarFinder`）建在该视图上的 KVO，触控栏机型关窗时
//! 在 display-cycle flush 里抛未捕获异常 → `SIGILL`。补丁**必须保留**（用户裁定）。
//!
//! 升级基线时，请以 crates.io 新版本 `src/window.rs` 为底重放以下四处（其余文件保持
//! 逐字节等于上游，勿手改）：
//!   1. `accesskit_macos::Adapter::new(native_view, false, action_handler)` 取代上游
//!      `SubclassingAdapter` 的 `for_window` 构造，适配器连同 `A11yActivationHandler` 存进
//!      `MacWindowState.accesskit: Option<Rc<RefCell<AccessKitState>>>`；
//!   2. `GPUIView` 自己实现 `accessibilityChildren` / `accessibilityFocusedUIElement` /
//!      `accessibilityHitTest:`（`build_classes` 里 `add_method`），经
//!      `with_accesskit_state` 转发给适配器，取不到状态时回落 `super` 的 `NSView` 默认实现；
//!   3. `a11y_tree_update` / `window_did_change_key_status` 里对 AccessKit 状态一律
//!      `try_lock` + `try_borrow_mut`（AppKit 可能在 GPUI 持锁时重入查询，宁可丢弃一次
//!      更新也不能死锁/panic）；
//!   4. `Drop for MacWindow` 中 `this.accesskit.take()`（`Adapter` 不再 strong-retain
//!      内容视图，只是让 AppKit 收尾期间的查询回落到默认实现）。
//!
//! 上一个基线时代的旧补丁（直接用 `SubclassingAdapter`）已废弃，勿照抄。

mod display;
mod display_link;
mod events;
mod keyboard;
mod pasteboard;
mod system_notifications;

#[cfg(feature = "screen-capture")]
mod screen_capture;

use gpui_apple::metal_renderer as renderer;

pub mod metal_renderer {
    pub use gpui_apple::metal_renderer::{PathRasterizationVertex, PathSprite, SurfaceBounds};

    #[cfg(any(test, feature = "bench-support", feature = "test-support"))]
    pub use gpui_apple::metal_renderer::MetalHeadlessRenderer;
}

#[cfg(feature = "font-kit")]
mod open_type;

#[cfg(feature = "font-kit")]
mod text_system;

mod platform;
mod window;
mod window_appearance;

use cocoa::{
    base::{id, nil},
    foundation::{NSAutoreleasePool, NSNotFound, NSString, NSUInteger},
};

use objc::runtime::{BOOL, NO, YES};
use std::{
    ffi::{CStr, c_char},
    ops::Range,
};

pub(crate) use display::*;
pub(crate) use display_link::*;
pub(crate) use gpui_apple::{AppleActivity, AppleDispatcher};
pub(crate) use keyboard::*;
pub(crate) use platform::*;
pub(crate) use window::*;

#[cfg(feature = "font-kit")]
pub(crate) use text_system::*;

pub use platform::MacPlatform;

trait BoolExt {
    fn to_objc(self) -> BOOL;
}

impl BoolExt for bool {
    fn to_objc(self) -> BOOL {
        if self { YES } else { NO }
    }
}

trait NSStringExt {
    unsafe fn to_str(&self) -> &str;
}

impl NSStringExt for id {
    unsafe fn to_str(&self) -> &str {
        unsafe {
            let cstr = self.UTF8String();
            if cstr.is_null() {
                ""
            } else {
                CStr::from_ptr(cstr as *mut c_char).to_str().unwrap()
            }
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct NSRange {
    pub location: NSUInteger,
    pub length: NSUInteger,
}

impl NSRange {
    fn invalid() -> Self {
        Self {
            location: NSNotFound as NSUInteger,
            length: 0,
        }
    }

    fn is_valid(&self) -> bool {
        self.location != NSNotFound as NSUInteger
    }

    fn to_range(self) -> Option<Range<usize>> {
        if self.is_valid() {
            let start = self.location as usize;
            let end = start + self.length as usize;
            Some(start..end)
        } else {
            None
        }
    }
}

impl From<Range<usize>> for NSRange {
    fn from(range: Range<usize>) -> Self {
        NSRange {
            location: range.start as NSUInteger,
            length: range.len() as NSUInteger,
        }
    }
}

unsafe impl objc::Encode for NSRange {
    fn encode() -> objc::Encoding {
        let encoding = format!(
            "{{NSRange={}{}}}",
            NSUInteger::encode().as_str(),
            NSUInteger::encode().as_str()
        );
        unsafe { objc::Encoding::from_str(&encoding) }
    }
}

/// Allow NSString::alloc use here because it sets autorelease
#[allow(clippy::disallowed_methods)]
unsafe fn ns_string(string: &str) -> id {
    unsafe { NSString::alloc(nil).init_str(string).autorelease() }
}
