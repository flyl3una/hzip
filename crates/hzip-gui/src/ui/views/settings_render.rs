//! 设置面板注册表驱动渲染器（T-SET3）：通用页装配 + 叶子控件。
//!
//! # 职责
//!
//! - **页装配**：按分类页过滤 [`REGISTRY`]，逐项依 [`WidgetKind`] 分派渲染；
//!   [`ACTIONS`] 产出页级按钮（`Emit`）与行内按钮（`Clear`/`FilePicker`）。
//!   页内容不再手写——新增设置项只改注册表。
//! - **叶子控件**：Switch / Select / 勾选组 / 文本行 / 列块（[`columns_section`]）。
//! - **事件**：值控件一律上行 `DialogEvent::SettingChanged { id, value }` →
//!   `AppData::apply_setting` 五步流程；归一化在注册表 `store` 完成，本层
//!   **不校验/不写配置/不持全局态**。
//! - **刷新**：[`PageCtx::refresh`] 由面板注入（事件后重读活读视图 + 重渲）。
//!   Select 的确认事件在面板 Entity 装配期订阅（`SelectState` 窗口亲和需
//!   `&mut Window`），本模块只消费实体句柄。

use crate::ui::constant::LABEL_COL_W_BASE;
use crate::ui::spacing::{component_size, sp};
use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::{Field, Form, field as form_field};
use gpui_kit::component::group_box::{GroupBox, GroupBoxVariants as _};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, IntoElement, SharedString, StyleRefinement, Styled, div};

use crate::app::settings_registry::{
    self, ACTIONS, ActionKind, OpenTarget, SettingDef, SettingValue, SettingsTab, SettingsView,
    WidgetKind,
};
use crate::ui::contracts::{BrowseColumnId, ColumnsConfig, DialogEvent};
use crate::ui::widgets::AppIcon;
use crate::viewmodel::SettingId;

/// Select 实体句柄简写（`SearchableVec<String>` delegate）。
pub type SelectEntity = Entity<SelectState<SearchableVec<String>>>;

/// ColorPicker 实体类型别名（T-SEL 走查 #3——面板装配期预建、行渲染取用）。
pub type PickerEntity = Entity<gpui_kit::component::color_picker::ColorPickerState>;

/// 页渲染输入束（事件上行 + 视图刷新）。
pub struct PageCtx {
    /// 事件上行（UI → AppData 五步流程）
    pub on: Rc<dyn Fn(DialogEvent)>,
    /// 事件后刷新面板（重读活读视图 + 重渲）
    pub refresh: Rc<dyn Fn(&mut App)>,
}

/// 页分组框（无标题、**无边框**——页签已含名称，走查 #8：内容不再重复标签；
/// 2026-09-21 用户二轮：「右侧不需要再用一个边框把内容包住」）。
fn group(body: AnyElement) -> AnyElement {
    // 分组框的间距随布局密度：标准档 = 组件默认（内容区 p_4/gap_4 = 16px），
    // 紧凑 0.75×、宽松 1.4×。
    //
    // `content_style` 是唯一能改**内容区**内边距的口子：`GroupBox::render` 先铺
    // 内建 `p_4()/gap_4()`，再 `refine_style(&self.content_style)`
    // （group_box.rs:158-165）⇒ 我们的值后写胜出。外层 `.gap(...)` 同理。
    //
    // 变体取 `Normal`（既无背景也无边框；`Outline` 才画那圈边框）——
    // 只保留它的内边距/间距语义。组与组之间的间距由外层 `Form` 的行距承担。
    let m = crate::ui::spacing::metrics();
    GroupBox::new()
        .normal()
        // `min_w_0`：内容区要能缩到**可用宽度**（默认 `min-width:auto` 会以内容的
        // min-content 为下限 ⇒ 行内按钮排不下时既换不了行、又把列撑爆）
        .content_style(
            StyleRefinement::default()
                .p(m.pad_group())
                .gap(m.gap_lg())
                .min_w_0(),
        )
        .child(body)
        .into_any_element()
}

/// 设置页左侧名称列宽（标准档 140px；走查 #11：110 → 140，约多放两个汉字）。
///
/// **随密度缩放**（2026-09-21 用户六轮口径：label 与右侧设置项区域的间距也要跟
/// 紧凑/标准/宽松走）：列宽 = 140 × 密度系数（紧凑 105 / 标准 140 / 宽松 196）。
/// 标签文本左对齐、字号不随密度变 ⇒ 列宽一缩，标签文字到控件之间的**可见间距**
/// 同步变窄（kit 的 `FormField` 内建 label↔content gap 固定 4px，是这一层唯一
/// 的口子）。超宽标签仍在列内换行而非被裁剪（[`label_cell`] 的 `max_w` 封顶）。
fn label_col_w() -> gpui_kit::Pixels {
    LABEL_COL_W_BASE * crate::ui::spacing::density_scale()
}

/// **行带**：行内「首行」的统一垂直基准（高 = 最高单行控件的固有高度）。
///
/// # 为什么需要它（用户实测：设置页文字「一会儿高一会儿低」）
///
/// 行内各控件固有高度并不相同（gpui-component 0.6）：
///
/// | 控件 | 高 |
/// |---|---|
/// | `Switch`（默认 Medium） | 20 |
/// | 复选框（`widgets::checkbox`，标签行高 1.5） | 21 |
/// | 文本行（`text_sm` 自然行框） | 22.5 |
/// | `Select` / `ColorPicker`（默认档） | 32 |
///
/// 旧实现每行都 `.items_start()`（**顶边**对齐）⇒ 标签文字与控件的相对位置
/// 随控件高度漂移：`Select` 行标签中心比控件中心高 4.75px、`Switch` 行低
/// 1.25px——**跨行相差 6px**，正是「文字一会儿高一会儿低」的来源。
///
/// # 契约
///
/// 行内**首行**的一切（标签文本、标签行尾 ？ 图标、控件）都在本高度的带内
/// **垂直居中**（[`band`]）⇒ 无论控件固有高度多少，中心线恒定。
/// 高于本带的块（勾选组、列块）由调用方保证「首行」进带。
///
/// 回归锚点：`settings_layout::tests::row_first_line_alignment_is_uniform`
/// （对注册表每一行断言「控件首行中心 == 标签中心 == ？图标中心」）。
/// 行带基准高——**取值口径在布局度量提供器**（`ui::spacing::LayoutMetrics::row_band`：
/// 标准 32px × 密度系数）。此处只保留同义名，供设置页渲染与几何回归用例共用。
///
/// 行带定的是行内「首行」的垂直基准：密度只缩间距时若行带仍固定 32px，
/// 设置页的行高不会变（`min_h` 一票决定）⇒ 行带必须随密度一起缩。
/// 字号不经此处（字号由窗口 rem 基准控制，两者独立）。
pub(crate) fn row_band() -> gpui_kit::Pixels {
    crate::ui::spacing::row_band()
}

