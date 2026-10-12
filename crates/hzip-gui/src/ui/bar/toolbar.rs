//! 工具栏布局（T-G50 §3.3 自 app.rs 迁入；T-BAR 改版：七钮 + 三下拉）。
//!
//! 动作派发闭包由调用方注入（调用方 `cx.listener` 构造 `emit`）——本模块
//! 不持有 AppShell 引用，保持布局件无状态。
//!
//! # 布局（用户口径 2026-09-29）
//!
//! ```text
//! [打开 ▾][解压 ▾][添加][删除][预览][查看 ▾]        [页签区]
//! ```
//!
//! - **打开在最左**；「解压全部」按钮**已移除**（语义由菜单与快捷键承担——
//!   `ToolbarAction::ExtractAll` 变体保留，`ctrl-alt-e` 仍派发它）；
//! - ▾ = 独立按钮（`tb-open-menu`/`tb-extract-menu`/`tb-columns` 本身就是下拉
//!   触发钮）：打开▾ = 最近打开（与菜单「文件 ▸ 最近的文件」同一份 `mru_items`）；
//!   解压▾ = 五种解压方式（`ExtractMode::ALL`）；查看▾ = 列显隐（勾选态 = 当前
//!   `ColumnsConfig`）。
//! - **可用性唯一事实源 = [`crate::ui::contracts::ToolbarEnable`]**（快照派生，
//!   与右键菜单 `browse::item_disabled` 同口径；对照表测试锁死两处不漂移）。
//!   本模块只做「`enable` → 灰态」的纯映射（[`enabled_for`]）。
//!
//! # 紧排口径（用户要求 2026-09-23，T-BAR 沿用）
//!
//! 工具栏整条**只有按钮自身的内部内边距**贡献视觉间隔：
//!
//! | 项 | 改前 | 改后 | 为什么 |
//! |---|---|---|---|
//! | 整条上下内边距 | `.py(sp(1.))` | **无** | 整条高度 = 最高的那个孩子（无页签时 = 按钮固有高度） |
//! | 按钮之间 | `h_flex().gap(sp(1.))` | **无** | 按钮不带左右外边距，相邻按钮左右缘重合 |
//! | 按钮描边 | `ButtonVariant::Default`（1px `theme.input` 边框 + 灰底） | `.ghost()`（无边框、无底色） | 用户口径「按钮不需要边框」；ghost 在**禁用态**也不补描边（Default 的 disabled 语义样式会补一条灰边，见 gpui-component `button.rs`） |
//! | 按钮圆角 | `ButtonRounded::Medium` | **不设**（= `theme().radius`） | 圆角统一归全局主题（`ui/theme.rs` 设 `radius = 0` ⇒ 直角）；组件级硬编码会让改主题失效 |
//!
//! 整条与页签组之间仍留一个 `sp(1.)`：那是**两区之间**的间隔，不是按钮外边距。
//! ▾ 与主按钮之间同理——两钮各自 `px_2p5` 内部内边距即视觉间隔，不额外加 gap。

use crate::ui::constant::{HAIRLINE, SPLIT_DIVIDER_H};
use crate::ui::spacing::{component_size, sp};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, IntoElement, Styled, div};

use crate::ui::bar::menu_bar::{MenuAction, MenuEntry, mru_items};
use crate::ui::contracts::{
    BrowseColumnId, ColumnsConfig, ExtractMode, ToolbarAction, ToolbarEmit, ToolbarEnable,
};
use crate::ui::widgets::AppIcon;

/// 工具栏渲染输入（快照派生的**只读**投影；构造见 `AppStateUiSnapshot::toolbar_enable`）。
pub struct ToolbarProps<'a> {
    /// 可用性（唯一事实源）
    pub enable: ToolbarEnable,
    /// 最近打开（LRU 序；「打开▾」内容源，与菜单同一份）
    pub mru: &'a [String],
    /// 浏览列配置（「查看▾」勾选态）
    pub columns: &'a ColumnsConfig,
}

