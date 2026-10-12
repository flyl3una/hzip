//! 对话框共用件：白名单常量 + 共享小件（提示行/字段列/骨架/表单装配）。
//!
//! 从 `dialogs/mod.rs` 迁出（T-STD：`mod.rs` 只做声明与再导出）；内容逐字未改。
//! 承载形态说明（`panel_shell` vs `dialog_shell`）见 `crate::ui::dialogs` 模块文档。

use crate::ui::constant::{FORM_LABEL_W, HAIRLINE};
use crate::ui::spacing::sp;
use crate::viewmodel::dialog_vm::LevelDetail;
use gpui_kit::component::dialog::{Dialog, DialogFooter, DialogHeader, DialogTitle};
use gpui_kit::component::form::Form;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, SharedString, Styled, div};

// —— 白名单（T-GUI T12 收口：label↔value 映射定义于 `crate::app::labels`，
// 此处仅派生 UI label 序列供 Select/文案消费）——

/// 白名单：压缩格式（**单格式子集**——组合格式界面屏蔽，见 `labels::UI_FORMAT`）。
pub const FORMAT_LABEL_KEYS: [&str; 7] =
    crate::app::labels::label_keys(crate::app::labels::UI_FORMAT);
/// 白名单：压缩级别。
pub const LEVEL_LABEL_KEYS: [&str; 6] = crate::app::labels::label_keys(crate::app::labels::LEVEL);
/// 白名单：冲突策略。
pub const CONFLICT_LABEL_KEYS: [&str; 5] =
    crate::app::labels::label_keys(crate::app::labels::CONFLICT);
/// 白名单：文件名编码。
pub const CODEPAGE_LABEL_KEYS: [&str; 7] =
    crate::app::labels::label_keys(crate::app::labels::CODEPAGE);
/// 白名单：设置-语言。
pub const LANG_LABEL_KEYS: [&str; 3] = crate::app::labels::label_keys(crate::app::labels::LANG);
/// 白名单：设置-默认格式（同 [`FORMAT_LABEL_KEYS`]，单格式子集）。
pub const SETTINGS_FORMAT_LABEL_KEYS: [&str; 7] = FORMAT_LABEL_KEYS;
/// 白名单：设置-默认级别（同 [`LEVEL_LABEL_KEYS`]）。
pub const SETTINGS_LEVEL_LABEL_KEYS: [&str; 6] = LEVEL_LABEL_KEYS;
/// 白名单：压缩方式（`labels::METHOD`）。
pub const METHOD_LABEL_KEYS: [&str; 4] = crate::app::labels::label_keys(crate::app::labels::METHOD);
/// 白名单：分卷预设（`labels::SPLIT`）。
pub const SPLIT_LABEL_KEYS: [&str; 7] = crate::app::labels::label_keys(crate::app::labels::SPLIT);
/// 白名单：线程数（`labels::THREADS`）。
pub const THREADS_LABEL_KEYS: [&str; 6] =
    crate::app::labels::label_keys(crate::app::labels::THREADS);
/// 白名单：加密方式（`labels::ENC_METHOD`）。
pub const ENC_METHOD_LABEL_KEYS: [&str; 4] =
    crate::app::labels::label_keys(crate::app::labels::ENC_METHOD);
/// 白名单：LZMA2 字典大小（`labels::DICT`）。
pub const DICT_LABEL_KEYS: [&str; 7] = crate::app::labels::label_keys(crate::app::labels::DICT);
/// 白名单：更新方式（`labels::UPDATE_MODE`）。
pub const UPDATE_MODE_LABEL_KEYS: [&str; 2] =
    crate::app::labels::label_keys(crate::app::labels::UPDATE_MODE);

// —— 共用小件（T-G56：form_row/text_display 自绘件删除）——

/// 说明行语气（灰 = 能力说明；警告 = 需注意；危险 = 校验失败）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HintTone {
    /// 次要信息（能力受限原因、分卷命名、实参提示）
    Muted,
    /// 需注意（配置有代价、格式能力不如预期）
    Warn,
    /// 校验失败（非法文件名、两次密码不一致）
    Danger,
}

/// 内嵌面（inset）：源摘要卡 / 摘要行 / 只读展示的底色。
///
/// 底色取 `secondary`（hzip 挂 = `SURFACE_HOVER` #eef0f2），**不是** `background`：
/// 卡片/面板底就是 `background`（浅色 = 栏色纯白，T-OPT 2026-10-08 起与菜单/工具栏
/// 同色），同色会让只读框除 1 px 边框外完全看不出层次（09 §12 问题 ①）。
fn inset_surface(cx: &App) -> gpui_kit::Div {
    div()
        .w_full()
        .px(sp(2.))
        .py(sp(1.))
        // 圆角跟全局主题（`[ui].corner_radius`），不再抄死 6px
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
}

/// 纯只读展示行（只读字段；唯一合法用途 = 摘要类文本——数据来自 props 非受控输入）。
pub(crate) fn read_only_field(cx: &App, text: &str) -> impl IntoElement {
    inset_surface(cx)
        .text_sm()
        .text_ellipsis()
        .child(SharedString::from(text.to_owned()))
}

