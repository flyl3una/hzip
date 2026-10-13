//! 页签行布局（T-G50 §3.3 自 app.rs 迁入；T-G56 点击接线落地）。
//!
//! # 2026-09-23 三次修订：改用 `gpui-base` 的无样式页签原语（用户口径）
//!
//! 用户三条口径：①「文件浏览 / 任务中心 / 设置」这条栏要 **Underline Tabs**（聚焦项
//! 用主题色下划线提示当前页）；② 鼠标悬停要**跟工具栏按钮一样有浅色背景**；③ 这条栏
//! **不要自己单独的边框**，行高与左侧工具按钮**一致**（否则两边高低不齐）。
//!
//! `gpui-component` 的 `TabBar` + `TabVariant` 五个变体都做不到三条同时成立：
//!
//! | 变体 | 行高（Medium） | 这条栏自身的边框 | 悬停底色 | 聚焦提示 |
//! |---|---|---|---|---|
//! | `Tab` | 32（= 按钮） | **全宽 1px `theme().border` 轨道线** | 无 | 灰底 + 左右灰边 |
//! | `Outline` | 32 | 每项 1px 全周描边（未选中 = `theme().border`） | `tokens.secondary_hover` | 1px 全周主题色描边 |
//! | `Pill` | 32 | 无 | `tokens.secondary` | 主题色**填充** |
//! | `Segmented` | 32 | 无 | 无 | 滑动指示块 |
//! | `Underline` | **36**（Small 30） | **全宽 1px 轨道线** | 无 | 底部 2px 主题色下划线 |
//!
//! 只有 `Underline` 给「主题色下划线」，但它行高 36 ≠ 按钮 32，而且
//! `tab_bar.rs` 对 `Underline`/`Tab` **无条件**塞一个 `#border-b`（`absolute`
//! `bottom_0` `size_full` `border_b_1`），外部无法关掉——于是「自己有边框 + 两边不
//! 等高」两件事都踩中；反过来 32 行高的变体又都不是下划线样式。
//!
//! 所以本模块改用 **`gpui-base` 的无样式原语** `gpui_kit::base::{Tabs, Tab}`
//! （`gpui-component` 自己内部就是用它实现的）：`Tabs` 只挂 `Role::TabList`，
//! `Tab` 只挂 `Role::Tab` + 指针激活 + `aria-selected` / `aria-position-in-set`，
//! **样式全归调用方**。于是三条口径同时成立：
//!
//! - **两边同高**：行高/左右内边距用与工具按钮**同一套映射**（`button.rs` 的普通
//!   按钮分支：XSmall `h_5/px_1`、Small `h_6/px_2`、Medium `h_8/px_2p5`、
//!   Large `h_8/px_3`）+ 同一个 `component_size()` ⇒ 页签与按钮逐像素等高；
//!   字号同 `button_text_size(size)`。
//! - **悬停浅底**：跟 `ButtonVariant::Ghost` 的悬停口径同源——0.7.1 起是主题 token
//!   `tokens.accent`（暗色再压 0.5 透明度，见 [`hover_surface`]），**前景同源取
//!   `accent_foreground`**（Ghost 的悬停前景就是它：kit `button.rs` 的
//!   `Self::Ghost => cx.theme().accent_foreground`）⇒ 与工具按钮的悬停底色+文字
//!   完全同款，不会「按钮一种灰、页签另一种灰」，也不会出现下一条那种事故。
//! - **2026-10-13 修：悬停文字曾经是空操作**。原实现
//!   `.hover(|s| s.bg(hover).text_color(tab_active_foreground))`——而
//!   `tab_foreground` 与 `tab_active_foreground` 在深浅两套主题下**是同一个颜色**
//!   ⇒ 底色变了、文字一字不变；用户把 `[ui].theme_color` 设成饱和色（#16A34A）后
//!   就是「鼠标移上去仍是深字、看不清」（菜单栏/工具栏悬停正常，因为它们是成对 token）。
//!   现取 [`hover_style`] 的（底色, 前景）成对值，并由 `tests/tab_bar.rs` 的
//!   `hover_foreground_is_paired_and_differs_from_idle` 按四种主题组合钉住。
//! - **没有自己的边框**：一个边框都不画，整行唯一那条线是工具栏自己的 `border_b_1`；
//!   聚焦项 = 底部 2px `theme().primary` 下划线，用**绝对定位**画（不占内容盒 ⇒
//!   出现/消失都不会让文字上下跳 1px，也不会撑高这一栏）。
//!
//! 元素 id 由本模块自己发（[`tab_id`] / [`close_id`]）——kit 的 `Tab::ix()` 是
//! `pub(crate)`，若用 `gpui-component` 的 `Tab` 又脱离 `TabBar`，每个 `Tab` 的
//! element id 都会停在 `0usize`（多元素共用一个 id）；`gpui-base` 的
//! `Tab::new(id)` 没有这个问题。
//!
//! 注：kit 的「页签溢出 ▾」下拉（`TabBar::menu`）随之不用了——本栏最多 3 个页签
//! （`MainTab` 只有三个变体），`tabs.len() > 3` 永不成立，那枚箭头本来就出不来。
//!
//! 页签点击 / 关闭经 `on_click(MainTab)` / `on_close(MainTab)` 回调上行
//! （gpui 现场 `&mut App` 由调用方闭包消费，本模块不持 AppShell 引用）。