/// 行带容器：把「行内首行」的内容放进 [`ROW_BAND`] 高、垂直居中的槽。
///
/// 高于带的内容（如 7 项勾选组）不受影响——`min_h` 只是下限，容器高度随内容
/// 增长，`items_center` 于是无位移（内容顶边 = 行顶边，首行即带顶）。
pub(crate) fn band(child: impl IntoElement) -> gpui_kit::Div {
    h_flex()
        .min_h(row_band())
        .items_center()
        .w_full()
        // `min_w_0`：行带要能缩到**可用宽度**。默认 `min-width:auto` 会让长文本
        // 控件（如「日志目录」的路径输入框，min-content = 整条路径宽度）把控件列
        // 顶出内容列——用户六轮实测「紧凑档日志目录一行显示不全」。
        .min_w_0()
        .child(child)
}

/// 「恢复默认」成功提示的去重 id 标记（连点只更新同一条提示，不堆叠）。
struct ResetOkMarker;

/// 页级动作的**成功提示**（i18n key）——目前只有「恢复默认配置」需要。
///
/// 原设置 Dialog 底部按钮会弹这条提示，2026-09-23 设置改页签、弹层 footer 下线后
/// 功能挪到常规页底部，反馈一并搬过来（用户口径「对齐原来 Dialog 上的恢复默认」）。
fn action_toast(event: &DialogEvent) -> Option<&'static str> {
    match event {
        DialogEvent::SettingsReset => Some("setting.reset_ok"),
        _ => None,
    }
}

/// 通用页装配：注册表逐项渲染 + 页级动作按钮。
///
/// `selects` 为面板装配期预建的 Select 实体（窗口亲和），按 [`SettingId`] 取用。
pub fn page(
    tab: SettingsTab,
    view: &SettingsView,
    selects: &[(SettingId, SelectEntity)],
    pickers: &[(SettingId, PickerEntity)],
    ctx: &PageCtx,
    cx: &App,
) -> AnyElement {
    // 文件关联页需候选格式集（无可用档案格式 → 整页降级说明）。
    //
    // 三平台都支持关联（Windows 注册表 / Linux `.desktop`+`mimeapps.list` /
    // macOS LaunchServices），故不再按操作系统降级——只有「本构建没有任何可
    // 打开的档案格式」这一种情况才降级。
    let assoc_ready = !view.assoc_candidates().is_empty();

    let mut form = Form::horizontal().label_width(label_col_w());
    for def in settings_registry::REGISTRY.iter().filter(|d| d.page == tab) {
        if def.id == SettingId::Assoc && !assoc_ready {
            form = form.child(assoc_fallback(def));
            continue;
        }
        form = form.child(row(def, view, selects, pickers, ctx, cx));
    }

    // 上下文菜单页：状态行（系统实况）+ 未应用提示。
    // 状态来自 `SettingsView::ctx_status`（面板每次 reload 在锁外查询一次），
    // 故「点了应用/清除之后」立刻反映真实结果，而不是可能过期的缓存。
    if tab == SettingsTab::ContextMenu {
        form = form.child(ctx_status_row(view, cx));
    }

    // 文件关联页：**双击实况**状态行。
    //
    // 「我们写过注册表」不等于「双击真的归我们」：`FileExts\.<ext>\UserChoice`
    // 是哈希保护的，被别的程序占着的格式，我们怎么点「应用关联」都不生效。
    // 状态行如实区分「已生效 / 待应用 / 被占用」，被占用的给一键「打开方式」
    // 入口（唯一受支持的夺回途径）。
    if tab == SettingsTab::Assoc && assoc_ready {
        form = form.child(assoc_status_row(view, ctx, cx));
    }

    let page_actions: Vec<&'static settings_registry::ActionDef> = ACTIONS
        .iter()
        .filter(|a| a.page == tab && matches!(a.kind, ActionKind::Emit(_)))
        .filter(|_| tab != SettingsTab::Assoc || assoc_ready)
        .collect();
    if !page_actions.is_empty() {
        let mut buttons = h_flex().gap(sp(2.));
        for action in page_actions {
            let ActionKind::Emit(event) = &action.kind else {
                continue;
            };
            let event = event.clone();
            let on = ctx.on.clone();
            let refresh = ctx.refresh.clone();
            let button = Button::new(action.key)
                .with_size(component_size())
                .label(crate::tr(action.label))
                .on_click(move |_, window, cx| {
                    on(event.clone());
                    refresh(cx);
                    // 成功提示（原设置 Dialog 底部「恢复默认」的反馈）：按事件
                    // 类型查表；类型作去重 id，连点只更新同一条提示、不堆叠。
                    if let Some(key) = action_toast(&event) {
                        use gpui_kit::component::WindowExt as _;
                        window.push_notification(
                            gpui_kit::component::notification::Notification::new()
                                .message(crate::tr(key))
                                .id::<ResetOkMarker>(),
                            cx,
                        );
                    }
                });
            // 测试锚点（`cfg!(test)`）：页级动作按钮边界——「按钮点击 → 事件
            // 上行」链路回归用（走查 #13：应用到系统点击后无任何事件痕迹）
            buttons = buttons.child(if cfg!(test) {
                let key = format!("act-{}", action.key);
                div()
                    .debug_selector(move || key.clone())
                    .child(button)
                    .into_any_element()
            } else {
                button.into_any_element()
            });
        }
        // 标签走 `label_cell`（而非 `Field::label`）以便测试锚点覆盖本行
        form = form.child(
            form_field()
                .items_start()
                .label_fn(|_, _| label_cell(crate::tr("setting.actions"), None, "actions"))
                .child(band(buttons)),
        );
    }

    group(form.into_any_element())
}

