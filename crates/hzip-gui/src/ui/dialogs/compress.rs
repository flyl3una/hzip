//! 压缩对话框（T-GUI T10 自 dialogs.rs 拆分；09 设计稿落地的分区式改版）。
//!
//! # 形态（09 §2/§3）
//!
//! ```text
//! 源摘要卡
//! ── 基本 ────────────────────────
//! 文件名 [____] .zip [浏览…]
//! 保存到 [____]
//! 格式   [zip ▾]
//! 压缩方式 [Deflate ▾]（单候选格式禁用 + 原因行）
//! 压缩级别 更快 ●━━━●━━━● 更慢   + 档位名 + 实参提示行
//! ▸ 高级选项        分卷 … · 线程 … · 固实 …
//! ▸ 加密            未加密
//! [摘要] 格式 · 方式 · 级别 · …
//! ⚠ 单流格式提示 / 目标已存在提示 / 错误行
//!                        [取消] [开始压缩]
//! ```
//!
//! # 承载形态（2026-09 返工）
//!
//! 本面板是**参数窗的内容视图**（窗口级布局：头部固定 / 正文滚动 / 底部固定），
//! 不是 `Dialog` 弹层——旧实现用 `Dialog` 承载，参数一多弹层就长出窗口底部
//! （底部按钮不可见、正文滚不动）。理由与三条布局不变量见 `super::panel_shell`。
//!
//! # 契约（T-G11 §3.1 沿用）
//!
//! 纯 props：字段 = `CompressDialogVm` 的 UI 投影；实体（`SelectState`/
//! `InputState`/`SliderState`）由装配方在 `open_dialog` builder 内**预构建**后
//! 放入 props（窗口亲和）。本文件不做校验、不建 `JobSpec`、不缓存决策——一律
//! 经 `DialogEvent` 上抛。
//!
//! # 能力门控（2026-09 按用户要求返工）
//!
//! 规则：**该格式不支持的参数一律不渲染**（不是禁用）——参照 7-Zip 的压缩参数
//! 弹窗：不同格式露出的参数面不同（tar 只打包 → 没有压缩方式/字典/分卷；
//! zip 有方式/分卷/更新方式；7z 有字典/线程）。
//!
//! 旧策略是「禁用 + 一行灰字原因」，理由是「隐藏会让参数集随格式跳动」——
//! 实际用下来的问题是：本项目每个格式的 `caps.methods` 都只有 1 个候选，
//! 于是「压缩方式」对**所有**格式都是灰的（点不动也看不懂），用户直接反馈
//! 「不能选就隐藏」。定稿以用户体感为准。
//!
//! 门控来源仍是 `FormatCaps`（`op_window::format_caps` → core handler），
//! 面板只消费 props 里的 `*_supported` 布尔，不自己判断格式。

use crate::ui::constant::{DIALOG_W, SPLIT_INPUT_W};
use crate::ui::spacing::{component_size, sp};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Dialog, DialogFooter};
use gpui_kit::component::form::Field as FormField;
use gpui_kit::component::h_flex;
use gpui_kit::component::input::{Input, InputState, NumberInput};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::component::{ActiveTheme, Disableable, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, SharedString, div};

use super::{
    HintTone, dialog_form, dialog_shell, error_line, field_col, hint_line, level_detail_text,
    level_speed_key, muted_fg, panel_shell, section_header, summary_card,
};
use crate::ui::contracts::DialogEvent;

/// 压缩参数面板 props（字段 = `CompressDialogVm` 的 UI 投影 + 能力门控/派生文案）。
///
/// 形态见 [`compress_panel`]：**窗口级面板**（不是 `Dialog`）。
///
/// 派生文案（`ext` / `method_hint` / `level_detail` / 各 `*_summary` / `*_hint` /
/// `preview_text`）由装配方计算：它们要么依赖格式能力（`FormatCaps`，须经 core
/// 注册表查），要么依赖 `hzip_codec` 映射表——放在装配侧可让本组件保持纯渲染。
pub struct CompressPanelProps {
    // —— 源摘要 ——
    /// 压缩对象（首项路径或「首项 等 N 项」）
    pub summary: String,
    /// 条目统计（3 个文件 · 2 个目录 · 48.2 MB）
    pub summary_meta: String,

