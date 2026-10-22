//! 请求-应答提示窗（密码 / 冲突）——**独立 OS 窗口**（2026-09-23 用户实测返工）。
//!
//! # 为什么不是主窗内 Dialog
//!
//! 旧形态用 `window.open_dialog` 把提示长在「宿主窗口」（主窗优先，直启模式退化为
//! 参数窗/进度窗）上，由此出过三个用户可见缺陷：
//!
//! 1. **层级**：压缩/解压进度窗是独立 OS 窗口，天然压在宿主窗内的弹层之上 →
//!    冲突提示看不见；
//! 2. **直启模式无宿主**：右键「智能解压」等 argv 意图不建主窗，宿主解析失败时
//!    密码请求无人应答（120 s 决策超时 → 显示「不支持的操作」）；
//! 3. **布局/交互**：窄 Modal 一行塞 4 按钮 + 复选框 → 溢出；复选框状态存在
//!    **每帧重建**的 `Cell` 里 → 永远勾不上（受控件必须由持久状态驱动）。
//!
//! 本模块把它做成与参数窗/进度窗同族的独立窗口：宽度固定（按钮不溢出）、
//! 复选框状态是**实体字段**（点击 `cx.notify()` 重绘，勾得住）、层级在进度窗之上、
//! 任何模式（含无主窗的直启模式）都有宿主。
//!
//! # 生命周期
//!
//! 主线程泵每批信号后调 [`sync`]：`Some(prompt)` → 无窗则开、有窗则就地更新
//! （身份变化重置交互态）；`None` → 关窗。应答经注入的 `on_event` 回投
//! （`shell::prompt::answer_prompt`），router 收口后下一批信号即 `None` → 自动关窗。

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::Field as FormField;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, Context, Entity, IntoElement, Render, SharedString, Window, div, size,
};

use crate::ui::constant::{PROMPT_WINDOW_H, PROMPT_WINDOW_W};
use crate::ui::contracts::{ConflictDecision, DialogEvent, UiDialogPrompt};
use crate::ui::dialogs::{dialog_form, error_line, muted_fg, read_only_field};
use crate::ui::spacing::{component_size, sp};

/// 提示窗数据面（密码 or 冲突；一次只有一个——router 单槽）。
enum PromptKind {
    Password {
        /// 重试说明（含第 n/max 次；空 = 不渲染）
        error: String,
    },
    Conflict {
        path: String,
        existing_brief: String,
        archive_brief: String,
    },
}

/// 提示窗实体（复选框状态是真字段——受控件靠它勾选，见模块文档）。
pub struct PromptWindow {
    kind: PromptKind,
    /// 「应用到全部」勾选态（冲突专用）
    apply_all: bool,
    /// 密码输入态（密码专用；每窗建一次，输入不丢）
    password_input: Option<Entity<InputState>>,
    /// 密码框回车订阅（用户实测 2026-10-19：密码框回车没反应，只能点按钮）
    password_enter_sub: Option<gpui_kit::Subscription>,
    /// 应答回调（shell 侧注入：`answer_prompt` + 信号回投）
    on_event: Rc<dyn Fn(DialogEvent)>,
}

/// 已开提示窗（单槽：router 同时只有一个待决提示）。
#[derive(Default)]
struct PromptWindowGlobal {
    handle: Option<AnyWindowHandle>,
    entity: Option<Entity<PromptWindow>>,
    identity: Option<String>,
    /// 提示种类（用户直接 ✕ 关窗时按种类回一个「取消」，见 [`sync`]）
    kind: Option<PromptKindTag>,
}

/// 提示种类标记（关窗回取消用）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum PromptKindTag {
    Password,
    Conflict,
}

impl gpui_kit::Global for PromptWindowGlobal {}

/// 提示种类标记。
fn tag_of(prompt: &UiDialogPrompt) -> PromptKindTag {
    match prompt {
        UiDialogPrompt::Password { .. } => PromptKindTag::Password,
        _ => PromptKindTag::Conflict,
    }
}

fn alive(cx: &App, handle: AnyWindowHandle) -> bool {
    cx.windows().contains(&handle)
}