/// 「本动作此刻可点吗」——纯映射（可用性只由 [`ToolbarEnable`] 决定，
/// 本函数是它与动作的**唯一**连接点；新增动作必须在此给出灰态口径）。
pub(crate) fn enabled_for(enable: &ToolbarEnable, action: &ToolbarAction) -> bool {
    if enable.busy {
        return false; // 忙碌一律置灰（全局语义，叠加在各项之上）
    }
    match action {
        // 打开类：无档案也可用（选择器 / 最近记录）
        ToolbarAction::Open | ToolbarAction::OpenRecent(_) | ToolbarAction::ClearMru => true,
        ToolbarAction::Extract | ToolbarAction::ExtractAll | ToolbarAction::ExtractMode(_) => {
            enable.can_extract
        }
        ToolbarAction::AddFiles => enable.can_add,
        // 「无聚焦文件时置灰」（用户口径）
        ToolbarAction::Delete => enable.can_delete,
        ToolbarAction::Preview => enable.can_preview,
        ToolbarAction::ToggleColumn(_) => enable.can_columns,
        // 无工具栏按钮的菜单类动作
        ToolbarAction::Refresh | ToolbarAction::TestIntegrity | ToolbarAction::Settings => true,
    }
}

/// 「查看▾」菜单项纯数据：`(label, key, checked)`，顺序 = [`BrowseColumnId::ALL`]。
///
/// 抽成纯函数是为了**可测**：菜单由 kit 的 `PopupMenu` 在渲染期构建，测试拿不到
/// 项集；勾选态与列集是本功能唯一的逻辑，单独锁死。
pub(crate) fn column_menu_entries(columns: &ColumnsConfig) -> Vec<(String, &'static str, bool)> {
    BrowseColumnId::ALL
        .iter()
        .map(|c| {
            (
                c.label(),
                c.key(),
                !columns.hidden.contains(c), // 勾选 = 当前显示（Name 恒显示）
            )
        })
        .collect()
}