    // —— 基本 ——
    /// 文件名输入态（**只含文件名**，不含目录与扩展名）
    pub filename_input: Entity<InputState>,
    /// 输出目录输入态
    pub out_dir_input: Entity<InputState>,
    /// 输出扩展名（不含点；随格式派生，只读展示）
    pub ext: String,
    /// 文件名校验错误（danger 小字；空 = 合法）
    pub filename_error: String,
    /// 格式 Select 实体（预构建）
    pub format_select: Entity<SelectState<SearchableVec<String>>>,
    /// 压缩方式 Select 实体（预构建）
    pub method_select: Entity<SelectState<SearchableVec<String>>>,
    /// 压缩方式是否可选（单候选/无候选格式 → false = 禁用 + 原因行）
    pub method_selectable: bool,
    /// 压缩方式原因行（「该格式固定使用 LZMA2」/「该格式仅打包，不压缩」）
    pub method_hint: String,
    /// 压缩级别滑块实体（预构建；6 档离散）
    pub level_slider: Entity<SliderState>,
    /// 当前档位下标（刻度高亮 + 摘要）
    pub level_ix: usize,
    /// 级别实参（「LZMA2 字典 64 MB · 模式 normal」）
    pub level_detail: String,
    /// 速度档文案（「中」）
    pub level_speed: String,
    /// 是否渲染「压缩级别」行（`caps.methods` 非空 = 有压缩层；纯打包格式 tar
    /// 无压缩层 → 级别无意义，整行不渲染，同 `threads_supported` 模式）
    pub level_supported: bool,

    // —— 高级选项（折叠区） ——
    /// 折叠区展开态
    pub advanced_open: bool,
    /// 折叠区就地摘要（收起时也能读到取值）
    pub advanced_summary: String,
    /// 分卷预设 Select 实体（预构建）
    pub split_select: Entity<SelectState<SearchableVec<String>>>,
    /// 自定义分卷输入态（预构建；单位 MB）
    pub split_custom_input: Entity<InputState>,
    /// 是否显示自定义分卷输入（预设选到「自定义」时）
    pub split_custom_visible: bool,
    /// 分卷是否受支持（`caps.split`）
    pub split_supported: bool,
    /// 分卷原因/命名提示行
    pub split_hint: String,
    /// 线程数 Select 实体（预构建）
    pub threads_select: Entity<SelectState<SearchableVec<String>>>,
    /// 线程数原因行（7z 单线程说明）
    pub threads_hint: String,
    /// 是否渲染「线程数」（纯打包格式没有可并行的编码器 → false）
    pub threads_supported: bool,
    /// 字典大小 Select 实体（预构建；仅 7z 的 LZMA2 消费）
    pub dict_select: Entity<SelectState<SearchableVec<String>>>,
    /// 字典大小是否可用
    pub dict_supported: bool,
    /// 字典原因行
    pub dict_hint: String,
    /// 更新方式 Select 实体（预构建）
    pub update_select: Entity<SelectState<SearchableVec<String>>>,
    /// 更新方式是否支持追加（`caps.append`）
    pub append_supported: bool,
    /// 更新方式原因行
    pub update_hint: String,
    /// 固实开关值
    pub solid: bool,
    /// 固实是否受支持（`caps.solid`，当前仅 7z）
    pub solid_supported: bool,
    /// 固实原因行
    pub solid_hint: String,
    /// 排除 glob 输入态（预构建）
    pub excludes_input: Entity<InputState>,
    /// 排除隐藏文件
    pub exclude_hidden: bool,
    /// 跟随符号链接
    pub follow_symlinks: bool,
    /// 保留文件属性
    pub keep_metadata: bool,
    /// 完成后校验
    pub verify_after: bool,
    /// 单条失败继续
    pub best_effort: bool,