/// 同步提示窗到当前待决提示（`None` = 关闭）。
///
/// 主线程调用（窗口创建/关闭是 gpui 主线程亲和）。
pub fn sync(cx: &mut App, prompt: Option<&UiDialogPrompt>, on_event: Rc<dyn Fn(DialogEvent)>) {
    if !cx.has_global::<PromptWindowGlobal>() {
        cx.set_global(PromptWindowGlobal::default());
    }
    let identity = prompt.map(|p| p.identity());
    let (live, entity, shown, shown_kind) = {
        let g = cx.global::<PromptWindowGlobal>();
        (
            g.handle.map(|h| alive(cx, h)).unwrap_or(false),
            g.entity.clone(),
            g.identity.clone(),
            g.kind,
        )
    };
    // 用户直接 ✕ 关窗（不改由 `sync(None)` 驱动的正常收口）：**视为取消应答**。
    //
    // 前提：应答按钮**不再自己关窗**（见密码/冲突按钮注释）——否则本条会把「应答
    // 导致的窗消失」误判为用户关窗，回一个取消去解掉刚放行的密码门（用户实测
    // 2026-09-28：`resolved=false` + 密码已缓存 + 操作不续走）。
    // 否则该请求会一直挂在 router 上，任务干等到 120 s 决策超时才失败——
    // 期间它还占着并发名额，后续任务只能排队（用户实测：压缩「一直在队列中等待」）。
    if !live && shown.is_some() && shown == identity {
        if let Some(kind) = shown_kind {
            match kind {
                PromptKindTag::Password => on_event(DialogEvent::PasswordCancel),
                PromptKindTag::Conflict => {
                    on_event(DialogEvent::ConflictAnswer(ConflictDecision::Abort, false))
                }
            }
        }
        *cx.global_mut::<PromptWindowGlobal>() = PromptWindowGlobal::default();
        return;
    }

    let Some(prompt) = prompt else {
        // 待决消失（已应答/任务终局）→ 关窗并清登记
        if live && let Some(handle) = cx.global::<PromptWindowGlobal>().handle {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        *cx.global_mut::<PromptWindowGlobal>() = PromptWindowGlobal::default();
        return;
    };

    // 已开：就地更新（身份变化重置交互态——换提示时取消勾选/换输入框）
    if live && let Some(entity) = entity {
        let changed = shown != identity;
        entity.update(cx, |w, cx| {
            if changed {
                w.apply_all = false;
                w.password_input = None;
            }
            w.set_prompt(prompt);
            w.on_event = on_event;
            cx.notify();
        });
        cx.global_mut::<PromptWindowGlobal>().identity = identity;
        cx.global_mut::<PromptWindowGlobal>().kind = Some(tag_of(prompt));
        return;
    }

    // 未开：建实体（输入态在窗内首帧建，窗口亲和）→ 开窗 → 登记
    let entity = cx.new(|_| PromptWindow {
        kind: PromptKind::Password {
            error: String::new(),
        },
        apply_all: false,
        password_input: None,
        password_enter_sub: None,
        on_event: on_event.clone(),
    });
    entity.update(cx, |w, _| w.set_prompt(prompt));
    let options = gpui_kit::WindowOptions {
        window_bounds: Some(gpui_kit::WindowBounds::Windowed(
            gpui_kit::Bounds::centered(None, size(PROMPT_WINDOW_W, PROMPT_WINDOW_H), cx),
        )),
        is_resizable: false,
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some(SharedString::from(crate::tr("dlg.prompt.title"))),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        ..gpui_kit::component::TitleBar::window_options()
    };
    let view = entity.clone();
    let opened = cx.open_window(options, move |window, cx| {
        let any: gpui_kit::AnyView = view.into();
        cx.new(|cx| gpui_kit::component::Root::new(any, window, cx))
    });
    match opened {
        Ok(handle) => {
            let handle: AnyWindowHandle = handle.into();
            // 新窗主动激活：右键直启场景新窗默认落后台（用户以为「没反应」）
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            let g = cx.global_mut::<PromptWindowGlobal>();
            g.handle = Some(handle);
            g.entity = Some(entity);
            g.identity = identity;
            g.kind = Some(tag_of(prompt));
        }
        Err(e) => tracing::error!("提示窗创建失败: {e}"),
    }
}

impl PromptWindow {
    /// 写入提示内容（密码 → 说明行；冲突 → 摘要文案）。
    fn set_prompt(&mut self, prompt: &UiDialogPrompt) {
        match prompt {
            UiDialogPrompt::Password { attempts, .. } => {
                self.kind = PromptKind::Password {
                    error: if *attempts > 0 {
                        crate::tr_with(
                            "dlg.password.error_retry",
                            &[
                                (
                                    "n",
                                    &(*attempts)
                                        .min(crate::viewmodel::router::PASSWORD_MAX_RETRIES)
                                        .to_string(),
                                ),
                                (
                                    "max",
                                    &crate::viewmodel::router::PASSWORD_MAX_RETRIES.to_string(),
                                ),
                            ],
                        )
                    } else {
                        String::new()
                    },
                };
            }
            UiDialogPrompt::Conflict {
                path,
                existing_is_dir,
                existing_size,
                ..
            } => {
                self.kind = PromptKind::Conflict {
                    path: path.clone(),
                    existing_brief: crate::tr_with(
                        "dlg.conflict.existing_brief",
                        &[
                            (
                                "kind",
                                &crate::tr(if *existing_is_dir {
                                    "dlg.conflict.kind_dir"
                                } else {
                                    "dlg.conflict.kind_file"
                                }),
                            ),
                            ("size", &crate::ui::binding::fmt_bytes(*existing_size)),
                        ],
                    ),
                    archive_brief: crate::tr("dlg.conflict.archive_brief"),
                };
            }
        }
    }

    /// 保证密码输入框存在（密码态渲染前调用）。
    fn ensure_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(t) = &self.password_input {
            return t.clone();
        }
        let state = crate::ui::dialogs::input_state("", true, window, cx);
        // 回车 = 确认（用户实测 2026-10-19：密码框回车没反应，只能点按钮）
        self.password_enter_sub = Some(cx.subscribe(
            &state,
            |this: &mut Self,
             _input: Entity<InputState>,
             ev: &gpui_kit::component::input::InputEvent,
             cx: &mut Context<Self>| {
                if matches!(
                    ev,
                    gpui_kit::component::input::InputEvent::PressEnter { .. }
                ) {
                    this.submit_password_from_event(cx);
                }
            },
        ));
        self.password_input = Some(state.clone());
        state
    }

    /// 订阅回调拿不到 `Window`：只发提交事件（成功时窗口由 `sync` 关闭；
    /// 失败时输入框内容保留供用户覆盖，与「确定」按钮的清空时机差异可忽略）。
    fn submit_password_from_event(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.password_input.clone() else {
            return;
        };
        let text = input.read(cx).value().to_string();
        (self.on_event)(DialogEvent::PasswordSubmit(text));
    }
}