/// 标签单元：文本 + 可选 ？ 帮助钮（走查 #8：帮助不常驻行内，悬浮查看——
/// 降低设置页信息密度；`help = None` 不显示 ？）。
///
/// 走查 #11：标签根 `max_w(label_col_w())` 封顶 + 文本 `flex_1`（basis 0）——
/// 长标签在列内**换行**（gpui 文本默认 WhiteSpace::Normal），？ 钮占行尾
/// 固定宽；不依赖外层 Field 的 `overflow_x_hidden` 裁剪，英文等语言下
/// 名称完整可见。
///
/// **行带**（[`ROW_BAND`]）：文本块 `min_h(ROW_BAND)` + `justify_center`、
/// ？ 图标钮外层固定 `h(ROW_BAND)` + `items_center` ⇒ 单行标签的文本中心与
/// 图标中心、以及控件中心三者同轴（多行标签文本块超出带高，此时图标停在带
/// 中心 = 控件中心，不再随文本块下移）。
fn label_cell(label: String, help: Option<&'static str>, key: &str) -> impl IntoElement {
    h_flex()
        .gap(sp(1.))
        .items_start()
        .max_w(label_col_w())
        .child(
            v_flex()
                .min_h(row_band())
                .justify_center()
                .flex_1()
                .min_w_0()
                .child(
                    // 测试锚点（`cfg!(test)`）：标签**文本**边界——「列内换行
                    // 不溢出」与「行带同轴」回归用（锚点只包文本，不含图标钮）
                    div()
                        .when(cfg!(test), |t| {
                            let k = format!("lbl-{key}");
                            t.debug_selector(move || k.clone())
                        })
                        .child(label),
                ),
        )
        .when_some(help, |this, help| {
            this.child(
                h_flex().h(row_band()).items_center().child(
                    // 测试锚点（`cfg!(test)`）：？ 图标钮边界——「图标与文字行
                    // 同轴」回归用
                    div()
                        .when(cfg!(test), |d| {
                            let k = format!("help-btn-{key}");
                            d.debug_selector(move || k.clone())
                        })
                        .child(
                            Button::new(format!("help-{key}"))
                                .with_size(component_size())
                                .ghost()
                                .xsmall()
                                .icon(AppIcon::CircleHelp)
                                .tooltip(crate::tr(help)),
                        ),
                ),
            )
        })
}

/// 注册表行的标签（文案 = `def.label`，帮助 = `def.help`）。
fn label_with_help(def: &'static SettingDef) -> impl IntoElement {
    label_cell(crate::tr(def.label), def.help, def.id.key())
}

/// 密集列表行（勾选组 / 列块）的标签：**不做行带**。
///
/// 行带（[`ROW_BAND`]）解决的是「单行控件固有高度 20/21/22.5/32 不一 ⇒ 中心漂移」：
/// 把控件与标签都放进同一高度带内居中，中心自然同轴。而密集列表是**多行块**，
/// 要同轴的是「标签 ↔ 块首行」这一行**文字**本身——此时行带反而有害：标签会被
/// 居中到 32px 带心，而块首行（文字行框 ≈ 24px）的带心比它低
/// `(32 − 22.5)/2 ≈ 4.7px`，于是「标签比首行高一点点」（与旧实现顶边对齐的
/// 6px 漂移同类，只是小一号）。
///
/// 对策：标签与块首行**顶边对齐**（都是文字行框的顶边）⇒ 两者文字中心只差
/// 字号行框之差的一半（1rem 的自然行框 vs 复选行 1.5rem ⇒ 实测 < 1px）。
/// 回归锚点：`settings_layout::tests::row_first_line_alignment_is_uniform` 的
/// 密集行分支（用真实渲染边界断言）。
fn dense_label_with_help(def: &'static SettingDef) -> impl IntoElement {
    dense_label_cell(crate::tr(def.label), def.help, def.id.key())
}

/// [`dense_label_with_help`] 的通用形（行带关闭版 [`label_cell`]）。
fn dense_label_cell(label: String, help: Option<&'static str>, key: &str) -> impl IntoElement {
    h_flex()
        .gap(sp(1.))
        // ？ 与文字行框同轴：无行带（`h(row_band())`）时改由行向居中承担——
        // 内容高 = max(标签行框, ？ 钮) = 标签行框，？ 因而居中于该行框。
        .items_center()
        .max_w(label_col_w())
        // 与块首行同顶：块行自身带 `py(row_gap)`，标签若不加同量的上内边距就会
        // 比首行文字高 `row_gap`（实测标准档 1px / 宽松档 2px）。
        .pt(crate::ui::spacing::row_gap())
        .child(
            // 无 `min_h(row_band())`、无 `justify_center`：标签行框即文字行框，
            // 顶边与块首行顶边对齐（对齐语义见 [`dense_label_with_help`]）。
            v_flex().flex_1().min_w_0().child(
                div()
                    .when(cfg!(test), |t| {
                        let k = format!("lbl-{key}");
                        t.debug_selector(move || k.clone())
                    })
                    .child(label),
            ),
        )
        .when_some(help, |this, help| {
            this.child(
                div()
                    .when(cfg!(test), |d| {
                        let k = format!("help-btn-{key}");
                        d.debug_selector(move || k.clone())
                    })
                    .child(
                        Button::new(format!("help-{key}"))
                            .with_size(component_size())
                            .ghost()
                            .xsmall()
                            .icon(AppIcon::CircleHelp)
                            .tooltip(crate::tr(help)),
                    ),
            )
        })
}