    // —— 加密（折叠区） ——
    /// 折叠区展开态
    pub enc_open: bool,
    /// 折叠区就地摘要（未加密 / 已设密码）
    pub enc_summary: String,
    /// 密码输入态（预构建；密文模式）
    pub password_input: Entity<InputState>,
    /// 确认密码输入态（预构建；密文模式）
    pub password_confirm_input: Entity<InputState>,
    /// 两次密码不一致提示（danger 小字）
    pub password_error: String,
    /// 加密方式 Select 实体（预构建）
    pub enc_select: Entity<SelectState<SearchableVec<String>>>,
    /// 加密是否受支持（`caps.encryption != None`）
    pub enc_supported: bool,
    /// 加密文件名开关值
    pub enc_names: bool,
    /// 加密文件名是否受支持（仅 7z 头加密）
    pub enc_names_supported: bool,
    /// 加密区原因行（写侧未启用 / 该格式不支持加密）
    pub enc_hint: String,
    /// 是否渲染加密折叠区：格式支持加密 **且** 写侧可用（T45/T46 接通后为真）
    pub enc_writable: bool,

    // —— 底部 ——
    /// 单流格式 + 多文件/目录提示（warn 小字；空 = 不显示）
    pub stream_hint: String,
    /// 目标已存在提示（warn 小字；空 = 不显示）
    pub exists_hint: String,

    /// 参数实时预览摘要
    pub preview_text: String,
    /// 是否允许提交（文件名合法且非空）
    pub start_enabled: bool,
    /// 错误/摘要文案（danger 小字；空 = 不渲染）
    pub error: String,
}

/// 输出扩展名后缀（只读 chip；随格式联动，用户不可编辑）。
fn ext_chip(cx: &App, ext: &str) -> impl IntoElement {
    div()
        .flex_none()
        .px(sp(2.))
        .py(sp(1.))
        // 圆角跟全局主题（`[ui].corner_radius`）
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
        .text_sm()
        .text_color(muted_fg(cx))
        .child(SharedString::from(format!(".{ext}")))
}

/// 压缩级别行：两端语义 + 滑块 + 档位名 + 实参提示。
///
/// 档位名**不在滑块下逐档对位**（gpui 无 translate/负边距，逐档对位会与轨道
/// 刻度错开），而是排成一行、选中档高亮——语义（有哪些档、当前在哪档）不丢，
/// 且不引入与控件宽度耦合的魔法值。
fn level_row(
    cx: &App,
    slider: &Entity<SliderState>,
    level_ix: usize,
    detail: &str,
    speed: &str,
) -> impl IntoElement {
    let names = crate::app::labels::labels_of(&crate::app::labels::LEVEL);
    let poles = h_flex()
        .w_full()
        .justify_between()
        .text_xs()
        .text_color(muted_fg(cx))
        .child(crate::tr("dlg.compress.pole_faster"))
        .child(crate::tr("dlg.compress.pole_smaller"));
    let ticks = h_flex()
        .w_full()
        .flex_wrap()
        .gap(sp(2.))
        .children(names.iter().enumerate().map(|(ix, name)| {
            let on = ix == level_ix;
            div()
                .text_xs()
                .when(on, |d| {
                    d.text_color(cx.theme().accent)
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                })
                .when(!on, |d| d.text_color(muted_fg(cx)))
                .child(SharedString::from(name.clone()))
        }));
    // 实参提示 = 档位实参 + 速度档（不显示「预计压缩率」——无可靠来源，见 09 §14）
    let hint = crate::tr_with(
        "dlg.compress.level_detail",
        &[("detail", detail), ("speed", speed)],
    );
    v_flex()
        .w_full()
        .gap(sp(2.))
        .child(poles)
        .child(div().w_full().child(Slider::new(slider)))
        .child(ticks)
        .child(hint_line(cx, HintTone::Muted, &hint))
}