/// 工具栏（七钮 + 三下拉；`style` 三态：IconText / IconOnly / Hidden）。
pub fn toolbar(
    props: &ToolbarProps<'_>,
    style: crate::ui::bar::menu_bar::ToolbarStyle,
    emit: &ToolbarEmit,
    tabs: Option<gpui_kit::AnyElement>,
    cx: &App,
) -> impl IntoElement {
    use crate::ui::bar::menu_bar::ToolbarStyle;
    let enable = props.enable;
    if style == ToolbarStyle::Hidden {
        // 工具栏收起时**页签仍要在**（右侧页签是切页唯一入口）——退化为只剩
        // 页签的窄条；底色/下边框与工具栏行同源，保证页签栏外观一致。
        return h_flex()
            .id("toolbar")
            .debug_selector(|| "toolbar".into())
            .flex_shrink_0()
            .px(sp(2.))
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .children(tabs)
            .into_any_element();
    }
    let btn = |id: &'static str,
               icon: AppIcon,
               label: String,
               action: ToolbarAction|
     -> gpui_kit::AnyElement {
        let emit = emit.clone();
        let enabled = enabled_for(&enable, &action);
        // 仅图标模式不带 label（Button.label 是 Option，原生支持）；
        // tooltip 保留——悬停仍能看到功能名
        let mut button = Button::new(id)
            // 稳定调试锚点（版式/存在性测试用：页签与按钮必须同行同侧）
            .debug_selector(move || id.to_string())
            // 无边框按钮（用户口径）：ghost 在常态/悬停/选中/禁用四态都不画描边，
            // 常态底色也是透明——按钮之间的视觉间隔只来自各自的 `px_2p5`。
            .ghost()
            // 圆角**交给全局主题**（`ui/theme.rs::apply_theme` 把 `theme.radius`
            // 设为 0 ⇒ kit 默认的 `ButtonRounded::Medium` 就是直角）。此处不再
            // 单独写 `ButtonRounded::None`：组件级硬编码会让「改主题圆角」失效
            //（用户口径 2026-09-23）。
            .with_size(component_size())
            .icon(icon)
            .tooltip(label.clone())
            .disabled(!enabled)
            .on_click(move |_, window, cx| emit(&action, window, cx));
        if style == ToolbarStyle::IconText {
            button = button.label(label.clone());
        }
        button.into_any_element()
    };
    // —— 打开▾：最近打开（与菜单「文件 ▸ 最近的文件」同一份 `mru_items`）——
    let mru: Vec<String> = props.mru.to_vec();
    let open_menu = {
        let emit = emit.clone();
        move |mut menu: PopupMenu,
              _window: &mut gpui_kit::Window,
              _cx: &mut gpui_kit::Context<PopupMenu>| {
            let items = mru_items(&mru);
            for entry in items {
                match entry {
                    MenuEntry::Separator => menu = menu.item(PopupMenuItem::separator()),
                    MenuEntry::Submenu { .. } => {} // MRU 无子菜单（占位分支，保持穷尽）
                    MenuEntry::Item {
                        label,
                        icon,
                        action,
                        disabled,
                        checked,
                    } => {
                        // 最近项：索引 → 路径（与菜单同规则；占位项的索引为 usize::MAX）
                        let action = match &action {
                            MenuAction::OpenRecent(ix) if *ix < mru.len() => {
                                ToolbarAction::OpenRecent(mru[*ix].clone())
                            }
                            MenuAction::ClearMru => ToolbarAction::ClearMru,
                            _ => ToolbarAction::Open, // 禁用占位项：动作不可达
                        };
                        let mut item = PopupMenuItem::new(label)
                            .disabled(disabled)
                            .checked(checked);
                        if let Some(ic) = icon {
                            item = item.icon(ic);
                        }
                        let emit = emit.clone();
                        menu = menu
                            .item(item.on_click(move |_, window, cx| emit(&action, window, cx)));
                    }
                }
            }
            menu
        }
    };

    // —— 解压▾：五种方式（桌面不可用 → 桌面两项置灰）——
    let extract_menu = {
        let emit = emit.clone();
        move |mut menu: PopupMenu,
              _window: &mut gpui_kit::Window,
              _cx: &mut gpui_kit::Context<PopupMenu>| {
            for mode in ExtractMode::ALL {
                let disabled = mode.needs_desktop() && !enable.desktop_available;
                let action = ToolbarAction::ExtractMode(mode);
                let emit = emit.clone();
                menu = menu.item(
                    PopupMenuItem::new(crate::tr(mode.key()))
                        .disabled(disabled)
                        .on_click(move |_, window, cx| emit(&action, window, cx)),
                );
            }
            menu
        }
    };

    // —— 查看▾：列显隐（勾选态 = 当前配置；点击 = 切换该列）——
    let columns_menu = {
        let entries = column_menu_entries(props.columns);
        let emit = emit.clone();
        move |mut menu: PopupMenu,
              _window: &mut gpui_kit::Window,
              _cx: &mut gpui_kit::Context<PopupMenu>| {
            for (label, key, checked) in &entries {
                let action = ToolbarAction::ToggleColumn((*key).to_string());
                let emit = emit.clone();
                menu = menu.item(
                    PopupMenuItem::new(label.clone())
                        .checked(*checked)
                        .on_click(move |_, window, cx| emit(&action, window, cx)),
                );
            }
            menu
        }
    };

    // ▾ 触发钮灰态：跟随其主动作/内容可用性（打开▾ 恒可用；解压▾ 随档案；
    // 查看▾ 随浏览页）；忙碌时一律置灰（`enabled_for` 内已含）。
    let open_menu_enabled = enabled_for(&enable, &ToolbarAction::Open);
    // 解压▾：随档案在场（菜单内桌面两项按 `desktop_available` 单独置灰——
    // 菜单本身是「方式选择器」，只要档案在就该能打开它看选项）。
    let extract_menu_enabled = enabled_for(&enable, &ToolbarAction::ExtractMode(ExtractMode::Here));
    let columns_menu_enabled = enabled_for(&enable, &ToolbarAction::ToggleColumn(String::new()));

    h_flex()
        .id("toolbar")
        // 版式测试锚点：整条高度必须等于按钮高度（上下内边距已移除）
        .debug_selector(|| "toolbar".into())
        .flex_shrink_0()
        // 两区之间的间隔（工具按钮组 ↔ 页签组）；**不是**按钮外边距——
        // 按钮之间的间隔见下（无 gap）。
        .gap(sp(1.))
        // `px` 保留（整条左右贴边留白）；`py` 移除（用户口径：上下 padding 不要）
        .px(sp(2.))
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().popover)
        // 一行两区（用户要求 2026-09-23）：左 = 工具按钮组，右 = 页面页签组
        //（省掉独立页签行；工具栏收起时页签退化为窄条，见上）。
        .justify_between()
        .child(
            h_flex()
                .flex_shrink_0()
                // 无 gap：按钮不带左右外边距（用户口径），左右缘直接相邻。
                .items_center()
                .children([
                    // 拆分按钮①：打开 + 最近打开▾（用户口径 2026-09-29：箭头部
                    // **只画图标**、与主动作**合成一个控件**）
                    split_pair(
                        btn(
                            "tb-open",
                            AppIcon::Folder,
                            crate::tr("toolbar.open"),
                            ToolbarAction::Open,
                        ),
                        chevron_btn(
                            "tb-open-menu",
                            crate::tr("toolbar.open_recent"),
                            open_menu_enabled,
                            open_menu,
                        ),
                        cx,
                    ),
                    // 拆分按钮②：解压 + 解压方式▾
                    split_pair(
                        btn(
                            "tb-extract",
                            AppIcon::Replace,
                            crate::tr("toolbar.extract"),
                            ToolbarAction::Extract,
                        ),
                        chevron_btn(
                            "tb-extract-menu",
                            crate::tr("toolbar.extract_mode"),
                            extract_menu_enabled,
                            extract_menu,
                        ),
                        cx,
                    ),
                    btn(
                        "tb-add",
                        AppIcon::Plus,
                        crate::tr("toolbar.add"),
                        ToolbarAction::AddFiles,
                    ),
                    btn(
                        "tb-delete",
                        AppIcon::Minus,
                        crate::tr("toolbar.delete"),
                        ToolbarAction::Delete,
                    ),
                    btn(
                        "tb-preview",
                        AppIcon::Eye,
                        crate::tr("toolbar.preview"),
                        ToolbarAction::Preview,
                    ),
                    menu_btn(
                        "tb-columns",
                        // 用户口径 2026-09-29：查看按钮换成 lucide 的
                        // `layout-list`（左侧两块 + 右侧列表行），比原「columns」
                        // 更贴「查看（看到什么/怎么排）」这个下拉的语义
                        AppIcon::LayoutList,
                        crate::tr("toolbar.columns"),
                        columns_menu_enabled,
                        style,
                        columns_menu,
                    ),
                ]),
        )
        .children(tabs)
        .into_any_element()
}