use gpui_kit::base::{Tab, Tabs};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::badge::Badge;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme, Size, StyleSized as _};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClickEvent, Hsla, IntoElement, Styled, Window, div, px};

use crate::ui::contracts::MainTab;
use crate::ui::widgets::AppIcon;

/// 页签点击回调（kind + gpui App 现场调用权）。
pub type TabClick = std::rc::Rc<dyn Fn(MainTab, &mut App)>;
/// 页签关闭回调（点关闭钮；语义由 shell `close_tab` 承担）。
pub type TabClose = std::rc::Rc<dyn Fn(MainTab, &mut App)>;

/// 页签标题 locale key。
fn tab_label(tab: MainTab) -> &'static str {
    match tab {
        MainTab::Browse => "tab_bar.browse",
        MainTab::Jobs => "tab_bar.jobs",
        MainTab::Settings => "tab_bar.settings",
    }
}

/// 页签本体的 element id（**本模块自己发**：`gpui-base` 的 `Tab::new(id)` 要调用方
/// 给 id，也顺带成了版式测试的调试锚点）。
fn tab_id(tab: MainTab) -> &'static str {
    match tab {
        MainTab::Browse => "main-tab-browse",
        MainTab::Jobs => "main-tab-jobs",
        MainTab::Settings => "main-tab-settings",
    }
}

/// 关闭钮 id（每页签一个，避免 keyed state 撞键）。
fn close_id(tab: MainTab) -> &'static str {
    match tab {
        MainTab::Browse => "tab-close-browse",
        MainTab::Jobs => "tab-close-jobs",
        MainTab::Settings => "tab-close-settings",
    }
}

/// 页签几何 = **工具按钮几何**（用户口径：「和左侧工具栏的按钮…不然两边高度不一致」）。
///
/// 映射逐条抄 `gpui-component` `button.rs` 里普通按钮（有文字）那一支：
/// XSmall `h_5/px_1` · Small `h_6/px_2` · Medium `h_8/px_2p5` · Large `h_8/px_3`
/// （`Size::Size(_)` 档 `component_size()` 不会返回，落 Medium 口径）。
fn tab_metrics(size: Size, tab: Tab) -> Tab {
    match size {
        Size::XSmall => tab.h_5().px_1(),
        Size::Small => tab.h_6().px_2(),
        Size::Large => tab.h_8().px_3(),
        _ => tab.h_8().px_2p5(),
    }
}

/// 悬停底色（用户口径：「跟其他按钮聚焦一样得有一个浅色背景，跟按钮聚焦类似」）。
///
/// 口径 = `gpui-component` 0.7.1 `ButtonVariant::Ghost` 的悬停底色：直接用主题
/// token `tokens.accent`，**暗色再压 0.5 透明度**
/// （上游证据：`gpui-component-0.7.1/src/button/button.rs:1168-1175`
/// `Self::Ghost => { let accent: Background = cx.theme().tokens.accent.into();
/// if cx.theme().mode.is_dark() { accent.opacity(0.5) } else { accent } }`）。
/// 页签与工具按钮的悬停底色同源，不会出现「按钮一种灰、页签另一种灰」。
///
/// 0.6 的公式是 `secondary` 提亮/压暗 `0.1` 再压 `0.8` 透明度；0.7 起已弃用
/// ——再按旧公式写会让页签与 0.7 的 Ghost 按钮悬停色不同源（「一种灰、另一种灰」
/// 这个原始症状会复发），故此处只跟 `tokens.accent` 走。
fn hover_surface(cx: &App) -> Hsla {
    let accent: Hsla = cx.theme().tokens.accent.into();
    if cx.theme().mode.is_dark() {
        accent.opacity(0.5)
    } else {
        accent
    }
}

/// 页签悬停的「底色 + 前景」**成对**返回（渲染与测试共用同一份取值）。
///
/// 抽出来是为了让测试能按**真实主题**求值并算对比度（`tests/tab_bar.rs` 的
/// `hover_pair_keeps_text_readable`）——若渲染路径自己再拼一次，测试就会变成
/// 「测另一次实现」。
pub(crate) fn hover_style(cx: &App) -> (Hsla, Hsla) {
    (hover_surface(cx), cx.theme().accent_foreground)
}