/// 单项行渲染（`WidgetKind` 分派 + 行内动作按钮）。
///
/// 走查 #8：描述不再常驻 `.description()`——帮助经行尾 ？ 悬浮显示
/// （[`label_with_help`]），无帮助的项只有标签一行。
fn row(
    def: &'static SettingDef,
    view: &SettingsView,
    selects: &[(SettingId, SelectEntity)],
    pickers: &[(SettingId, PickerEntity)],
    ctx: &PageCtx,
    cx: &App,
) -> Field {
    let control: AnyElement = match def.widget {
        WidgetKind::Switch => {
            let id = def.id;
            let on = ctx.on.clone();
            let refresh = ctx.refresh.clone();
            let switch = Switch::new(id.key())
                .with_size(component_size())
                .checked(view.bool_of(id))
                .on_click(move |checked: &bool, _, cx| {
                    on(DialogEvent::SettingChanged {
                        id,
                        value: SettingValue::Bool(*checked),
                    });
                    refresh(cx);
                });
            switch.into_any_element()
        }
        WidgetKind::Select { .. } | WidgetKind::FontSelect => {
            // FontSelect 的候选项来源不同（系统字体枚举），但控件与取值路径同款：
            // 实体在装配期建好，这里只消费句柄
            let Some(entity) = select_of(selects, def.id) else {
                debug_assert!(false, "Select 行缺装配实体：{:?}", def.id);
                return empty_row(def);
            };
            let select = Select::new(entity)
                .with_size(component_size())
                .placeholder(crate::tr(def.label));
            // 字体下拉的搜索框占位（开搜索在 `SelectState` 构造处——渲染期
            // 只提供文案）。白名单下拉（主题/格式/级别…）项少且固定，不给占位。
            let select = if matches!(def.widget, WidgetKind::FontSelect) {
                select.search_placeholder(crate::tr("setting.font_family.search"))
            } else {
                select
            };
            select.into_any_element()
        }
        WidgetKind::Number { min, max, step } => {
            // 数值步进行（界面字号）：控件自带上/下界与步长，越界归一在控件内
            // 完成；上行 `Number`（注册表 store 会再兜一次边界）
            let id = def.id;
            let on = ctx.on.clone();
            let refresh = ctx.refresh.clone();
            crate::ui::widgets::number::number_stepper(
                id.key(),
                view.number_of(id),
                min,
                max,
                step,
                "px",
                Rc::new(move |v: u32, cx: &mut App| {
                    on(DialogEvent::SettingChanged {
                        id,
                        value: SettingValue::Number(v),
                    });
                    refresh(cx);
                }),
            )
            .into_any_element()
        }
        WidgetKind::Color => {
            // T-SEL 走查 #3：主题色行（gpui-kit ColorPicker；值 = hex 文本，
            // 空 = 主题默认——picker 实体由面板装配期预建）
            let Some(entity) = pickers
                .iter()
                .find(|(sid, _)| *sid == def.id)
                .map(|(_, e)| e)
            else {
                debug_assert!(false, "Color 行缺装配实体：{:?}", def.id);
                return empty_row(def);
            };
            gpui_kit::component::color_picker::ColorPicker::new(entity)
                .with_size(component_size())
                .into_any_element()
        }
        WidgetKind::CheckGroup { keys } => check_group(keys, view.set_of(def.id), def.id, ctx),
        WidgetKind::ExtGroup => assoc_group(view, def.id, ctx, cx),
        WidgetKind::Text => {
            // 文本行：控件列显示当前值（空 = 「系统默认」；日志目录空值
            // 直接给出当前进程真在写的路径——「配了 logs 却找不到日志」的
            // 第一反应是不知道去哪找）
            let shown: String = match view.text_of(def.id) {
                "" if def.id == SettingId::LogDir => format!(
                    "{}（{}）",
                    hzip_log::effective_log_dir().display(),
                    crate::tr("setting.text.system_default")
                ),
                "" => crate::tr("setting.text.system_default"),
                cur => cur.to_string(),
            };
            // 走查 #9：路径可能很长——truncate（nowrap + 省略号）防撑宽
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .truncate()
                .child(shown)
                .into_any_element()
        }
        WidgetKind::ColumnsBlock => {
            let fallback = ColumnsConfig::default();
            let cols = view.columns_of(def.id).unwrap_or(&fallback);
            columns_section(cols, ctx, cx)
        }
    };

    // 测试驱动锚点：`cfg!(test)` 下包一层可查边界的 div（`debug_bounds("ctl-<key>")`
    // → `simulate_click` 真实点击该控件；发布构建不记录，无运行时开销）。
    //
    // 锚点必须**只包控件本身**：`ctl-*` 的中心被点击测试用作命中点，若把行带
    // （`w_full`）包进锚点，中心会落到控件之外的空白（`switch_click_*` 实测失败）。
    let control = if cfg!(test) {
        let key = format!("ctl-{}", def.id.key());
        div()
            .min_w_0()
            .debug_selector(move || key.clone())
            .child(control)
            .into_any_element()
    } else {
        control
    };
    // 行带（[`ROW_BAND`]）：控件顶边进带、带内垂直居中——单行控件的
    // **中心**于是恒定落在带中心（与其固有高度 20/21/22.5/32 无关）
    let control = band(control).into_any_element();

    // 行内动作（Clear / FilePicker）：**两行布局**——走查 #9：长路径 +
    // 多个按钮挤一行会超出弹窗宽度；改为路径行（truncate 截断）在上、
    // 按钮行在下（行内动作只挂在 Text 行，不影响其它控件类型）
    let inline = inline_actions(def.id, ctx);
    let child = if inline.is_empty() {
        control
    } else {
        v_flex()
            .gap(sp(1.))
            .w_full()
            .min_w_0()
            // 两行块：**首行**（控件）已在 [`ROW_BAND`] 内，按钮行紧随其下
            // ——首行与标签同轴，块其余部分自然流下（块中心不再是行中心，
            // 这是刻意的：行带只定义首行基准）
            .child(control)
            // 按钮行：每个按钮 `flex_none`（**不缩**——被压瘦到文字显示不全正是用户
            // 五轮报的症状：「常规 → 日志目录」那行按钮显示不全），并允许换行。
            // 注：换行要生效得让本行拿到**确定宽度**；kit 的 `FormField` 控件列
            // （`div().w_full().flex_1()`，min-width:auto）会以内容 min-content 兜底，
            // 故真正的保证是**弹窗宽度够**（`CARD_W_BASE` 700）+ 锚点
            // `inline_action_buttons_fit_at_every_density` 逐档断言按钮不越出内容列。
            .child(h_flex().flex_wrap().min_w_0().gap(sp(2.)).children(inline))
            .into_any_element()
    };
    // 标签带与否取决于控件形态：密集列表（勾选组 / 列块）与块首行**顶对齐**，
    // 其余行（单行控件）行进带内居中（契约见 settings_layout 的对齐用例）。
    let dense = matches!(
        def.widget,
        WidgetKind::CheckGroup { .. } | WidgetKind::ColumnsBlock
    );
    form_field()
        .items_start()
        .label_fn(move |_, _| {
            if dense {
                dense_label_with_help(def).into_any_element()
            } else {
                label_with_help(def).into_any_element()
            }
        })
        .child(child)
}

/// 降级占位行（装配不变量被破坏：不 panic、不静默丢行）。
fn empty_row(def: &'static SettingDef) -> Field {
    form_field()
        .items_start()
        .label_fn(move |_, _| label_with_help(def))
        .child(band(div().child("—")))
}

/// 文件关联降级行（本构建没有任何可打开的档案格式）。
fn assoc_fallback(def: &'static SettingDef) -> Field {
    let note = crate::tr("setting.assoc.no_formats");
    form_field()
        .items_start()
        .label_fn(move |_, _| label_with_help(def))
        .child(band(div().child(note)))
}

/// 按 [`SettingId`] 取面板装配期预建的 Select 实体。
fn select_of(selects: &[(SettingId, SelectEntity)], id: SettingId) -> Option<&SelectEntity> {
    selects.iter().find(|(sid, _)| *sid == id).map(|(_, e)| e)
}

