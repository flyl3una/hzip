//! 密码对话框（T-GUI T10 自 dialogs.rs 拆分；纯移动不改写）。

use crate::ui::constant::DIALOG_W_NARROW;
use crate::ui::spacing::component_size;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Dialog, DialogFooter};
use gpui_kit::component::form::Field as FormField;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{App, Entity};

use super::{dialog_form, dialog_shell, error_line};
use crate::ui::contracts::DialogEvent;

/// 密码对话框 props。
///
/// **只有一个输入框**（用户实测反馈 2026-09-23）：目标/尝试不再渲染成字段框，
/// 只保留「密码」输入 + 一行说明（重试次数并入说明行）。
/// 密码输入态由 T-G30 预构建（masked 密文模式）；提交即清空语义在
/// [`password_dialog`] 内实现（02 §1.2）。
#[derive(Clone)]
pub struct PasswordDialogProps {
    /// 重试说明行（含第 n/max 次；空 = 不渲染）
    pub last_error: String,
    /// 密码输入态（预构建；masked 密文模式）
    pub password_input: Entity<InputState>,
}

/// 密码对话框渲染（提交即清空——`PasswordSubmit(text)` 发出后立即清空内部态
/// + `cx.notify()`；组件不持有密码副本）。
pub fn password_dialog(
    cx: &mut App,
    props: &PasswordDialogProps,
    on: impl Fn(DialogEvent) + 'static,
) -> Dialog {
    let on = std::rc::Rc::new(on);
    let on_cancel = on.clone();
    // 提交：读 InputState 值 → 上抛 PasswordSubmit → 清空内部态 + notify
    //（Zeroizing 构造归 T-G30 绑定层，本卡只负责 UI 侧清零）。
    let on_submit_state = props.password_input.clone();
    let on_submit = on.clone();
    let footer = DialogFooter::new()
        .child(
            Button::new("password-cancel")
                .with_size(component_size())
                .label(crate::tr("dlg.cancel"))
                .on_click(move |_, _, _| on_cancel(DialogEvent::PasswordCancel)),
        )
        .child(
            Button::new("password-submit")
                .with_size(component_size())
                .primary()
                .label(crate::tr("dlg.confirm"))
                .on_click(move |_, window, cx| {
                    let text = on_submit_state.read(cx).value().to_string();
                    on_submit(DialogEvent::PasswordSubmit(text));
                    // 提交即清空（02 §1.2）：UI 侧立即清零 + 刷新。
                    on_submit_state.update(cx, |s, cx| {
                        s.set_value("", window, cx);
                    });
                }),
        );
    let body = dialog_form()
        .child(
            FormField::new().label(crate::tr("field.password")).child(
                Input::new(&props.password_input)
                    .with_size(component_size())
                    .mask_toggle(),
            ),
        )
        .child(FormField::new().child(error_line(cx, &props.last_error)));
    dialog_shell(
        cx,
        DIALOG_W_NARROW,
        crate::tr("dlg.password.title"),
        footer,
        body,
    )
}

#[cfg(test)]
#[path = "tests/password.rs"]
mod tests;