/// 关闭钮（页签内右侧槽位；点它只关页签，别顺带触发切页签）。
fn close_button(tab: MainTab, on_close: TabClose) -> Button {
    Button::new(close_id(tab))
        .debug_selector({
            let sel = close_id(tab);
            move || sel.to_string()
        })
        .xsmall()
        .ghost()
        // 圆角交给全局主题（`theme.radius = 0` ⇒ 直角），不再组件级硬编码
        //（用户口径 2026-09-23：圆角/直角统一走主题）。
        .icon(AppIcon::X)
        .tooltip(crate::tr("tab_bar.close"))
        .on_click(move |_, _, cx| {
            // 关闭钮在页签内部：吞掉事件，别让点叉顺带触发「切换页签」
            cx.stop_propagation();
            on_close(tab, cx);
        })
}

/// 页签行（`tabs` = 当前打开的页签，有序；`active` = 高亮项）。
///
/// 尺寸随布局密度：紧凑 → `Size::Small`（行高/内边距/字号/间距一起收），
/// 且**与同档的工具按钮完全相同**（见 [`tab_metrics`]）。
pub fn tab_row(
    tabs: &[MainTab],
    active: MainTab,
    jobs_badge: usize,
    on_click: TabClick,
    on_close: TabClose,
    cx: &App,
) -> impl IntoElement {
    let size = crate::ui::spacing::component_size();
    let total = tabs.len();
    let idle_fg = cx.theme().tab_foreground;
    let active_fg = cx.theme().tab_active_foreground;
    let (hover_bg, hover_fg) = hover_style(cx);
    let underline = cx.theme().primary;
    // 圆角走**全局主题**（用户口径 2026-09-23）：`gpui-base` 的 `Tabs`/`Tab` 是
    // 无样式原语，自己不画圆角——悬停底色与聚焦下划线要跟 `theme.radius` 走，必须
    // 在这里显式取用。`radius = 0` 时自然回到直角。
    let radius = cx.theme().radius;
    let radius_full = cx.theme().radius_full();
    // 内容间距同按钮内容行（`button.rs`：XSmall/Small 是 `gap_1`，其余 `gap_2`）。
    let content_gap = |tab: Tab| match size {
        Size::XSmall | Size::Small => tab.gap_1(),
        _ => tab.gap_2(),
    };

    Tabs::new("main-tabs")
        .debug_selector(|| "main-tabs".into())
        .flex_shrink_0()
        .flex()
        .items_center()
        // **页签之间不留间隔**（用户口径 2026-09-23）：紧贴排布，视觉分隔交给
        // 每个页签自己的内边距与聚焦下划线。
        .children(tabs.iter().enumerate().map(move |(ix, &tab)| {
            let on_click = on_click.clone();
            let on_close = on_close.clone();
            let selected = tab == active;
            let label = crate::tr(tab_label(tab));
            let item = Tab::new(tab_id(tab))
                .debug_selector({
                    let sel = tab_id(tab);
                    move || sel.to_string()
                })
                .selected(selected)
                // 无障碍：「第 2 项 / 共 3 项」+ 显式名字（页签里还有徽标与关闭钮）。
                .set_position(ix + 1, total)
                .accessibility_label(label.clone())
                .relative()
                .cursor_pointer()
                .whitespace_nowrap()
                .button_text_size(size)
                .text_color(if selected { active_fg } else { idle_fg })
                // 悬停：浅底（`tokens.accent`，同 Ghost 按钮）+ 文字转**成对**前景
                // `accent_foreground`（同 Ghost 按钮）——两者必须成组换，否则自定义
                // 主题色下会出现「深字 + 饱和底」（见上方 `hover_fg` 注释）。
                // 底色圆角跟主题（工具按钮的悬停底色也是这个弧度——kit `Button`
                // 取 `theme.radius`）。
                .rounded(radius)
                .hover(move |this| this.bg(hover_bg).text_color(hover_fg))
                .on_click(move |_: &ClickEvent, _: &mut Window, cx: &mut App| on_click(tab, cx))
                .child(label);
            let item = content_gap(item);
            let item = if tab == MainTab::Jobs {
                item.child(
                    Badge::new()
                        .count(jobs_badge)
                        .max(99)
                        .color(cx.theme().danger),
                )
            } else {
                item
            };
            let item = item.child(close_button(tab, on_close));
            // 聚焦项 = 底部 2px 主题色下划线；**绝对定位**（不占内容盒、不改版式，
            // 也不给这一栏添任何边框）。线端随主题圆角：`radius_full()`——`radius = 0`
            // 时两端是直角（与整条栏的直角观感一致），有弧度时是圆头。
            let item = if selected {
                item.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(2.))
                        .rounded(radius_full)
                        .bg(underline),
                )
            } else {
                item
            };
            tab_metrics(size, item).into_any_element()
        }))
}

#[cfg(test)]
#[path = "tests/tab_bar.rs"]
mod tests;
