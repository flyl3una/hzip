//! 对话框（About，T-MENU1）：**版本 + 开源许可**（用户口径 2026-10-09：只留
//! 这两项，不再列「支持的格式」清单，也不列仓库地址）。
//!
//! 复用 `window.open_dialog`（Root 对话框层；0.7.1 起层由 `component` 的
//! `WindowState` 插件在 `gpui_base::Root::render` 内自动挂载，内容视图不再手工
//! 挂层）——与请求-应答弹窗（prompt.rs）同机制，但纯展示（唯一动作 = 关闭：
//! ✕ / 遮罩 / Esc）。
//!
//! 关闭：kit `DialogProps` 默认 `close_button = true` + `overlay_closable = true`。

use crate::ui::constant::DIALOG_W;
use crate::ui::spacing::sp;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, SharedString, Window, div};

// 许可身份信息在**编译期**从仓库根 `LICENSE` 派生（`build.rs::derive_license` →
// `OUT_DIR/license.rs`），杜绝「清单改了、界面还显示旧许可」的静默漂移：
// `SPDX`/`NAME`/`VERSION`/`DATED`/`DISPLAY` 均由正文与 `CARGO_PKG_LICENSE`
// 交叉校验后生成。
include!(concat!(env!("OUT_DIR"), "/license.rs"));

/// 关于对话框（Modal；唯一动作 = 关闭）。
pub fn open_about_dialog(window: &mut Window, cx: &mut App) {
    let version = env!("CARGO_PKG_VERSION").to_string();
    let muted = cx.theme().muted_foreground;

    window.open_dialog(cx, move |dialog, _window, _cx| {
        let body = v_flex()
            .debug_selector(|| "about-body".into())
            .gap(sp(2.))
            .child(div().text_sm().child(SharedString::from(format!(
                "{} {version}",
                crate::tr("about.version")
            ))))
            .child(
                div()
                    .debug_selector(|| "about-license".into())
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(crate::tr_with(
                        "about.license",
                        // 许可**身份**（spdx + 显示名）来自编译期派生常量；
                        // 框架文案仍走 i18n 模板。
                        &[("spdx", SPDX), ("name", DISPLAY)],
                    ))),
            );

        dialog
            .title(crate::tr("about.title"))
            .width(DIALOG_W)
            .child(body)
    });
}

#[cfg(test)]
#[path = "tests/about.rs"]
mod tests;