/// 说明行（小字；语气决定颜色）。空串不渲染——避免留下空白行撑高对话框。
pub(crate) fn hint_line(cx: &App, tone: HintTone, text: &str) -> impl IntoElement {
    if text.is_empty() {
        return div();
    }
    let color = match tone {
        HintTone::Muted => muted_fg(cx),
        HintTone::Warn => cx.theme().warning,
        HintTone::Danger => danger_fg(cx),
    };
    div()
        .text_xs()
        .text_color(color)
        .child(SharedString::from(text.to_owned()))
}

/// 字段内容列：控件 +（可选）下方说明行。
///
/// 说明行放在**控件下方同一列**内，缩进由表单的 label_width 自动对齐——
/// 不用手写 `margin_left(108px)` 这类与 label 宽度耦合的魔法值。
pub(crate) fn field_col(
    cx: &App,
    hint: Option<&str>,
    tone: HintTone,
    control: impl IntoElement,
) -> impl IntoElement {
    let col = v_flex().w_full().flex_1().gap(sp(1.)).child(control);
    match hint {
        Some(h) if !h.is_empty() => col.child(hint_line(cx, tone, h)),
        _ => col,
    }
}

/// 分区标题（小字标签 + 右侧分隔线）。
pub(super) fn section_header(cx: &App, label: impl Into<SharedString>) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap(sp(2.))
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(muted_fg(cx))
                .child(label.into()),
        )
        .child(div().flex_1().h(HAIRLINE).bg(cx.theme().border))
}

/// 内嵌摘要卡（源摘要 / 参数摘要）：标题 + 内容同卡。
pub(super) fn summary_card(
    cx: &App,
    title: impl Into<SharedString>,
    body: impl IntoElement,
) -> impl IntoElement {
    inset_surface(cx)
        .flex()
        .items_baseline()
        .gap(sp(2.))
        .child(
            div()
                .flex_none()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(title.into()),
        )
        .child(
            div()
                .flex_1()
                .text_xs()
                .text_color(muted_fg(cx))
                .child(body),
        )
}

/// 压缩级别实参 → 提示行文案（09 §6.1）。
///
/// 数值来自 [`crate::viewmodel::dialog_vm::level_detail`]（即 `hzip_codec::level`
/// 映射表），故界面显示的字典/级别与实际写入用的**同源**。
///
/// **不显示「预计压缩率」**：该数字当前无可靠来源（09 §14 登记为独立需求），
/// 与其编一个像模像样的百分比，不如只显示能兑现的实参。
pub(crate) fn level_detail_text(d: &LevelDetail) -> String {
    use LevelDetail as D;
    match d {
        D::Deflate { level } => {
            crate::tr_with("dlg.level.detail_deflate", &[("n", &level.to_string())])
        }
        D::Lzma2 { dict, mode } => crate::tr_with(
            "dlg.level.detail_lzma2",
            &[
                ("dict", &crate::ui::binding::fmt_bytes(*dict)),
                ("mode", mode.as_str()),
            ],
        ),
        D::Xz { preset, extreme } => {
            let n = if *extreme {
                format!("{preset}e")
            } else {
                preset.to_string()
            };
            crate::tr_with("dlg.level.detail_xz", &[("n", &n)])
        }
        D::Zstd { level } => crate::tr_with("dlg.level.detail_zstd", &[("n", &level.to_string())]),
        D::BZip2 { block } => {
            crate::tr_with("dlg.level.detail_bzip2", &[("n", &block.to_string())])
        }
        D::None => crate::tr("dlg.level.detail_none"),
    }
}

/// 级别 → 速度档文案 key（比例↔速度的定性端）。
pub(crate) fn level_speed_key(level: hzip_core::Level) -> &'static str {
    use hzip_core::Level as L;
    match level {
        L::Store => "dlg.level.speed_none",
        L::Fastest => "dlg.level.speed_fastest",
        L::Fast => "dlg.level.speed_fast",
        L::Normal => "dlg.level.speed_medium",
        L::Maximum | L::Ultra => "dlg.level.speed_slow",
    }
}

/// 次级文字色（ActiveTheme 全局读取——App 全局态可无窗口上下文访问）。
pub(crate) fn muted_fg(cx: &App) -> gpui_kit::Hsla {
    cx.theme().muted_foreground
}

/// 错误/提示文案区（danger 色小字；空串不渲染）。
pub(crate) fn error_line(cx: &App, text: &str) -> impl IntoElement {
    if text.is_empty() {
        div()
    } else {
        div()
            .text_xs()
            .text_color(danger_fg(cx))
            .child(SharedString::from(text.to_owned()))
    }
}

/// danger 语义色（全局 Theme 读取）。
fn danger_fg(cx: &App) -> gpui_kit::Hsla {
    cx.theme().danger
}

/// 表单装配（T-G56 对齐：`Form::horizontal()` + label_width px(96.)）。
pub(crate) fn dialog_form() -> Form {
    Form::horizontal().label_width(FORM_LABEL_W)
}

