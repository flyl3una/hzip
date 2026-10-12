//! 编辑提交确认对话框（T-MENU6/7/11）：添加文件 / 删除文件 / 同名冲突。
//!
//! 与压缩覆盖确认（[`super::compress::compress_overwrite_dialog`]）同款形态：
//! `Dialog` 壳 + footer 按钮，出口全是 `DialogEvent`（不做参数校验/不构建
//! `ChangeSet`——那是装配层的事，与 T-G11 §3.4 一致）。
//!
//! 三种语义各一个构造函数：
//!
//! | 函数 | 出口事件 |
//! |---|---|
//! | [`add_files_confirm_dialog`] | `EditAddAccept { replace }` / `EditAddCancel` |
//! | [`delete_files_confirm_dialog`] | `EditDeleteAccept` / `EditDeleteCancel` |
//! | [`name_conflict_dialog`] | `EditConflictReplace` / `EditConflictSkip` / `EditConflictCancel` |

use crate::ui::constant::{DIALOG_W, DIALOG_W_MID, DIALOG_W_WIDE};
use crate::ui::spacing::{component_size, sp};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Dialog, DialogFooter};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, SharedString, div};

use super::HintTone;
use super::dialog_shell;
use super::dialog_shell_untitled;
use super::hint_line;
use crate::ui::contracts::DialogEvent;

/// 添加文件确认（列条数；可选「同名替换」选择）。
///
/// `conflicts > 0` 时多给一个「替换同名」出口（用户选了同名条目时）。
pub fn add_files_confirm_dialog(
    cx: &mut App,
    count: usize,
    conflicts: usize,
    on: impl Fn(DialogEvent) + 'static,
) -> Dialog {
    let on = std::rc::Rc::new(on);
    let on_cancel_btn = on.clone();
    let on_accept = on.clone();
    let on_replace = on.clone();
    let on_dismiss = on.clone();
    let has_conflict = conflicts > 0;

    let mut footer = DialogFooter::new().child(
        Button::new("edit-add-cancel")
            .with_size(component_size())
            .label(crate::tr("dlg.cancel"))
            .on_click(move |_, _, _| on_cancel_btn(DialogEvent::EditAddCancel)),
    );
    if has_conflict {
        footer = footer.child(
            Button::new("edit-add-replace")
                .with_size(component_size())
                .label(crate::tr("dlg.edit.replace_existing"))
                // 同「添加」：点击现场先关层再上行提交事件
                .on_click(move |_, window, cx| {
                    use gpui_kit::component::WindowExt as _;
                    window.close_dialog(cx);
                    on_replace(DialogEvent::EditAddAccept { replace: true });
                }),
        );
    }
    let footer = footer.child(
        Button::new("edit-add-accept")
            .with_size(component_size())
            .primary()
            .label(crate::tr("dlg.edit.add_confirm"))
            // 「添加」点击现场先关确认提示，再由 OpenProgress 开输出窗——
            // 否则提示框悬浮在输出窗之上（2026-09-15 用户报障）。
            .on_click(move |_, window, cx| {
                use gpui_kit::component::WindowExt as _;
                window.close_dialog(cx);
                on_accept(DialogEvent::EditAddAccept { replace: false });
            }),
    );

    let mut body = v_flex()
        .gap(sp(1.))
        .child(div().text_sm().child(SharedString::from(crate::tr_with(
            "dlg.edit.add_prompt",
            &[("count", &count.to_string())],
        ))));
    if has_conflict {
        body = body.child(hint_line(
            cx,
            HintTone::Warn,
            &crate::tr_with(
                "dlg.edit.conflict_prompt",
                &[("count", &conflicts.to_string())],
            ),
        ));
    }

    dialog_shell(cx, DIALOG_W, crate::tr("dlg.edit.add_title"), footer, body).on_cancel(
        move |_, _, _| {
            on_dismiss(DialogEvent::EditAddCancel);
            true
        },
    )
}