/// 两列开关行（每行最多两项，等宽平分）。
///
/// `Checkbox` 在 gpui-kit 中**无 `disabled`**（对比 `Select`/`Input`/`Button`），
/// 故能力受限的开关项**不渲染**，改由一行原因说明——渲染一个点不动的勾选框
/// 比不渲染更糟（09 §4 的例外登记）。
///
/// 每格带一个以控件 id 命名的 `debug_selector`：kit 的 `Checkbox` 把传入 id 只
/// 当 **keyed state 键**（不注册为可观测元素），测试无法按 id 定位/点击——
/// 而「面板画了开关但没接线」正是本仓库踩过的坑（见 `panel_controls_reach_spec`），
/// 必须能被测试点得到。
fn switch_row(cells: Vec<(&'static str, AnyElement)>) -> impl IntoElement {
    h_flex()
        .w_full()
        .gap(sp(4.))
        .children(cells.into_iter().map(|(id, e)| {
            div()
                .flex_1()
                .debug_selector(move || id.to_string())
                .child(e)
        }))
}

/// 压缩对话框渲染。
///
/// 布局/事件映射（09 §2/§3）：源摘要卡 → 基本区（文件名/保存到/格式/压缩方式/
/// 压缩级别）→ 高级选项折叠区 → 加密折叠区 → 摘要 → 提示行 → 底部按钮。
/// 所有取值的落库由装配方订阅实体事件（`InputEvent::Change` /
/// `SelectEvent::Confirm` / `SliderEvent::Change`）或消费 `DialogEvent` 完成。
pub fn compress_panel(
    cx: &mut App,
    props: &CompressPanelProps,
    on: impl Fn(DialogEvent) + 'static,
) -> impl IntoElement {
    let on = std::rc::Rc::new(on);
    let on_browse = on.clone();
    let on_cancel = on.clone();
    let on_start = on.clone();

    // 底部（固定）：提示/错误先行——它们必须**不滚动**就在视野内
    // （提交失败原因、单流格式警告、改名告知）。
    let mut notes: Vec<AnyElement> = Vec::new();
    for (tone, text) in [
        (HintTone::Warn, props.stream_hint.as_str()),
        (HintTone::Warn, props.exists_hint.as_str()),
    ] {
        if !text.is_empty() {
            notes.push(hint_line(cx, tone, text).into_any_element());
        }
    }
    if !props.error.is_empty() {
        notes.push(error_line(cx, &props.error).into_any_element());
    }
    let footer = v_flex().w_full().gap(sp(2.)).children(notes).child(
        h_flex()
            .w_full()
            .justify_end()
            .gap(sp(2.))
            .child(
                Button::new("compress-cancel")
                    .with_size(component_size())
                    .label(crate::tr("dlg.cancel"))
                    .on_click(move |_, _, _| on_cancel(DialogEvent::CompressCancel)),
            )
            .child(
                Button::new("compress-start")
                    .with_size(component_size())
                    .primary()
                    .disabled(!props.start_enabled)
                    .label(crate::tr("dlg.compress.start"))
                    .on_click(move |_, _, _| on_start(DialogEvent::CompressStart)),
            ),
    );

    // ── 基本区（Form 块：仅 FormField） ──
    //
    // 压缩方式：候选 ≤1（本项目所有格式都只有 1 个候选）→ **整行不渲染**。
    // 留着禁用下拉只会让用户问「为什么不能选」（2026-09 用户报障）。
    let method_field = props.method_selectable.then(|| {
        FormField::new().label(crate::tr("field.method")).child(
            Select::new(&props.method_select)
                .with_size(component_size())
                .placeholder(crate::tr("field.method")),
        )
    });
    let basic = dialog_form()
        .child(
            FormField::new()
                .label(crate::tr("field.filename"))
                .child(field_col(
                    cx,
                    Some(&props.filename_error)
                        .filter(|e| !e.is_empty())
                        .map(|e| e.as_str()),
                    HintTone::Danger,
                    h_flex()
                        .w_full()
                        .flex_1()
                        .gap(sp(2.))
                        .child(
                            Input::new(&props.filename_input)
                                .with_size(component_size())
                                .flex_1(),
                        )
                        .child(ext_chip(cx, &props.ext))
                        .child(
                            Button::new("compress-browse")
                                .with_size(component_size())
                                .label(crate::tr("dlg.browse"))
                                .on_click(move |_, _, _| on_browse(DialogEvent::CompressBrowse)),
                        ),
                )),
        )
        .child(
            FormField::new()
                .label(crate::tr("field.out_dir"))
                .child(Input::new(&props.out_dir_input).with_size(component_size())),
        )
        .child(
            FormField::new().label(crate::tr("field.format")).child(
                Select::new(&props.format_select)
                    .with_size(component_size())
                    .placeholder(crate::tr("field.format")),
            ),
        )
        .when_some(method_field, |form, field| form.child(field))
        .when(props.level_supported, |form| {
            form.child(
                FormField::new()
                    .label(crate::tr("field.level"))
                    .child(level_row(
                        cx,
                        &props.level_slider,
                        props.level_ix,
                        &props.level_detail,
                        &props.level_speed,
                    )),
            )
        });

    // ── 高级选项折叠区内容（Form 块；未受支持的项不渲染，改由原因行说明） ──
    let mut adv = dialog_form();
    if props.split_supported {
        adv = adv.child(
            FormField::new()
                .label(crate::tr("field.split"))
                .child(field_col(
                    cx,
                    Some(&props.split_hint)
                        .filter(|h| !h.is_empty())
                        .map(|h| h.as_str()),
                    HintTone::Muted,
                    h_flex()
                        .w_full()
                        .flex_1()
                        .gap(sp(2.))
                        .child(
                            Select::new(&props.split_select)
                                .with_size(component_size())
                                .flex_1(),
                        )
                        .when(props.split_custom_visible, |row| {
                            row.child(
                                NumberInput::new(&props.split_custom_input)
                                    .with_size(component_size())
                                    .w(SPLIT_INPUT_W),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px(sp(2.))
                                    .py(sp(1.))
                                    .text_sm()
                                    .text_color(muted_fg(cx))
                                    .child(crate::tr("unit.mb")),
                            )
                        }),
                )),
        );
    }
    // 线程数：纯打包格式（tar，`caps.methods` 为空）没有可并行的编码器 → 不渲染
    if props.threads_supported {
        adv = adv.child(
            FormField::new()
                .label(crate::tr("field.threads"))
                .child(field_col(
                    cx,
                    Some(&props.threads_hint)
                        .filter(|h| !h.is_empty())
                        .map(|h| h.as_str()),
                    HintTone::Warn,
                    Select::new(&props.threads_select)
                        .with_size(component_size())
                        .placeholder(crate::tr("field.threads")),
                )),
        );
    }
    // 字典大小（仅 7z 的 LZMA2 消费；其它格式不渲染）
    if props.dict_supported {
        adv = adv.child(
            FormField::new()
                .label(crate::tr("field.dict_size"))
                .child(field_col(
                    cx,
                    Some(&props.dict_hint)
                        .filter(|h| !h.is_empty())
                        .map(|h| h.as_str()),
                    HintTone::Muted,
                    Select::new(&props.dict_select)
                        .with_size(component_size())
                        .placeholder(crate::tr("field.dict_size")),
                )),
        );
    }
    // 更新方式（仅 `caps.append` 格式有追加语义；其它格式不渲染——绝不静默新建）
    if props.append_supported {
        adv = adv.child(
            FormField::new()
                .label(crate::tr("field.update_mode"))
                .child(field_col(
                    cx,
                    Some(&props.update_hint)
                        .filter(|h| !h.is_empty())
                        .map(|h| h.as_str()),
                    HintTone::Muted,
                    Select::new(&props.update_select)
                        .with_size(component_size())
                        .placeholder(crate::tr("field.update_mode")),
                )),
        );
    }
    adv = adv.child(
        FormField::new()
            .label(crate::tr("field.excludes"))
            .child(Input::new(&props.excludes_input).with_size(component_size())),
    );

    // 开关项：逐项构造 AnyElement 后再切行——避免在 `.when()` 的两个分支里
    // 各写一份「排除隐藏文件」，那会让同一个事件闭包被移动两次（编译不过），
    // 而且两处文案/元素 id 迟早写歪。
    let mut cells: Vec<(&'static str, AnyElement)> = Vec::new();
    if props.solid_supported {
        let on_solid = on.clone();
        cells.push((
            "compress-solid",
            crate::ui::widgets::checkbox("compress-solid", crate::tr("field.solid"))
                .checked(props.solid)
                .on_click(move |v: &bool, _, _| on_solid(DialogEvent::CompressSolid(*v)))
                .into_any_element(),
        ));
    }
    {
        let on_hidden = on.clone();
        cells.push((
            "compress-exclude-hidden",
            crate::ui::widgets::checkbox(
                "compress-exclude-hidden",
                crate::tr("field.exclude_hidden"),
            )
            .checked(props.exclude_hidden)
            .on_click(move |v: &bool, _, _| on_hidden(DialogEvent::CompressExcludeHidden(*v)))
            .into_any_element(),
        ));
    }
    {
        let on_symlink = on.clone();
        cells.push((
            "compress-follow-symlinks",
            crate::ui::widgets::checkbox(
                "compress-follow-symlinks",
                crate::tr("field.follow_symlinks"),
            )
            .checked(props.follow_symlinks)
            .on_click(move |v: &bool, _, _| on_symlink(DialogEvent::CompressFollowSymlinks(*v)))
            .into_any_element(),
        ));
    }
    {
        let on_meta = on.clone();
        cells.push((
            "compress-keep-metadata",
            crate::ui::widgets::checkbox(
                "compress-keep-metadata",
                crate::tr("field.keep_metadata"),
            )
            .checked(props.keep_metadata)
            .on_click(move |v: &bool, _, _| on_meta(DialogEvent::CompressKeepMetadata(*v)))
            .into_any_element(),
        ));
    }
    {
        let on_verify = on.clone();
        cells.push((
            "compress-verify-after",
            crate::ui::widgets::checkbox("compress-verify-after", crate::tr("field.verify_after"))
                .checked(props.verify_after)
                .on_click(move |v: &bool, _, _| on_verify(DialogEvent::CompressVerifyAfter(*v)))
                .into_any_element(),
        ));
    }
    {
        let on_best = on.clone();
        cells.push((
            "compress-best-effort",
            crate::ui::widgets::checkbox("compress-best-effort", crate::tr("field.best_effort"))
                .checked(props.best_effort)
                .on_click(move |v: &bool, _, _| on_best(DialogEvent::CompressBestEffort(*v)))
                .into_any_element(),
        ));
    }
    let mut rows: Vec<AnyElement> = Vec::new();
    while !cells.is_empty() {
        let take = cells.len().min(2);
        let row: Vec<(&'static str, AnyElement)> = cells.drain(..take).collect();
        rows.push(switch_row(row).into_any_element());
    }
    let attrs = v_flex().w_full().gap(sp(2.)).children(rows).when(
        props.solid_supported && !props.solid_hint.is_empty(),
        |col| col.child(hint_line(cx, HintTone::Muted, &props.solid_hint)),
    );
    adv = adv.child(FormField::new().label("").child(attrs));
    adv = adv.child(FormField::new().label("").child(hint_line(
        cx,
        HintTone::Muted,
        &crate::tr("dlg.compress.best_effort_hint"),
    )));

    // ── 加密折叠区内容 ──
    let on_names = on.clone();
    let mut enc = dialog_form()
        .child(
            FormField::new().label(crate::tr("field.password")).child(
                Input::new(&props.password_input)
                    .with_size(component_size())
                    .mask_toggle(),
            ),
        )
        .child(
            FormField::new()
                .label(crate::tr("field.password_confirm"))
                .child(field_col(
                    cx,
                    Some(&props.password_error)
                        .filter(|e| !e.is_empty())
                        .map(|e| e.as_str()),
                    HintTone::Danger,
                    Input::new(&props.password_confirm_input)
                        .with_size(component_size())
                        .mask_toggle(),
                )),
        );
    if props.enc_supported {
        enc = enc.child(
            FormField::new()
                .label(crate::tr("field.encryption_method"))
                .child(
                    Select::new(&props.enc_select)
                        .with_size(component_size())
                        .placeholder(crate::tr("field.encryption_method")),
                ),
        );
    } else {
        enc = enc.child(
            FormField::new()
                .label(crate::tr("field.encryption_method"))
                .child(
                    Select::new(&props.enc_select)
                        .with_size(component_size())
                        .disabled(true),
                ),
        );
    }
    if props.enc_names_supported {
        enc = enc.child(
            FormField::new().label("").child(
                crate::ui::widgets::checkbox(
                    "compress-encrypt-names",
                    crate::tr("field.encrypt_names"),
                )
                .checked(props.enc_names)
                .on_click(move |v: &bool, _, _| on_names(DialogEvent::CompressEncryptNames(*v))),
            ),
        );
    }
    enc = enc.child(FormField::new().label("").child(hint_line(
        cx,
        HintTone::Warn,
        &props.enc_hint,
    )));

    // ── 折叠区：展开态由 props 持有 ──
    //
    // **不用 gpui-kit `Accordion`**（09 §9 组件映射表的已登记偏差）：其
    // `AccordionItem::on_toggle_click` 是 crate 私有（accordion.rs:250），外部只能
    // 拿 `Accordion::on_toggle_click(&[usize])`；而 `AccordionItem::open` 是**纯
    // prop**（无内部态、无 `use_keyed_state`，见 accordion.rs:120/292），折叠态
    // 必须由调用方驱动，切换回调又依赖 gpui 的子→父事件冒泡顺序——该顺序无法在
    // 本仓库离线验证。故此处自绘**可点击标题行**：事件 → props → 重渲染闭环完全
    // 可控，与本对话框其余控件同款模式。
    let on_adv = on.clone();
    let on_enc = on.clone();
    let advanced_section = v_flex()
        .w_full()
        .child(collapsible_header(
            cx,
            "compress-sec-advanced",
            crate::tr("dlg.compress.sec_advanced"),
            props.advanced_summary.clone(),
            props.advanced_open,
            move |v| on_adv(DialogEvent::CompressToggleAdvanced(v)),
        ))
        .when(props.advanced_open, |col| col.child(adv));
    // 加密折叠区：格式不支持加密（tar/gz/bz2/xz/zst）**整区不渲染**；格式支持但
    // 写侧未实现（zip/7z，T45/T46）同样不渲染——露出一个选不动的加密区只会被
    // 当成缺陷（用户明确要求「不能选就隐藏」）。
    let encryption_section = v_flex().w_full().when(props.enc_writable, |col| {
        col.child(collapsible_header(
            cx,
            "compress-sec-encryption",
            crate::tr("dlg.compress.sec_encryption"),
            props.enc_summary.clone(),
            props.enc_open,
            move |v| on_enc(DialogEvent::CompressToggleEncryption(v)),
        ))
        .when(props.enc_open, |col| col.child(enc))
    });

    // ── 正文（可滚区；非表单内容与 Form 块并列——Form 不是容器，用 v_flex 汇总） ──
    let body = v_flex()
        .w_full()
        .gap(sp(3.))
        .child(section_header(cx, crate::tr("dlg.compress.sec_basic")))
        .child(basic)
        .child(advanced_section)
        .child(encryption_section)
        .child(summary_card(
            cx,
            crate::tr("dlg.compress.sec_summary"),
            SharedString::from(props.preview_text.clone()),
        ));

    // 头部副标题 = 对象 + 统计：滚正文时对象始终在视野内（旧版是正文首卡，
    // 高级区展开后要往回滚才看得到自己压的是什么）。
    let subtitle = if props.summary_meta.is_empty() {
        props.summary.clone()
    } else {
        format!("{} · {}", props.summary, props.summary_meta)
    };
    panel_shell(cx, crate::tr("dlg.compress.title"), subtitle, footer, body)
}

/// 可折叠分区的标题行（点击切换；右侧显示就地摘要）。
///
/// 自绘理由见渲染函数内注释（kit `AccordionItem` 的切换回调不可从外部接线）。
fn collapsible_header(
    cx: &App,
    id: &'static str,
    label: String,
    summary: String,
    open: bool,
    on_toggle: impl Fn(bool) + 'static,
) -> impl IntoElement {
    h_flex()
        .id(id)
        .w_full()
        .items_center()
        .gap(sp(2.))
        .py(sp(2.))
        .border_t_1()
        .border_color(cx.theme().border)
        .cursor_pointer()
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(muted_fg(cx))
                .child(if open { "▾" } else { "▸" }),
        )
        .child(
            div()
                .flex_none()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(label)),
        )
        .child(div().flex_1())
        .child(
            div()
                .text_xs()
                .text_color(muted_fg(cx))
                .child(SharedString::from(summary)),
        )
        .on_click(move |_, _, _| on_toggle(!open))
}

/// 覆盖确认对话框（目标已存在时由参数窗 `open_dialog` 呈现）。
///
/// 用户要求：已存在必须**弹 Dialog 确认**，不能只在面板里给一行提示。
/// 两个出口都走 `DialogEvent`（「覆盖」→ `CompressOverwriteAccept`；「取消」/✕/Esc/
/// 遮罩 → `CompressOverwriteCancel`），参数窗侧只做「提交 / 不提交」。
pub fn compress_overwrite_dialog(
    cx: &mut App,
    target: &str,
    on: impl Fn(DialogEvent) + 'static,
) -> Dialog {
    let on = std::rc::Rc::new(on);
    let on_cancel_btn = on.clone();
    let on_accept = on.clone();
    let on_dismiss = on.clone();
    let footer = DialogFooter::new()
        .child(
            Button::new("compress-overwrite-cancel")
                .with_size(component_size())
                .label(crate::tr("dlg.cancel"))
                .on_click(move |_, _, _| on_cancel_btn(DialogEvent::CompressOverwriteCancel)),
        )
        .child(
            // 测试驱动锚点：`cfg!(test)` 下包一层可查边界的 div（同
            // `switch_row`/settings_render 的做法）——`Button::new(id).with_size(component_size())` 的 id
            // 只是 keyed state 键，不注册为可观测元素，测试无法按 id 定位。
            div()
                .debug_selector(|| "compress-overwrite-accept".into())
                .child(
                    Button::new("compress-overwrite-accept")
                        .with_size(component_size())
                        .primary()
                        .label(crate::tr("dlg.overwrite.accept"))
                        .on_click(move |_, _, _| on_accept(DialogEvent::CompressOverwriteAccept)),
                ),
        );
    dialog_shell(
        cx,
        DIALOG_W,
        crate::tr("dlg.overwrite.title"),
        footer,
        v_flex()
            .gap(sp(1.))
            .child(
                div()
                    .text_sm()
                    .child(crate::tr_with("dlg.overwrite.prompt", &[("path", target)])),
            )
            .child(hint_line(
                cx,
                HintTone::Muted,
                &crate::tr("dlg.overwrite.hint"),
            )),
    )
    // ✕/Esc/遮罩 = 取消（返回 true 关层）：不提交、不动参数
    .on_cancel(move |_, _, _| {
        on_dismiss(DialogEvent::CompressOverwriteCancel);
        true
    })
}

/// 级别实参文案（供装配层复用，避免装配侧与对话框重复实现）。
pub fn compress_level_detail_text(d: &crate::viewmodel::dialog_vm::LevelDetail) -> String {
    level_detail_text(d)
}

/// 级别速度档文案（供装配层复用）。
pub fn compress_level_speed_text(level: hzip_core::Level) -> String {
    crate::tr(level_speed_key(level))
}