/// 窗口级参数面板骨架：头部固定 / **正文滚动** / 底部固定。
///
/// 形态与 `views::progress::ProgressWindow` 同款（窗口内容视图，不是弹层）：
///
/// ```text
/// ┌ 头部（flex_shrink_0）标题 · 对象摘要/统计 ─────────────┐
/// ├ 正文（flex_1 + min_h_0 + overflow_y_scrollbar）───────┤
/// │   参数分区 / 折叠区 / 摘要卡（超出即可滚）             │
/// ├ 底部（flex_shrink_0）提示·错误 + [取消][开始] ────────┤
/// └───────────────────────────────────────────────────────┘
/// ```
///
/// 三条布局不变量（改这个函数前先读）：
/// 1. **正文是唯一弹性区**（`flex_1`）——头部/底部一律 `flex_shrink_0`，
///    否则窗口变矮时它们会被压扁（按钮文字被裁）。
/// 2. **正文必须 `min_h_0`**——flex 项默认 `min-height:auto`，不置 0 的话
///    内容高度会把弹性区顶开，滚动条永不出现（典型「看不到滚动条」根因）。
/// 3. **滚动挂在正文自身**（`.overflow_y_scrollbar()`）——kit 的
///    `ScrollableElement` 保留调用方尺寸（`flex_1`/`min_h_0` 均被搬进滚动根）。
pub(super) fn panel_shell(
    cx: &App,
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    footer: impl IntoElement,
    body: impl IntoElement,
) -> impl IntoElement {
    let border = cx.theme().border;
    let muted = muted_fg(cx);
    v_flex()
        .size_full()
        .bg(cx.theme().background)
        .text_color(cx.theme().foreground)
        .debug_selector(|| "params-root".to_string())
        // —— 头部（固定）——
        .child(
            v_flex()
                .flex_shrink_0()
                .w_full()
                .px(sp(4.))
                .pt(sp(3.))
                .pb(sp(2.))
                .gap(sp(1.))
                .border_b_1()
                .border_color(border)
                .debug_selector(|| "params-head".to_string())
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(title.into()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_ellipsis()
                        .text_color(muted)
                        .child(subtitle.into()),
                ),
        )
        // —— 正文（滚动；唯一弹性区）——
        .child(
            v_flex()
                .id("params-body")
                .debug_selector(|| "params-body".to_string())
                .flex_1()
                .min_h_0()
                .w_full()
                .px(sp(4.))
                .py(sp(3.))
                .overflow_y_scrollbar()
                .child(
                    v_flex()
                        .w_full()
                        .gap(sp(3.))
                        .debug_selector(|| "params-body-inner".to_string())
                        .child(body),
                ),
        )
        // —— 底部（固定）——
        .child(
            v_flex()
                .flex_shrink_0()
                .w_full()
                .px(sp(4.))
                .pt(sp(2.))
                .pb(sp(3.))
                .gap(sp(2.))
                .border_t_1()
                .border_color(border)
                .debug_selector(|| "params-foot".to_string())
                .child(footer),
        )
}

/// Dialog 外框（Modal 容器 + 关闭按钮 + 遮罩）——全部对话框共用的三件套
/// （T-OPT 项9：收口 `.close_button(true).overlay(true)` 的重复书写）。
fn dialog_frame(cx: &mut App, width: gpui_kit::Pixels) -> Dialog {
    Dialog::new(cx).w(width).close_button(true).overlay(true)
}

/// Dialog 骨架（T-G56 对齐：Modal 容器 + 标题 + footer，不手拼遮罩）。
///
/// `body` 收 `impl IntoElement`（09 改）：压缩对话框的正文含分区标题 / 折叠区 /
/// 摘要卡等**非表单**内容，不再是单个 `Form`；`Form` 本身仍是元素，旧调用方不变。
pub(super) fn dialog_shell(
    cx: &mut App,
    width: gpui_kit::Pixels,
    // i18n：标题在**开窗时**求值（`String`），不用 `&'static str`——
    // 运行时切语言后重新打开即得新语言（不必给动态文案伪造静态生命周期）
    title: impl Into<gpui_kit::SharedString>,
    footer: DialogFooter,
    body: impl IntoElement,
) -> Dialog {
    dialog_frame(cx, width)
        .child(DialogHeader::new().child(DialogTitle::new().child(title.into())))
        .child(body)
        .footer(footer)
}

/// 无标题骨架（与 [`dialog_shell`] 同款外框，仅省略 `DialogHeader`）。
///
/// 供「本体自带说明、不需要标题行」的确认框使用（T-OPT 项9：删除确认原本
/// 手拼同款外框——省略标题是它既有的视觉契约，收口时不得顺手加标题行）。
pub(super) fn dialog_shell_untitled(
    cx: &mut App,
    width: gpui_kit::Pixels,
    footer: DialogFooter,
    body: impl IntoElement,
) -> Dialog {
    dialog_frame(cx, width).child(body).footer(footer)
}
