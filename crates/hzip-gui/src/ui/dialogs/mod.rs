//! 五对话框纯 props 组件（T-G11 独占；T-G56 组件对齐 gpui-kit 清零自绘）。
//!
//! T-GUI T10 拆分：`dialogs.rs` 单文件 → 按对话框分文件。
//!
//! | 文件 | 内容 |
//! |---|---|
//! | [`compress`] | 压缩对话框（props + 渲染） |
//! | [`extract`] | 解压对话框（props + 渲染） |
//! | [`password`] | 密码对话框（props + 渲染） |
//! | [`conflict`] | 冲突对话框（props + 渲染） |
//! | `shared` | 共用件（白名单常量 / 提示行 / 字段列 / 骨架 / 表单装配；T-STD 自本文件迁出） |
//! | `fields` | 表单/输入通道构造（Select/Input/NumberInput/Slider 预构建实体；T-STD 自本文件迁出） |
//!
//! 统一形态（T-G11 §3.1）：每对话框 = `fn xxx_dialog(cx: &App, props: &XxxProps,
//! on: &dyn Fn(DialogEvent)) -> AnyElement`。`XxxProps` 从 VM 快照构造
//! （T-G30 负责搬运）。
//!
//! **两种承载形态**（按「谁给它遮罩」区分，别混用）：
//!
//! | 形态 | 用在 | 骨架 | 为什么 |
//! |---|---|---|---|
//! | `panel_shell`（**窗口级**） | 参数窗（[`compress_panel`]/[`extract_panel`]） | 头部固定 / 正文滚动 / 底部固定 | 参数项多且可随格式增减，必须能滚动到底部的「开始」 |
//! | `dialog_shell`（`Dialog` Modal） | 主窗内模态（密码/冲突） | 遮罩 + 标题 + footer | 短暂打断式确认，内容量固定且小 |
//!
//! **参数窗不得走 `Dialog`**（本模块 2026-09 返工根因）：gpui-component 的
//! Dialog 弹层是 `absolute` + **内容高度驱动**（`dialog.rs` 的 popup 只设宽，
//! top = 视口高 1/10，**没有 `max_h`**），正文的 `overflow_y_scrollbar` 只在
//! 容器有界时才滚动。参数一多（高级＋加密展开）弹层就长出视口底部：底部按钮
//! 看不见、正文滚不动。窗口级 `panel_shell` 把滚动区钉在 `flex_1 + min_h_0`
//! 上（同 `views::progress::ProgressWindow`），窗口多小都能滚、底部永远在。
//!
//! **表单对齐**：`Form::horizontal()` + `FormField`（label_width px(96.)）；
//! 输出目录/排除 → 真 `Input`；分卷 → `NumberInput`；密码 → `InputState`
//! 密文模式（masked）；智能解压/尽力而为 → `Switch`。
//!
//! **契约偏差登记**（README §2 → 实测，T-G40 复核）：gpui-kit ActiveTheme
//! （`Theme::global(cx)`）与 SelectState/InputState（窗口亲和）要求构建期持有
//! `&App`/`&mut Window`——纯 `fn(props, on)` 签名无法取主题色/装配实体。偏差：
//! 追加首位 `cx: &App` 参数；Select/InputState 实体由 T-G30 在 open_dialog
//! builder 内预构建放入 props（[`select_string_state`] / [`input_state`] /
//! [`number_input_state`]）。
//!
//! # 密码语义（02 §1.2）
//!
//! 密码 Input 值只存组件内部态（InputState Entity）；`PasswordSubmit(text)`
//! 发出后立即清空内部态 + `cx.notify()`——组件不持有密码副本（Zeroizing
//! 构造归 T-G30 绑定层）。
//!
//! # 禁止事项（T-G11 §3.4）
//!
//! 不做参数校验/构建 JobSpec/决策缓存——全部 `DialogEvent` 上抛（T-G30 →
//! T41 VM）。白名单文案硬编码于本目录常量（与 T41 常量对齐，值不一致以
//! T41 为准并登记偏差）。
//!
//! # 子模块（T-STD：本文件只做声明与再导出）
//!
//! 业务项迁出 `shared`（共用件）/`fields`（实体构造）后，本模块以 `pub use` /
//! `pub(crate) use` 保持原公开面：外部调用方 `crate::ui::dialogs::X` 与兄弟
//! 对话框的 `super::X` 路径**零改动**。

mod about;
mod compress;
mod conflict;
mod edit_confirm;
mod extract;
mod fields;
mod job_detail;
mod password;
mod shared;

pub use about::open_about_dialog;
pub use compress::{
    CompressPanelProps, compress_level_detail_text, compress_level_speed_text,
    compress_overwrite_dialog, compress_panel,
};
pub use conflict::{ConflictDialogProps, conflict_dialog};
pub use edit_confirm::{
    add_files_confirm_dialog, delete_files_confirm_dialog, edit_error_dialog, name_conflict_dialog,
};
pub use extract::{ExtractPanelProps, extract_panel};
pub use job_detail::open_job_detail_dialog;
pub use password::{PasswordDialogProps, password_dialog};

pub use crate::ui::contracts::DialogEvent;

// —— 白名单常量（`crate::app::labels` 派生；原 `dialogs::X` 调用路径不变）——
pub use shared::{
    CODEPAGE_LABEL_KEYS, CONFLICT_LABEL_KEYS, DICT_LABEL_KEYS, ENC_METHOD_LABEL_KEYS,
    FORMAT_LABEL_KEYS, LANG_LABEL_KEYS, LEVEL_LABEL_KEYS, METHOD_LABEL_KEYS,
    SETTINGS_FORMAT_LABEL_KEYS, SETTINGS_LEVEL_LABEL_KEYS, SPLIT_LABEL_KEYS, THREADS_LABEL_KEYS,
    UPDATE_MODE_LABEL_KEYS,
};

// —— 共用件（`pub(crate)` 面：跨对话框 + 相邻视图 `crate::ui::dialogs::X` 消费）——
pub(crate) use shared::{
    HintTone, dialog_form, error_line, field_col, hint_line, level_detail_text, level_speed_key,
    muted_fg, read_only_field,
};

// —— 表单/输入通道构造（窗口亲和实体预构建；`op_window` / `prompt_window` 消费）——
pub(crate) use fields::{input_state, level_slider_state, number_input_state, select_string_state};

// —— 目录内私有共用件（兄弟对话框经 `super::X` 消费；可见性提为 `pub(super)`）——
use shared::{dialog_shell, dialog_shell_untitled, panel_shell, section_header, summary_card};

#[cfg(test)]
mod tests;