/// Checkbox 组渲染（白名单 keys；勾/取消均**整组**上行）。
fn check_group(
    keys: &'static [(&'static str, &'static str)],
    checked: &[String],
    id: SettingId,
    ctx: &PageCtx,
) -> AnyElement {
    // 行距（用户口径）：紧凑 0 / 标准 1 / 宽松 2px **上下各一份** —— 与文件列表
    // `row_h = 行框 + 2×row_gap` 同一口径（`spacing::row_gap` 的唯一来源）。
    // 不再逐行进 `band()`：块的每一行都是等高的复选行，行带只会把每行撑到
    // `row_band`（标准 32px）并把行距变成「带内余量 + 容器 gap」两段，正是用户
    // 要收的那种松散（标签与块首行的对齐改由密集标签承担，见 `dense_label_with_help`）。
    let row_gap = crate::ui::spacing::row_gap();
    let mut list = v_flex().w_full();
    for (ix, (key, label)) in keys.iter().enumerate() {
        let on = ctx.on.clone();
        let refresh = ctx.refresh.clone();
        let base = checked.to_vec();
        let check = crate::ui::widgets::checkbox(format!("{}-{key}", id.key()), crate::tr(label))
            .checked(checked.iter().any(|s| s == key))
            .on_click(move |new_checked: &bool, _, cx| {
                on(DialogEvent::SettingChanged {
                    id,
                    value: SettingValue::StrSet(settings_registry::toggle_set(
                        keys,
                        &base,
                        key,
                        *new_checked,
                    )),
                });
                refresh(cx);
            });
        // 上下各一份行距（行与行之间自然为 2×row_gap）；组内每行同一节奏
        // （首行加测试锚点：块状控件的「首行同轴」只有它能被断言）
        let content = if ix == 0 && cfg!(test) {
            let sel = format!("ctlrow0-{}", id.key());
            div()
                .debug_selector(move || sel.clone())
                .child(check)
                .into_any_element()
        } else {
            check.into_any_element()
        };
        list = list.child(h_flex().w_full().py(row_gap).child(content));
    }
    list.into_any_element()
}

/// 文件关联候选勾选（候选 = 本构建可关联的单点扩展名，见
/// [`crate::ui::contracts::associable_extensions`]）。
///
/// 版式（用户要求「一行多个选项」，参考 Bandizip）：按
/// [`hzip_format::exts::ExtKind`] 分三组，每组一个小标题 + **流式换行的等宽
/// 勾选格**（`flex_wrap`：窗宽变化时列数自然增减）。
///
/// 展示清单**取自表**（表序）而非候选数组：候选只用于「本构建是否可用」判定，
/// 顺序与分组由 `hzip_format::exts` 单独决定——两处派生自同一张表，不会漂移。
fn assoc_group(view: &SettingsView, id: SettingId, ctx: &PageCtx, cx: &App) -> AnyElement {
    use hzip_format::exts::{ExtKind, FORMAT_EXTS};

    let checked = view.set_of(id);
    let candidates = view.assoc_candidates();
    let m = crate::ui::spacing::metrics();
    let mut col = v_flex().gap(m.gap_lg()).w_full();

    let mut first_group = true;
    for (kind, key, label_key) in [
        (ExtKind::Archive, "archive", "assoc.group.archive"),
        (ExtKind::TarComposite, "composite", "assoc.group.composite"),
        (ExtKind::Stream, "stream", "assoc.group.stream"),
    ] {
        let exts: Vec<&str> = FORMAT_EXTS
            .iter()
            .filter(|e| e.kind == kind)
            .flat_map(|e| e.exts.iter().copied())
            .filter(|e| !e.contains('.') && candidates.iter().any(|c| c == e))
            .collect();
        if exts.is_empty() {
            continue;
        }
        let mut grid = h_flex().flex_wrap().gap(m.gap_sm()).w_full();
        for ext in exts {
            let on = ctx.on.clone();
            let refresh = ctx.refresh.clone();
            let base = checked.to_vec();
            let picked = ext.to_string();
            let check = crate::ui::widgets::checkbox(format!("assoc-{ext}"), ext)
                .checked(checked.iter().any(|s| s == ext))
                .on_click(move |new_checked: &bool, _, cx| {
                    on(DialogEvent::SettingChanged {
                        id,
                        value: SettingValue::StrSet(settings_registry::assoc_toggle(
                            &base,
                            &picked,
                            *new_checked,
                        )),
                    });
                    refresh(cx);
                });
            // 格宽**随内容**（2026-09-21 用户二轮：固定 96px 的三档都太宽 ⇒
            // 收窄到勾选框自身的宽度，只留 pad_cell 的左右内边距作点击区）；
            // 上下内边距按密度走 `row_gap`（紧凑 0 / 标准 1 / 宽松 2px，与
            // 上下文菜单的复选行 `check_group` **同一口径**）。
            let mut cell = div()
                .py(crate::ui::spacing::row_gap())
                .px(crate::ui::spacing::metrics().pad_cell());
            if cfg!(test) {
                // 布局锚点：断言「一行多个选项」靠相邻格 y 相同（渲染层唯一证据）
                let key = format!("assoc-cell-{ext}");
                cell = cell.debug_selector(move || key.clone());
            }
            grid = grid.child(cell.child(check));
        }
        let mut header = div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(crate::tr(label_key));
        if cfg!(test) {
            let key = format!("assoc-group-{key}");
            header = header.debug_selector(move || key.clone());
        }
        // 组标题进 [`ROW_BAND`]（块首行与标签同轴；勾选格保持流式紧凑）
        // 首组标题加测试锚点（块「首行同轴」的可断言证据）
        let head = band(header);
        let head = if first_group && cfg!(test) {
            let sel = format!("ctlrow0-{}", id.key());
            div().debug_selector(move || sel.clone()).child(head)
        } else {
            head
        };
        col = col.child(v_flex().gap(m.gap_sm()).child(head).child(grid));
        first_group = false;
    }

    col.into_any_element()
}

