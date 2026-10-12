//! 关于对话框测试（用户口径 2026-10-09：只留版本 + 开源许可）。
//!
//! 断言面：对话框**确实画出来**（正文有 `about-body`、许可行有 `about-license`）
//! 且可关闭；另钉住许可派生常量（`SPDX`/`NAME`/`VERSION`/`DATED`/`DISPLAY`，由
//! `build.rs` 从仓库根 `LICENSE` 生成）。版本文案来自 `env!("CARGO_PKG_VERSION")`，
//! 许可文案来自 `about.license` 词条——由 i18n 一致性用例守着，不在本文件重复断言。

use gpui_kit::component::WindowExt as _;
use gpui_kit::{AppContext as _, IntoElement, Styled as _, Window};

struct Host;

impl gpui_kit::Render for Host {
    fn render(
        &mut self,
        _window: &mut Window,
        _cx: &mut gpui_kit::Context<Self>,
    ) -> impl IntoElement {
        // 0.7.1：dialog 层由 `WindowState` 插件在 `gpui_base::Root::render` 内自动
        // 挂载（`Root` 的手工挂层函数整族已从 kit 删除），内容视图只交容器即可。
        gpui_kit::div().size_full()
    }
}

/// 用户实测口径 2026-10-12：**点 ✕ 必须关掉「关于」**。
///
/// 走**真实鼠标路径**（不是像上一个用例那样 `close_dialog` API 直调）：kit 的 ✕ 是
/// `gpui_base::DialogClose`，它派发 `Cancel` 动作（`gpui-base-0.7.1/src/dialog.rs:336-380`
/// 「Wrapper that dispatches the dialog cancel action when activated」），由 dialog
/// 宿主的 `on_cancel` 接住（默认实现即「关」）。0.7.1 迁移把弹层从「内容视图手工
/// 挂载」改成「`WindowState` 插件自动挂载」，动作路由（焦点路径）随之改变——本用例
/// 钉的就是这条路径，扫掠失败即断言失败，不假绿。
#[gpui_kit::test]
fn about_dialog_closes_when_clicking_the_x(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;

    let (_host, vtcx) = cx.add_window_view(|window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let host = cx.new(|_| Host);
        let view: gpui_kit::AnyView = host.into();
        gpui_kit::component::Root::new(view, window, cx)
    });

    vtcx.update(|window, cx| {
        crate::ui::dialogs::open_about_dialog(window, cx);
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.update(|window, cx| window.has_active_dialog(cx)),
        "前置：关于对话框应已打开"
    );

    let surface = vtcx.debug_bounds("dialog-0").expect("弹层表面应已渲染");

    // 对照：先在**正文区域**点一下（弹层内侧、远离右上角 ✕）——必须仍然打开着。
    // 没有这条，"关掉"就无法排除「弹层任意位置点击都关」这类假绿。
    vtcx.simulate_click(
        gpui_kit::Point {
            x: gpui_kit::px(f32::from(surface.origin.x) + f32::from(surface.size.width) / 2.),
            y: gpui_kit::px(f32::from(surface.origin.y) + f32::from(surface.size.height) - 12.),
        },
        gpui_kit::Modifiers::default(),
    );
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.update(|window, cx| window.has_active_dialog(cx)),
        "点弹层正文不该关闭对话框（对照断言）"
    );

    // ✕ 位于弹层表面右上角（`gpui-component-0.7.1/src/dialog/dialog.rs:736-751`：
    // `top/right = clamp(padding - 10, ≥ 8)`，按钮为 `small`）。在右上角内侧
    // 12~32px 的小范围扫掠——**全部落在外表面之内**，避开背板以免「点背板也关」的
    // 假绿。
    let right = f32::from(surface.origin.x) + f32::from(surface.size.width);
    let top = f32::from(surface.origin.y);
    let mut closed = false;
    let mut tried = 0;
    'sweep: for dy in [12., 16., 20., 24., 28., 32.] {
        for dx in [12., 16., 20., 24., 28., 32.] {
            tried += 1;
            vtcx.simulate_click(
                gpui_kit::Point {
                    x: gpui_kit::px(right - dx),
                    y: gpui_kit::px(top + dy),
                },
                gpui_kit::Modifiers::default(),
            );
            vtcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            if !vtcx.update(|window, cx| window.has_active_dialog(cx)) {
                closed = true;
                break 'sweep;
            }
        }
    }
    assert!(
        closed,
        "点 ✕ 应关闭「关于」对话框（右上角内侧扫过 {tried} 个位置都没关）"
    );
}

/// 打开 → 画出来 → 关闭（`close_dialog` = ✕/Esc/遮罩走的同一条关闭路径）。
#[gpui_kit::test]
fn about_dialog_opens_and_closes(cx: &mut gpui_kit::TestAppContext) {
    let (_host, vtcx) = cx.add_window_view(|window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let host = cx.new(|_| Host);
        let view: gpui_kit::AnyView = host.into();
        gpui_kit::component::Root::new(view, window, cx)
    });

    vtcx.update(|window, cx| {
        crate::ui::dialogs::open_about_dialog(window, cx);
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.update(|window, cx| window.has_active_dialog(cx)),
        "「关于」应打开对话框"
    );
    assert!(
        vtcx.debug_bounds("about-body").is_some(),
        "关于对话框正文应画出来（版本 + 开源许可）"
    );
    assert!(
        vtcx.debug_bounds("about-license").is_some(),
        "关于对话框的许可行应画出来（锚点 about-license）"
    );

    vtcx.update(|window, cx| {
        window.close_dialog(cx);
        let _ = window.draw(cx);
    });
    assert!(
        !vtcx.update(|window, cx| window.has_active_dialog(cx)),
        "关于对话框应可关闭"
    );
}

/// 派生常量自身：`SPDX` 含 `GPL-3.0`，名称/版本/日期与 GPL-3.0 正文相符。
///
/// 常量由 `build.rs::derive_license` 从仓库根 `LICENSE` 生成（见 about.rs 的
/// `include!`）；这里钉住「正文解析成了什么」，防 build.rs 解析回归。
#[test]
fn derived_license_constants_match_gpl3() {
    use super::{DATED, DISPLAY, NAME, SPDX, VERSION};

    assert!(SPDX.contains("GPL-3.0"), "SPDX 应含 `GPL-3.0`：`{SPDX}`");
    assert_eq!(NAME, "GNU GENERAL PUBLIC LICENSE");
    assert_eq!(VERSION, "3");
    assert_eq!(DATED, "29 June 2007");
    assert_eq!(DISPLAY, "GNU General Public License v3");
}
