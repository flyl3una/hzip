//! 解压参数面板（T-GUI T10 自 dialogs.rs 拆分；T-GUI T24 冲突策略/文件名编码
//! 只读框 → Select，与压缩面板同款 SelectState 预构建通道）。
//!
//! 承载形态：**窗口级面板**（头部固定 / 正文滚动 / 底部固定），不是 `Dialog`
//! 弹层——理由见 `super::panel_shell`。

use crate::ui::spacing::{component_size, sp};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::Field as FormField;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, SharedString, div};

use super::{dialog_form, error_line, panel_shell};
use crate::ui::contracts::DialogEvent;

/// 解压参数面板 props（字段 = ExtractDialogVm 投影 + 修复预览对）。
///
/// 实体由调用方在参数窗 builder 内预构建（窗口亲和，同压缩面板偏差登记）；
/// 冲突策略/编码 Select 的 Confirm 事件由装配方经
/// [`select_string_state`] 订阅转 `DialogEvent::ExtractConflict/Codepage`。
#[derive(Clone)]
pub struct ExtractPanelProps {
    /// 解压对象摘要（demo.zip（12,408 条目 · 已选 2））
    pub summary: String,
    /// 目标目录输入态（预构建）
    pub dest_input: Entity<InputState>,
    /// 解压密码输入态（掩码；预构建）。可选：填了就是 core 的**首个候选**，
    /// 成功即零弹窗；未填/不对回落运行时询问链。
    pub password_input: Entity<InputState>,
    /// 智能解压开关
    pub smart: bool,
    /// 冲突策略 Select 实体（预构建；T-GUI T24 替代只读框）
    pub conflict_select: Entity<SelectState<SearchableVec<String>>>,
    /// 文件名编码 Select 实体（预构建）
    pub codepage_select: Entity<SelectState<SearchableVec<String>>>,
    /// 尽力而为开关
    pub best_effort: bool,
    /// 编码修复预览对（原始 → 修复；warn 色）
    pub repaired: Vec<(String, String)>,
    /// 错误文案
    pub error: String,
}

/// 解压参数面板渲染（窗口内容视图）。
///
/// 布局/事件映射（06 §4.3）：头部 = 档案摘要；正文 = 目标目录 Input+浏览、
/// 智能解压 Switch、冲突策略 Select、文件名编码 Select、尽力而为 Switch、
/// 修复预览对照列表；底部 = 错误行 + 开始/取消。
pub fn extract_panel(
    cx: &mut App,
    props: &ExtractPanelProps,
    on: impl Fn(DialogEvent) + 'static,
) -> impl IntoElement {
    let on = std::rc::Rc::new(on);
    let on_browse = on.clone();
    let on_smart = on.clone();
    let on_best = on.clone();
    let on_start = on.clone();
    let on_cancel = on.clone();
    let footer = v_flex()
        .w_full()
        .gap(sp(2.))
        .child(error_line(cx, &props.error))
        .child(
            h_flex()
                .w_full()
                .justify_end()
                .gap(sp(2.))
                .child(
                    Button::new("extract-cancel")
                        .with_size(component_size())
                        .label(crate::tr("dlg.cancel"))
                        .on_click(move |_, _, _| on_cancel(DialogEvent::ExtractCancel)),
                )
                .child(
                    Button::new("extract-start")
                        .with_size(component_size())
                        .primary()
                        .label(crate::tr("dlg.extract.start"))
                        .on_click(move |_, _, _| on_start(DialogEvent::ExtractStart)),
                ),
        );
    let body = dialog_form()
        .child(
            FormField::new().label(crate::tr("field.dest_dir")).child(
                h_flex()
                    .w_full()
                    .flex_1()
                    .gap(sp(1.))
                    .child(
                        Input::new(&props.dest_input)
                            .with_size(component_size())
                            .flex_1(),
                    )
                    .child(
                        Button::new("extract-browse")
                            .with_size(component_size())
                            .label(crate::tr("dlg.browse"))
                            .on_click(move |_, _, _| on_browse(DialogEvent::ExtractBrowse)),
                    ),
            ),
        )
        .child(
            FormField::new().label(crate::tr("field.password")).child(
                Input::new(&props.password_input)
                    .with_size(component_size())
                    .mask_toggle(),
            ),
        )
        .child(
            FormField::new()
                .label(crate::tr("field.smart_extract"))
                .child(
                    // 可观测 selector：kit `Switch` 的 id 只作 keyed state 键，
                    // 不注册为可观测元素——测试要能按 id 点到它（见 op_window 测试）
                    div().debug_selector(|| "extract-smart".to_string()).child(
                        Switch::new("extract-smart")
                            .with_size(component_size())
                            .checked(props.smart)
                            .label(crate::tr("dlg.extract.smart_hint"))
                            .on_click(move |checked: &bool, _, _| {
                                on_smart(DialogEvent::ExtractSmart(*checked))
                            }),
                    ),
                ),
        )
        .child(
            // 冲突策略/文件名编码（T-GUI T24：只读框 → Select 实体；
            // Confirm 事件由装配方订阅——见 props 文档）
            FormField::new()
                .label(crate::tr("field.conflict_strategy"))
                .child(
                    Select::new(&props.conflict_select)
                        .with_size(component_size())
                        .placeholder(crate::tr("field.conflict_strategy")),
                ),
        )
        .child(
            FormField::new()
                .label(crate::tr("field.filename_encoding"))
                .child(
                    Select::new(&props.codepage_select)
                        .with_size(component_size())
                        .placeholder(crate::tr("field.filename_encoding")),
                ),
        )
        .child(
            FormField::new()
                .label(crate::tr("field.best_effort"))
                .child(
                    div()
                        .debug_selector(|| "extract-best-effort".to_string())
                        .child(
                            Switch::new("extract-best-effort")
                                .with_size(component_size())
                                .checked(props.best_effort)
                                .label(crate::tr("dlg.extract.best_effort_hint"))
                                .on_click(move |checked: &bool, _, _| {
                                    on_best(DialogEvent::ExtractBestEffort(*checked))
                                }),
                        ),
                ),
        )
        .child(
            FormField::new()
                .label(crate::tr("field.repaired_preview"))
                .child(
                    v_flex()
                        .flex_1()
                        .gap(sp(1.))
                        .children(props.repaired.iter().map(|(raw, fixed)| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(SharedString::from(format!("{raw} → {fixed}")))
                        })),
                ),
        );
    let _ = on_start; // 事件闭包已在 footer 内消费
    panel_shell(
        cx,
        crate::tr("dlg.extract.title"),
        props.summary.clone(),
        footer,
        body,
    )
}