/// 行内动作按钮（`Clear` = 置空文本 → 系统默认；`FilePicker` = 后台选可执行文件）。
fn inline_actions(id: SettingId, ctx: &PageCtx) -> Vec<AnyElement> {
    let mut out = Vec::new();
    for action in ACTIONS
        .iter()
        .filter(|a| action_target(&a.kind) == Some(id))
    {
        let on = ctx.on.clone();
        let refresh = ctx.refresh.clone();
        let button = match &action.kind {
            ActionKind::Emit(_) => continue,
            ActionKind::Clear(target) => {
                let target = *target;
                Button::new(action.key)
                    .with_size(component_size())
                    .label(crate::tr(action.label))
                    .on_click(move |_, _, cx| {
                        on(DialogEvent::SettingChanged {
                            id: target,
                            value: SettingValue::Text(String::new()),
                        });
                        refresh(cx);
                    })
            }
            ActionKind::Open(target) => {
                let target = *target;
                Button::new(action.key)
                    .with_size(component_size())
                    .label(crate::tr(action.label))
                    .on_click(move |_, _, cx| {
                        // 打开行为走后台执行器：不阻塞 UI 线程（与 FilePicker 同纪律）
                        cx.spawn(async move |cx| {
                            cx.background_executor()
                                .spawn(async move {
                                    match target {
                                        OpenTarget::LogDir => {
                                            let _ = hzip_platform::open::reveal_in_file_manager(
                                                &hzip_log::effective_log_dir(),
                                                false,
                                            );
                                        }
                                        OpenTarget::ConfigFile => {
                                            let _ = hzip_platform::open::open_with_default_app(
                                                &hzip_config::Config::path(),
                                            );
                                        }
                                    }
                                })
                                .await;
                        })
                        .detach();
                    })
            }
            ActionKind::FilePicker { target } => {
                let target = *target;
                Button::new(action.key)
                    .with_size(component_size())
                    .label(crate::tr(action.label))
                    .on_click(move |_, _, cx| {
                        let on = on.clone();
                        let refresh = refresh.clone();
                        cx.spawn(async move |cx| {
                            // 原生选择器阻塞调用禁止在 UI 线程直呼（file_picker 契约）
                            let picked = cx
                                .background_executor()
                                .spawn(async {
                                    smol::unblock(crate::app::file_picker::pick_executable).await
                                })
                                .await;
                            let Some(path) = picked else {
                                return; // 取消 = 无动作
                            };
                            on(DialogEvent::SettingChanged {
                                id: target,
                                value: SettingValue::Text(path.display().to_string()),
                            });
                            cx.update(|cx| refresh(cx));
                        })
                        .detach();
                    })
            }
        };
        // 按钮放进不可压缩的盒子（`flex_none`）+ 测试锚点：按钮不再被压扁到文字
        // 不全（宽度由弹窗尺寸保证，见 `settings_layout::CARD_W_BASE` 与锚点
        // `inline_action_buttons_fit_at_every_density`）。
        let key = action.key;
        out.push(
            div()
                .flex_none()
                .when(cfg!(test), |d| {
                    d.debug_selector(move || format!("act-{key}"))
                })
                .child(button)
                .into_any_element(),
        );
    }
    out
}

/// 动作目标项（`Emit` 无目标）。
fn action_target(kind: &ActionKind) -> Option<SettingId> {
    match kind {
        ActionKind::Emit(_) => None,
        // 两个「打开」动作都挂在日志目录行（那里正是用户找不到东西的地方）
        ActionKind::Open(_) => Some(SettingId::LogDir),
        ActionKind::Clear(target) | ActionKind::FilePicker { target } => Some(*target),
    }
}

// —— 浏览列块（查看器页内嵌；T-G54 迁移） ——

/// 列块渲染（走查 #8：名称文本 + 上移/下移/眼睛三钮的紧凑行）。
///
/// - 显隐 = 眼睛按钮（可见 = Eye，点击隐藏；隐藏 = EyeOff 置灰名称，点击
///   恢复）；名称列锁定（眼睛禁用）。
/// - **紧凑模式**（用户要求）：行内边距 `py_0p5` + 间距 `gap_1`，控件为 kit
///   最小档（xsmall）——列项密集排布，不占满表单行高。
/// - 不再嵌套 Form——控件列与其它设置行对齐。
///
/// 任一变更全量重排 `columns.order` 后经
/// `SettingChanged { id: Columns, .. }` 上行（变更即发）。
pub fn columns_section(columns: &ColumnsConfig, ctx: &PageCtx, cx: &App) -> AnyElement {
    let id = SettingId::Columns;
    let mut list = v_flex().gap(sp(0.)).w_full();

    for (ix, &col) in columns.order.iter().enumerate() {
        let locked = col == BrowseColumnId::Name;
        let visible = locked || !columns.hidden.contains(&col);
        let first = ix == 0;
        let last = ix + 1 == columns.order.len();

        // 上移/下移/显隐切换：各自独立预计算目标配置（闭包捕获 owned 数据）
        let moved_up = move_to(columns, ix, ix.wrapping_sub(1));
        let moved_down = move_to(columns, ix, ix + 1);

        let mut btn_up = Button::new(format!("col-up-{}", col.key()))
            .with_size(component_size())
            .ghost()
            .xsmall()
            .icon(AppIcon::ArrowUp)
            .tooltip(crate::tr("setting.columns.move_up"))
            .disabled(locked || first);
        if let Some(next) = moved_up {
            let on = ctx.on.clone();
            let refresh = ctx.refresh.clone();
            btn_up = btn_up.on_click(move |_, _, cx| {
                on(DialogEvent::SettingChanged {
                    id,
                    value: SettingValue::Columns(next.clone()),
                });
                refresh(cx);
            });
        }

        let mut btn_down = Button::new(format!("col-down-{}", col.key()))
            .with_size(component_size())
            .ghost()
            .xsmall()
            .icon(AppIcon::ArrowDown)
            .tooltip(crate::tr("setting.columns.move_down"))
            .disabled(locked || last);
        if let Some(next) = moved_down {
            let on = ctx.on.clone();
            let refresh = ctx.refresh.clone();
            btn_down = btn_down.on_click(move |_, _, cx| {
                on(DialogEvent::SettingChanged {
                    id,
                    value: SettingValue::Columns(next.clone()),
                });
                refresh(cx);
            });
        }

        // 眼睛按钮：可见 → Eye（点击隐藏）；隐藏 → EyeOff（点击恢复）。
        // 名称列恒可见：禁用态作视觉锚（不可关掉唯一身份列）。
        let mut btn_eye = Button::new(format!("col-eye-{}", col.key()))
            .with_size(component_size())
            .ghost()
            .xsmall()
            .icon(if visible {
                AppIcon::Eye
            } else {
                AppIcon::EyeOff
            })
            .tooltip(crate::tr(if visible {
                "setting.columns.hide"
            } else {
                "setting.columns.show"
            }))
            .disabled(locked);
        if !locked {
            let on = ctx.on.clone();
            let refresh = ctx.refresh.clone();
            let base = columns.clone();
            btn_eye = btn_eye.on_click(move |_, _, cx| {
                let next = toggle_visible(&base, col, !visible);
                on(DialogEvent::SettingChanged {
                    id,
                    value: SettingValue::Columns(next),
                });
                refresh(cx);
            });
        }

        // 名称文本：隐藏列置灰（muted），可见列常规前景色。
        // 行高显式取配置值 [`crate::ui::spacing::line_height_em`]：行高由本行最高的
        // 元素定（名称文本 1.5em > 图标钮），不显式给就会退回 gpui 默认行高
        // φ/1.5em，行距又变松（用户口径：密度只管间距，行框由 `[ui].line_height` 定）。
        let name = div()
            .text_sm()
            .line_height(gpui_kit::relative(crate::ui::spacing::line_height_em()))
            .text_color(if visible {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            });

        // 密集行（用户口径）：上下各 `row_gap`（紧凑 0 / 标准 1 / 宽松 2px，
        // 与文件列表同一口径）；行内控件间距收紧（gap_1 / 按钮组 gap_1），
        // 控件本身已是 kit 最小档（xsmall）——六列一屏可见。
        // 首行不再单独进 `band()`：标签已改为与块首行顶对齐
        // （`dense_label_with_help`），逐行进带只会把行距变成「带内余量 + gap」。
        let row = h_flex()
            .gap(sp(1.))
            .items_center()
            .w_full()
            .py(crate::ui::spacing::row_gap())
            // 首行加测试锚点：块「首行与标签同顶」的可断言证据。锚点包**名称
            // 文本**（其顶边 = 首行文字行框顶边，与 `lbl-*` 同一比较面）。
            .child({
                let text = name.child(col.label());
                if first && cfg!(test) {
                    let sel = format!("ctlrow0-{}", id.key());
                    div()
                        .debug_selector(move || sel.clone())
                        .child(text)
                        .into_any_element()
                } else {
                    text.into_any_element()
                }
            })
            .child(
                h_flex()
                    .ml_auto()
                    .gap(sp(1.))
                    .child(btn_up)
                    .child(btn_down)
                    .child(btn_eye),
            );
        list = list.child(row);
    }

    list.into_any_element()
}