/// 删除确认（防误删；列条数）。
pub fn delete_files_confirm_dialog(
    cx: &mut App,
    count: usize,
    on: impl Fn(DialogEvent) + 'static,
) -> Dialog {
    let on = std::rc::Rc::new(on);
    let on_cancel_btn = on.clone();
    let on_accept = on.clone();
    let on_dismiss = on.clone();
    let footer = DialogFooter::new()
        .child(
            Button::new("edit-del-cancel")
                .with_size(component_size())
                .label(crate::tr("dlg.cancel"))
                .on_click(move |_, _, _| on_cancel_btn(DialogEvent::EditDeleteCancel)),
        )
        .child(
            Button::new("edit-del-accept")
                .with_size(component_size())
                .danger()
                .label(crate::tr("dlg.edit.delete_confirm"))
                // 同添加确认：点击现场先关层，再上行提交事件（OpenProgress
                // 随后开输出窗）。
                .on_click(move |_, window, cx| {
                    use gpui_kit::component::WindowExt as _;
                    window.close_dialog(cx);
                    on_accept(DialogEvent::EditDeleteAccept);
                }),
        );
    // 本框既有视觉契约 = **无标题行**（标题信息在正文里），故用无标题骨架
    dialog_shell_untitled(
        cx,
        DIALOG_W,
        footer,
        v_flex()
            .gap(sp(1.))
            .child(div().text_sm().child(SharedString::from(crate::tr_with(
                "dlg.edit.delete_prompt",
                &[("count", &count.to_string())],
            )))),
    )
    .on_cancel(move |_, _, _| {
        on_dismiss(DialogEvent::EditDeleteCancel);
        true
    })
}

/// 同名条目冲突（追加时目标名已存在：替换 / 跳过 / 取消）。
pub fn name_conflict_dialog(
    cx: &mut App,
    conflicts: usize,
    on: impl Fn(DialogEvent) + 'static,
) -> Dialog {
    let on = std::rc::Rc::new(on);
    let on_cancel_btn = on.clone();
    let on_replace = on.clone();
    let on_skip = on.clone();
    let on_dismiss = on.clone();
    let footer = DialogFooter::new()
        .child(
            Button::new("edit-conflict-cancel")
                .with_size(component_size())
                .label(crate::tr("dlg.cancel"))
                .on_click(move |_, _, _| on_cancel_btn(DialogEvent::EditConflictCancel)),
        )
        .child(
            Button::new("edit-conflict-skip")
                .with_size(component_size())
                .label(crate::tr("dlg.edit.conflict_skip"))
                .on_click(move |_, _, _| on_skip(DialogEvent::EditConflictSkip)),
        )
        .child(
            Button::new("edit-conflict-replace")
                .with_size(component_size())
                .primary()
                .label(crate::tr("dlg.edit.conflict_replace"))
                .on_click(move |_, _, _| on_replace(DialogEvent::EditConflictReplace)),
        );
    dialog_shell(
        cx,
        DIALOG_W_MID,
        crate::tr("dlg.edit.conflict_title"),
        footer,
        v_flex()
            .gap(sp(1.))
            .child(div().text_sm().child(SharedString::from(crate::tr_with(
                "dlg.edit.conflict_body",
                &[("count", &conflicts.to_string())],
            )))),
    )
    .on_cancel(move |_, _, _| {
        on_dismiss(DialogEvent::EditConflictCancel);
        true
    })
}

/// 编辑失败/信息提示对话框（一行文案 + 关闭；T-MENU6~8 错误出口）。
pub fn edit_error_dialog(cx: &mut App, message: &str) -> Dialog {
    let body = v_flex().gap(sp(1.)).child(
        div()
            .text_sm()
            .child(SharedString::from(message.to_string())),
    );
    let _ = cx.theme().danger;
    let _ = h_flex();
    dialog_shell(
        cx,
        DIALOG_W_WIDE,
        crate::tr("dlg.edit.failed"),
        DialogFooter::new(),
        body,
    )
}
