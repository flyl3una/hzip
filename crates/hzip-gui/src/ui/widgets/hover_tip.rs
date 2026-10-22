//! 自绘悬浮提示（行/单元格名称的完整文本）。
//!
//! # 为什么自绘
//!
//! 组件库没有可用的行悬浮 API：`gpui_kit::div()` 是 `gpui-base` 的 `Div`，
//! 它只通过 **`InteractiveElement`**（`on_click`/`on_mouse_down`/`on_mouse_move`）
//! 和 **`StatefulInteractiveElement`**（`on_hover`/`tooltip`，需先 `.id(...)`）
//! 暴露交互——`table`/`list`/`tree` 三个组件本身没有任何 tooltip 钩子，所以这里
//! 自己画。
//!
//! # 形态
//!
//! 名称元素 `.on_hover` 设置文本、`.on_mouse_move` 更新鼠标位置；`ShellState`
//! 持有当前提示，shell 根 render 末尾把浮层画在**窗口坐标**上（绝对定位、树序
//! 最后）——因此不会被列表/树的 `overflow_hidden` 裁掉。

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::*;
use gpui_kit::{App, SharedString, div, px};

/// 当前悬浮提示（坐标 = 鼠标位置，窗口坐标系）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverTip {
    /// 显示文本（完整路径/名称）
    pub text: SharedString,
    /// 鼠标 x（窗口坐标）
    pub x: i32,
    /// 鼠标 y（窗口坐标）
    pub y: i32,
}

/// 浮层渲染（`None` = 不渲染）。shell 根 render 调用一次。
///
/// 位置 = 鼠标右下 14/18px（不压住指针本身）；超长文本自动换行，最大宽度 640px。
pub fn layer(tip: Option<&HoverTip>, cx: &App) -> Option<gpui_kit::Div> {
    let tip = tip?;
    let theme = cx.theme();
    Some(
        div()
            .absolute()
            .left(px(tip.x as f32 + 14.0))
            .top(px(tip.y as f32 + 18.0))
            .max_w(px(640.0))
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .text_xs()
            .child(tip.text.clone()),
    )
}