/// 切换单列显隐（hidden 集合增删；order 不动）。
pub fn toggle_hidden(
    hidden: &[BrowseColumnId],
    col: BrowseColumnId,
    visible: bool,
) -> Vec<BrowseColumnId> {
    if visible {
        hidden.iter().copied().filter(|c| *c != col).collect()
    } else if !hidden.contains(&col) {
        let mut h = hidden.to_vec();
        h.push(col);
        h
    } else {
        hidden.to_vec()
    }
}

/// 显隐切换后的整份列配置：**只改 hidden**，`order` 原样保留
/// （回归锚点：旧实现用 `FULL_ORDER` 重建 order，静默重置用户重排）。
pub fn toggle_visible(
    columns: &ColumnsConfig,
    col: BrowseColumnId,
    visible: bool,
) -> ColumnsConfig {
    let mut next = columns.clone();
    next.hidden = toggle_hidden(&columns.hidden, col, visible);
    next
}

/// 构造把 `from` 位置列移动到 `to` 位置后的新 ColumnsConfig（越界/原位 → None）。
pub fn move_to(columns: &ColumnsConfig, from: usize, to: usize) -> Option<ColumnsConfig> {
    if from >= columns.order.len() || to >= columns.order.len() || from == to {
        return None;
    }
    let mut next = columns.clone();
    let col = next.order.remove(from);
    next.order.insert(to, col);
    Some(next)
}

/// 上下文菜单状态行：系统实况 + 「有未应用的更改」提示。
///
/// 状态色的语义与仓库其余处一致（危险/警告/成功/次要取自主题 token，07 红线）：
/// 未安装与不支持用次要色（不是错误，只是没做），受限用警告色，已安装用成功色。
/// 已勾选格式的双击实况判定（**纯函数**：渲染与测试共用，不碰注册表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssocStatus {
    /// 双击已经由 hzip 打开。
    Effective(String),
    /// 已勾选但系统侧还是别人（点「应用关联」后就归 hzip）。
    Pending(String),
    /// 被系统的**用户显式默认**压着（Windows `UserChoice`，哈希保护、程序写不进去）。
    ///
    /// 出路：设置里的「经典模式」（应用时清掉那一层）或页脚的「打开系统
    /// 『默认应用』设置」按钮。**不再**给每行配「打开方式」按钮——用户口径
    /// 2026-09-29：那个按钮要求在模态对话框里再选一次程序，价值不如经典模式直接。
    Blocked {
        /// 扩展名（不带点）。
        ext: String,
        /// 占用者的可读名（查不到 → 标识原文）。
        owner: String,
    },
}

/// 逐个判定已勾选格式的双击实况。
///
/// 判据是平台层给出的**跨平台三态**（见 [`ExtAssocState`]）：已归我们 ⇒
/// Effective；被**程序改不动**的默认压着（只有 Windows 的 `UserChoice` 会这样）
/// ⇒ Blocked；其余（系统里仍是别人、或干脆没有默认程序）⇒ Pending——点一次
/// 「应用关联」就归我们。
///
/// [`ExtAssocState`]: hzip_platform::assoc::ExtAssocState
pub fn assoc_status_of(
    states: &[(String, hzip_platform::assoc::ExtAssocState)],
) -> Vec<AssocStatus> {
    states
        .iter()
        .map(|(ext, st)| {
            if st.is_effective() {
                AssocStatus::Effective(ext.clone())
            } else if st.blocked_by_other() {
                AssocStatus::Blocked {
                    ext: ext.clone(),
                    owner: st.owner_name().unwrap_or_default().to_string(),
                }
            } else {
                AssocStatus::Pending(ext.clone())
            }
        })
        .collect()
}

/// 扩展名清单 → 「.zip、.7z」（空清单 → 空串）。
fn join_exts(exts: &[String]) -> String {
    exts.iter()
        .map(|e| format!(".{e}"))
        .collect::<Vec<_>>()
        .join("、")
}