/// 拆分按钮（split button）：主动作半 + 下拉箭头半**视觉一体**（用户口径
/// 2026-09-29「合到一起，不要看起来像两个按钮」）。
///
/// 构成 = 主半 ·[`HAIRLINE`] 竖分隔· 箭头半，三者同属一个 `h_flex`、**无 gap**，
/// 相邻缘相贴（沿用工具栏紧排口径：按钮不带外边距、组内无 gap）。
fn split_pair(
    main: gpui_kit::AnyElement,
    arrow: gpui_kit::AnyElement,
    cx: &App,
) -> gpui_kit::AnyElement {
    h_flex()
        .flex_shrink_0()
        .items_center()
        .child(main)
        .child(div().w(HAIRLINE).h(SPLIT_DIVIDER_H).bg(cx.theme().border))
        .child(arrow)
        .into_any_element()
}

/// 拆分按钮的**箭头半**：只画下拉箭头，**永不带文字**（用户口径 2026-09-29：
/// 「最近打开 / 解压方式 这两个按钮不需要文字，只需要图标」——它只是主动作的
/// 下拉附录，不是第二个功能按钮）。tooltip 保留功能名。
fn chevron_btn<F>(id: &'static str, label: String, enabled: bool, build: F) -> gpui_kit::AnyElement
where
    F: Fn(PopupMenu, &mut gpui_kit::Window, &mut gpui_kit::Context<PopupMenu>) -> PopupMenu
        + 'static,
{
    Button::new(id)
        .debug_selector(move || id.to_string())
        .ghost()
        .with_size(component_size())
        .icon(AppIcon::ChevronDown)
        .tooltip(label)
        .disabled(!enabled)
        .dropdown_menu(build)
        .into_any_element()
}

/// ▾ 下拉触发钮（**图标 + 文字**形态；用于「查看」这类本身就是功能入口的按钮）
/// ：独立按钮 + kit 原生 dropdown（`DropdownMenu` trait 已为 `Button` 实现；
/// 锚点用 trait 默认 `Anchor::TopLeft` = 菜单贴按钮左上向下展开）。
fn menu_btn<F>(
    id: &'static str,
    icon: AppIcon,
    label: String,
    enabled: bool,
    style: crate::ui::bar::menu_bar::ToolbarStyle,
    build: F,
) -> gpui_kit::AnyElement
where
    F: Fn(PopupMenu, &mut gpui_kit::Window, &mut gpui_kit::Context<PopupMenu>) -> PopupMenu
        + 'static,
{
    let mut button = Button::new(id)
        .debug_selector(move || id.to_string())
        .ghost()
        .with_size(component_size())
        .icon(icon)
        .tooltip(label.clone())
        .disabled(!enabled);
    if style == crate::ui::bar::menu_bar::ToolbarStyle::IconText {
        button = button.label(label.clone());
    }
    button.dropdown_menu(build).into_any_element()
}

#[cfg(test)]
#[path = "tests/toolbar.rs"]
mod tests;