impl Render for PromptWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, subtitle) = match &self.kind {
            PromptKind::Password { .. } => (
                crate::tr("dlg.password.title"),
                crate::tr("dlg.password.subtitle"),
            ),
            PromptKind::Conflict { .. } => (
                crate::tr("dlg.conflict.title"),
                crate::tr("dlg.conflict.subtitle"),
            ),
        };
        // 输入框先建（`&mut self`），再借用 `kind` 渲染（借用冲突见下）
        let password_input = if matches!(self.kind, PromptKind::Password { .. }) {
            Some(self.ensure_input(window, cx))
        } else {
            None
        };
        let body = match &self.kind {
            PromptKind::Password { error } => {
                let mut form = dialog_form();
                if let Some(input) = password_input.as_ref() {
                    form = form.child(
                        FormField::new()
                            .label(crate::tr("field.password"))
                            .child(Input::new(input).with_size(component_size()).mask_toggle()),
                    );
                }
                form.child(FormField::new().child(error_line(cx, error)))
                    .into_any_element()
            }
            PromptKind::Conflict {
                path,
                existing_brief,
                archive_brief,
            } => dialog_form()
                .child(
                    FormField::new()
                        .label(crate::tr("field.target"))
                        .child(read_only_field(cx, path)),
                )
                .child(
                    FormField::new()
                        .label(crate::tr("field.existing_file"))
                        .child(read_only_field(cx, existing_brief)),
                )
                .child(
                    FormField::new()
                        .label(crate::tr("field.archive_entry"))
                        .child(read_only_field(cx, archive_brief)),
                )
                .into_any_element(),
        };

        // —— 底部：左「应用到全部」（仅冲突）+ 右动作按钮 ——
        let mut footer = h_flex().w_full().gap(sp(2.)).items_center();
        if matches!(self.kind, PromptKind::Conflict { .. }) {
            footer = footer.child(
                div().flex_1().child(
                    crate::ui::widgets::checkbox(
                        "conflict-apply-all",
                        crate::tr("dlg.conflict.apply_all"),
                    )
                    .checked(self.apply_all)
                    .on_click(cx.listener(|w, checked: &bool, _, cx| {
                        // 受控件：状态写实体 + 重绘（旧实现存在每帧重建的
                        // Cell 里 → 永远勾不上，用户实测缺陷）
                        w.apply_all = *checked;
                        cx.notify();
                    })),
                ),
            );
        }
        match &self.kind {
            PromptKind::Password { .. } => {
                let on_cancel = self.on_event.clone();
                footer = footer.child(
                    Button::new("prompt-cancel")
                        .with_size(component_size())
                        .label(crate::tr("dlg.cancel"))
                        .on_click(move |_, _window, _| {
                            // **不要在这里 remove_window**（用户实测 2026-09-28 根因）：
                            // 窗口的死活由 [`sync`] 按 router 待决态收口。自己关窗会让
                            // `sync` 看到「窗没了但待决还在」→ 误判成用户 ✕ 关窗 →
                            // 回 `PasswordCancel` → **把已武装的密码门解掉**，于是
                            // 「输入正确密码后不续走，但密码已缓存，重开档案才好」。
                            on_cancel(DialogEvent::PasswordCancel);
                        }),
                );
                let input = self.password_input.clone();
                let on_submit = self.on_event.clone();
                footer = footer.child(
                    Button::new("prompt-submit")
                        .with_size(component_size())
                        .primary()
                        .label(crate::tr("dlg.confirm"))
                        .on_click(move |_, window, cx| {
                            let Some(input) = input.clone() else { return };
                            let text = input.read(cx).value().to_string();
                            on_submit(DialogEvent::PasswordSubmit(text));
                            // 提交即清空（02 §1.2：UI 侧不留密码副本）
                            input.update(cx, |s, cx| s.set_value("", window, cx));
                            // **不要 remove_window**（同取消按钮注释）：密码错时窗口必须
                            // 留着显示「密码错误（第 n/max 次）」并允许原地重输；密码对时
                            // router 放行 → 下一批信号 `sync(None)` 关窗。
                        }),
                );
            }
            PromptKind::Conflict { .. } => {
                let apply_all = self.apply_all;
                let on_event = self.on_event.clone();
                let btn = |decision: ConflictDecision, label: &'static str, primary: bool| {
                    let on = on_event.clone();
                    let b = Button::new(match decision {
                        ConflictDecision::Abort => "conflict-abort",
                        ConflictDecision::Overwrite => "conflict-overwrite",
                        ConflictDecision::Skip => "conflict-skip",
                        ConflictDecision::RenameAuto => "conflict-rename",
                        _ => "conflict-other",
                    })
                    .with_size(component_size())
                    .label(crate::tr(label))
                    .on_click(move |_, _window, _| {
                        // **不要 remove_window**（同密码按钮注释）：同任务可能有多个冲突
                        // 待决，窗口由 `sync` 按 router 待决态就地更新/收口。
                        on(DialogEvent::ConflictAnswer(decision, apply_all));
                    });
                    if primary { b.primary() } else { b }
                };
                footer = footer
                    .child(btn(ConflictDecision::Abort, "dlg.conflict.abort", false))
                    .child(btn(
                        ConflictDecision::Overwrite,
                        "dlg.conflict.overwrite",
                        false,
                    ))
                    .child(btn(ConflictDecision::Skip, "dlg.conflict.skip", false))
                    .child(btn(
                        ConflictDecision::RenameAuto,
                        "dlg.conflict.rename_auto",
                        true,
                    ));
            }
        }

        let muted = muted_fg(cx);
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                v_flex()
                    .flex_shrink_0()
                    .w_full()
                    .gap(sp(1.))
                    .px(sp(4.))
                    .pt(sp(3.))
                    .pb(sp(2.))
                    .child(div().text_lg().child(SharedString::from(title)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(subtitle)),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .px(sp(4.))
                    // 顶部留白：输入框聚焦高亮环不能被滚动容器上沿裁掉
                    .pt(sp(2.))
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .w_full()
                    .px(sp(4.))
                    .py(sp(3.))
                    .child(footer),
            )
    }
}

/// 弱引用观察者句柄（供测试与装配层保留；窗口自持有实体）。
pub type PromptWindowEntity = Entity<PromptWindow>;

/// 全局槽只读视图（测试用：当前提示窗是否打开）。
pub fn is_open(cx: &App) -> bool {
    cx.has_global::<PromptWindowGlobal>()
        && cx
            .global::<PromptWindowGlobal>()
            .handle
            .map(|h| alive(cx, h))
            .unwrap_or(false)
}

/// 供测试注入的最小应答回调。
pub fn noop_on_event() -> Rc<dyn Fn(DialogEvent)> {
    Rc::new(|_| {})
}

/// 共享句柄容器（装配层与测试共用；避免每处重复写 `Arc<Mutex<…>>`）。
pub type PromptSlot = Arc<Mutex<Option<AnyWindowHandle>>>;

#[cfg(test)]
#[path = "tests/prompt_window.rs"]
mod tests;