/// 文件关联页状态行：双击实况 + 被占格式的「打开方式」夺回入口。
fn assoc_status_row(view: &SettingsView, ctx: &PageCtx, cx: &App) -> Field {
    let statuses = assoc_status_of(view.assoc_states());
    let mut lines = v_flex().gap(sp(1.));

    if statuses.is_empty() {
        lines = lines.child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(crate::tr("assocstatus.none")),
        );
    }

    // 已生效 / 待应用：各聚一行（同一状态的多格式不必逐行占地方）
    let group = |pick: fn(&AssocStatus) -> Option<String>, key: &'static str| {
        let mut exts = Vec::new();
        for st in &statuses {
            if let Some(e) = pick(st) {
                exts.push(e);
            }
        }
        (!exts.is_empty()).then(|| {
            div()
                .text_color(cx.theme().muted_foreground)
                .child(crate::tr_with(key, &[("exts", &join_exts(&exts))]))
        })
    };
    if let Some(line) = group(
        |s| match s {
            AssocStatus::Effective(e) => Some(e.clone()),
            _ => None,
        },
        "assocstatus.effective",
    ) {
        lines = lines.child(line);
    }
    if let Some(line) = group(
        |s| match s {
            AssocStatus::Pending(e) => Some(e.clone()),
            _ => None,
        },
        "assocstatus.pending",
    ) {
        lines = lines.child(line);
    }

    // 被占用：逐扩展名一行——占用者可能各不相同，且每行配一个「打开方式」按钮
    let mut blocked_any = false;
    for st in &statuses {
        let AssocStatus::Blocked { ext, owner } = st else {
            continue;
        };
        // 行文案 + （若被系统「用户显式选择」压着）指向系统「默认应用」页的说明。
        // 用户口径 2026-09-29：**不再**给每个格式配「打开方式」按钮——那个按钮
        // 需要用户在一个模态对话框里再选一次程序，而「经典模式」（设置项）打开后
        // 应用关联即直接生效；真被哈希保护挡住时，页脚的「默认应用设置」按钮就够。
        let key = format!("act-assoc-blocked-{ext}");
        let row =
            h_flex()
                .gap(sp(2.))
                .items_center()
                .child(div().text_color(cx.theme().warning).child(crate::tr_with(
                    "assocstatus.blocked",
                    &[("exts", &format!(".{ext}")), ("owner", owner)],
                )));
        let row = if cfg!(test) {
            row.child(div().debug_selector(move || key.clone()))
        } else {
            row
        };
        lines = lines.child(row);
        blocked_any = true;
    }

    // 批量入口（**整页一个**，不是每行一个——元素 id 必须唯一，且重复按钮没有
    // 意义）：Win11 深链到 hzip 的条目页，本程序注册的全部扩展名一屏列全。
    // 只在真有「被占行」时出现——那正是 Windows 特有的、需要用户本人出手的场景。
    if blocked_any {
        let on_settings = ctx.on.clone();
        let refresh_settings = ctx.refresh.clone();
        lines = lines.child(
            Button::new("assoc-open-app-settings")
                .with_size(component_size())
                .label(crate::tr("assocstatus.open_settings"))
                .on_click(move |_, _, cx| {
                    on_settings(crate::ui::contracts::DialogEvent::SettingsAssocOpenAppSettings);
                    refresh_settings(cx);
                }),
        );
    }

    // 系统侧操作失败原因（apply/remove/打开方式）——警告色直达
    if let Some(reason) = view.assoc_error() {
        lines = lines.child(
            div()
                .text_color(cx.theme().warning)
                .child(crate::tr_with("assocstatus.failed", &[("reason", reason)])),
        );
    }

    form_field()
        .items_start()
        .label_fn(move |_, _| {
            label_cell(
                crate::tr("setting.assoc_status.label"),
                Some("setting.assoc_status.desc"),
                "assoc-status",
            )
        })
        .child(band(lines))
}

fn ctx_status_row(view: &SettingsView, cx: &App) -> Field {
    use crate::app::state::{CtxMenuMode, CtxMenuStatus};

    let mode_label = |m: CtxMenuMode| -> SharedString {
        crate::tr(match m {
            CtxMenuMode::ModernTopLevel => "ctxstatus.mode.modern",
            CtxMenuMode::LegacyMenu => "ctxstatus.mode.legacy",
            CtxMenuMode::FinderSync => "ctxstatus.mode.finder",
            CtxMenuMode::Services => "ctxstatus.mode.services",
            CtxMenuMode::Desktop => "ctxstatus.mode.desktop",
        })
        .into()
    };

    let (text, danger) = match view.ctx_status() {
        None => (crate::tr("ctxstatus.not_installed"), false),
        Some(CtxMenuStatus::NotInstalled) => (crate::tr("ctxstatus.not_installed"), false),
        Some(CtxMenuStatus::NeedsRestart) => (crate::tr("ctxstatus.restart"), false),
        Some(CtxMenuStatus::Installed { mode, items }) => (
            crate::tr_with(
                "ctxstatus.installed",
                &[("mode", &mode_label(*mode)), ("items", &items.to_string())],
            ),
            false,
        ),
        Some(CtxMenuStatus::Degraded { reason }) => (
            crate::tr_with("ctxstatus.degraded", &[("reason", reason)]),
            true,
        ),
        Some(CtxMenuStatus::Unsupported { reason }) => (
            crate::tr_with("ctxstatus.unsupported", &[("reason", reason)]),
            true,
        ),
    };

    // 危险态（受限/不支持）与普通态（未安装/已安装）用不同语义色——
    // 主题 token 而非裸色值（07 红线）
    let color = if danger {
        cx.theme().warning
    } else {
        cx.theme().muted_foreground
    };
    let status_line = div().text_color(color).child(text);

    // 「有未应用的更改」：草稿存在即提示（点「应用」后草稿清空、提示消失）
    let dirty = view.has_pending_ctx_changes().then(|| {
        div()
            .text_color(cx.theme().primary)
            .child(crate::tr("setting.ctx_dirty"))
    });

    // 走查 #8：状态行帮助同样收敛为 ？ 悬浮（描述不常驻）；
    // 走查 #11：标签同款 max_w 封顶 + flex_1 换行结构（长语言不裁剪）
    form_field()
        .items_start()
        .label_fn(move |_, _| {
            label_cell(
                crate::tr("setting.ctx_status.label"),
                Some("setting.ctx_status.desc"),
                "ctx-status",
            )
        })
        .child({
            // 首行（系统实况）进 [`ROW_BAND`]；「有未应用的更改」提示流于其下
            // （首行加测试锚点：本行不在注册表内，对齐契约靠它覆盖）
            let head = band(status_line);
            let head = if cfg!(test) {
                div().debug_selector(|| "ctl-ctx-status".into()).child(head)
            } else {
                head
            };
            let mut col = v_flex().gap(sp(1.)).child(head);
            if let Some(d) = dirty {
                col = col.child(d);
            }
            col
        })
}

#[cfg(test)]
#[path = "tests/settings_render.rs"]
mod tests;
