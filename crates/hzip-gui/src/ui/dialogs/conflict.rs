//! 冲突对话框（T-GUI T10 自 dialogs.rs 拆分；纯移动不改写）。

use crate::ui::constant::DIALOG_W_WIDE;
use crate::ui::spacing::{component_size, sp};
use gpui_kit::App;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Dialog, DialogFooter};
use gpui_kit::component::form::Field as FormField;
use gpui_kit::component::h_flex;
use gpui_kit::prelude::*;

use super::{dialog_form, dialog_shell, read_only_field};
use crate::ui::contracts::{ConflictDecision, DialogEvent};

/// 冲突对话框 props（四决策 + 全部应用）。
#[derive(Debug, Clone, Default)]
pub struct ConflictDialogProps {
    /// 目标路径（wrap 展示）
    pub path: String,
    /// 现有文件简要
    pub existing_brief: String,
    /// 档案条目简要
    pub archive_brief: String,
}

/// 冲突对话框渲染（四决策 + apply-all 复选——决策语义归 [`ConflictDecision`]，
/// 全部应用语义由第二参数携带；决策缓存归 T-G30/T41）。
pub fn conflict_dialog(
    cx: &mut App,
    props: &ConflictDialogProps,
    on: impl Fn(DialogEvent) + 'static,
) -> Dialog {
    let on = std::rc::Rc::new(on);
    let on_overwrite = on.clone();
    let on_skip = on.clone();
    let on_rename = on.clone();
    let on_abort = on.clone();
    // 「应用到全部」（T-GUI T32 接线）：复选态存共享 Cell——勾选回调写，
    // 四决策按钮点击时读出并以第二参数上抛（不缓存决策——缓存归 T-G30/T41）。
    let apply_all = std::rc::Rc::new(std::cell::Cell::new(false));
    let on_check_all = apply_all.clone();
    let all_abort = apply_all.clone();
    let all_overwrite = apply_all.clone();
    let all_skip = apply_all.clone();
    let all_rename = apply_all;
    let footer = DialogFooter::new()
        .child(
            h_flex()
                .mr_auto()
                .gap(sp(2.))
                .items_center()
                .child(
                    crate::ui::widgets::checkbox(
                        "conflict-apply-all",
                        crate::tr("dlg.conflict.apply_all"),
                    )
                    .on_click(move |checked: &bool, _, _| {
                        on_check_all.set(*checked);
                    }),
                )
                .child(
                    Button::new("conflict-abort")
                        .with_size(component_size())
                        .label(crate::tr("dlg.conflict.abort"))
                        .on_click(move |_, _, _| {
                            on_abort(DialogEvent::ConflictAnswer(
                                ConflictDecision::Abort,
                                all_abort.get(),
                            ))
                        }),
                ),
        )
        .child(
            Button::new("conflict-overwrite")
                .with_size(component_size())
                .label(crate::tr("dlg.conflict.overwrite"))
                .on_click(move |_, _, _| {
                    on_overwrite(DialogEvent::ConflictAnswer(
                        ConflictDecision::Overwrite,
                        all_overwrite.get(),
                    ))
                }),
        )
        .child(
            Button::new("conflict-skip")
                .with_size(component_size())
                .label(crate::tr("dlg.conflict.skip"))
                .on_click(move |_, _, _| {
                    on_skip(DialogEvent::ConflictAnswer(
                        ConflictDecision::Skip,
                        all_skip.get(),
                    ))
                }),
        )
        .child(
            Button::new("conflict-rename")
                .with_size(component_size())
                .primary()
                .label(crate::tr("dlg.conflict.rename_auto"))
                .on_click(move |_, _, _| {
                    on_rename(DialogEvent::ConflictAnswer(
                        ConflictDecision::RenameAuto,
                        all_rename.get(),
                    ))
                }),
        );
    let body = dialog_form()
        .child(
            FormField::new()
                .label(crate::tr("field.target"))
                .child(read_only_field(cx, &props.path)),
        )
        .child(
            FormField::new()
                .label(crate::tr("field.existing_file"))
                .child(read_only_field(cx, &props.existing_brief)),
        )
        .child(
            FormField::new()
                .label(crate::tr("field.archive_entry"))
                .child(read_only_field(cx, &props.archive_brief)),
        );
    dialog_shell(
        cx,
        DIALOG_W_WIDE,
        crate::tr("dlg.conflict.title"),
        footer,
        body,
    )
}
