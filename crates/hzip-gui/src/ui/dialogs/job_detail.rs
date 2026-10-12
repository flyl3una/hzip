//! 任务详情弹窗（任务中心行内「详情」图标钮；用户口径 2026-10-09）。
//!
//! 内容 = 任务行**全量字段**（快照口径：点开那一刻的值——实时速率/剩余看进度
//! 窗口，此处不与它抢事实源）。字段文本复用 [`crate::ui::binding::jobs_row_ui`]
//! 的既有格式化 ⇒ 与任务中心各行**逐字一致**，不再造第二套口径。
//!
//! 关闭：对话框自带 ✕、遮罩点击与 Esc（kit `DialogProps` 默认 `close_button =
//! true` / `overlay_closable = true`），本弹窗无自定义动作。
//!
//! 尺寸：**可拖动右下抓手调整**（用户口径 2026-10-09）。尺寸存进程内 `Global`
//! （本次运行内记忆、跨开窗保持；不落配置——`[ui]` 无对应键，且属临时观感偏好）。

use crate::ui::constant::DIALOG_W;
use crate::ui::spacing::sp;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, CursorStyle, Global, Hsla, MouseButton, Pixels, Point, SharedString, Window, div, px,
};

/// 空值占位（与任务中心单元格同字形 `—`）。
const NA: &str = "—";

// —— 弹窗尺寸（拖动右下抓手改；进程内记忆）——

/// 宽度下限（再窄 label/value 会挤在一起）。
const MIN_W: f32 = 300.;
/// 高度下限（标题 + 两三行字段）。
const MIN_H: f32 = 140.;
/// 高度上限（再高不如在窗内滚动）。
const MAX_H: f32 = 720.;
/// 默认高度（首次打开）。
const DEFAULT_H: f32 = 320.;
/// 抓手边长（点阵图案 + 底边拖动面高度）。
const GRIP: f32 = 14.;
/// 抓手拖动时与视口边缘的留白（弹窗不得被推出窗口）。
const VIEWPORT_MARGIN_X: f32 = 40.;
const VIEWPORT_MARGIN_Y: f32 = 120.;

/// 详情弹窗尺寸（**进程内记忆**：本次运行改过就一直用；不落配置）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct JobDetailSize {
    pub w: f32,
    pub h: f32,
    /// 本次拖动的**按下点**（窗口坐标；`on_mouse_down` 记录，Move 阶段换算位移）。
    ///
    /// 为什么要自己记：`on_drag` 构造器拿到的 `Point` 是**元素内偏移**，而
    /// `DragMoveEvent.event.position` 是**窗口坐标**——两者不同源，直接相减会错。
    /// 用按下点做基准，两侧同为窗口坐标，位移即为精确的拖动量。
    drag_start: Option<Point<Pixels>>,
}

impl Default for JobDetailSize {
    fn default() -> Self {
        Self {
            w: DIALOG_W.as_f32(),
            h: DEFAULT_H,
            drag_start: None,
        }
    }
}

impl Global for JobDetailSize {}

/// 拖动位移 → 新尺寸（下限 / `MAX_H` / 视口留白三重钳制）。纯函数，可单测。
///
/// 视口很小时视口上限会小于下限 ⇒ 高度上限用 `clamp(MIN_H, MAX_H)`（两参是常量、
/// 序确定，不会踩 `clamp` 的 `min > max` panic）；宽度上限取「视口 − 留白」与下限
/// 的 `max`。
pub(crate) fn resized_by(start: (f32, f32), dx: f32, dy: f32, viewport: (f32, f32)) -> (f32, f32) {
    let max_w = (viewport.0 - VIEWPORT_MARGIN_X).max(MIN_W);
    let max_h = (viewport.1 - VIEWPORT_MARGIN_Y).clamp(MIN_H, MAX_H);
    (
        (start.0 + dx).clamp(MIN_W, max_w),
        (start.1 + dy).clamp(MIN_H, max_h),
    )
}

/// 抓手拖拽载荷：**起拖时的尺寸**（Move 阶段据按下点位移换算新尺寸）。
///
/// 类型本身即 `on_drag_move::<GripDrag>` 的匹配键——`on_drag` 传的载荷类型必须
/// 与它一致，否则 Move 回调永不触发（gpui 按 `TypeId` 过滤活动拖拽）。
#[derive(Clone)]
struct GripDrag {
    size: (f32, f32),
}

impl gpui_kit::Render for GripDrag {
    /// 拖拽幽灵：尺寸变化本身就是反馈，不另画浮层。
    fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        div()
    }
}

/// 拖动面（弹窗**整条底边**）+ 右下角装饰点阵（**贴角、无内缩**）。
///
/// 为什么拖动面是整条底边：底边整条都可命中，抓取面积大；右下角点阵既是位置提示
/// 也落在拖动面内。
///
/// **为什么必须让 Dialog 的 padding 归零**（实测 2026-10-09）：kit `Dialog` 默认
/// 四周 16px padding，而子元素（本外框）的命中盒会被这层 padding **裁掉**——表现为
/// 弹窗右侧/下侧各约 16px 一条不可命中（按下穿到背板 ⇒ `overlay_closable` 直接
/// 关窗），点阵贴角也就点不中。padding 归零后外框与弹窗同盒，右下角整片可命中。
///
/// `size` = 起拖时的尺寸快照（载荷），位移换算以按下点为基准（两者同为窗口坐标）。
fn resize_grip(muted: Hsla, size: (f32, f32)) -> impl IntoElement {
    let dots = v_flex()
        .w(px(GRIP))
        .h(px(GRIP))
        .gap(px(2.))
        .children((0..3).map(|row| {
            h_flex()
                .gap(px(2.))
                .justify_end()
                .children((0..(row + 1)).map(|_| div().size(px(2.)).rounded_full().bg(muted)))
        }));
    h_flex()
        .w_full()
        .justify_end()
        .items_end()
        .h(px(GRIP))
        .cursor(CursorStyle::ResizeUpLeftDownRight)
        .when(cfg!(test), |d| {
            d.debug_selector(|| "job-detail-resize".into())
        })
        .id("job-detail-resize")
        .on_mouse_down(MouseButton::Left, |ev, _window, cx| {
            cx.update_global::<JobDetailSize, _>(|s, _| s.drag_start = Some(ev.position));
        })
        .on_drag(GripDrag { size }, move |grip, _offset, _window, cx| {
            // 吞掉拖动事件，别让动作穿到底下（背板相关手势只认自己）
            cx.stop_propagation();
            cx.new(|_| grip.clone())
        })
        .on_drag_move::<GripDrag>(move |ev, window, cx| {
            let size = ev.drag(cx).size;
            let Some(from) = cx.global::<JobDetailSize>().drag_start else {
                return; // 无按下点（异常路径）：本次拖动忽略，不改尺寸
            };
            let vp = window.viewport_size();
            let (w, h) = resized_by(
                size,
                (ev.event.position.x - from.x).as_f32(),
                (ev.event.position.y - from.y).as_f32(),
                (vp.width.as_f32(), vp.height.as_f32()),
            );
            cx.update_global::<JobDetailSize, _>(|s, _| {
                s.w = w;
                s.h = h;
            });
            // 尺寸是全局态（不在快照里）⇒ 改完主动重绘；弹窗 builder 每帧重跑，
            // 新尺寸随之生效（拖动过程中即时应手）。
            cx.refresh_windows();
        })
        .child(dots)
}

/// 详情字段行：`label` = i18n key（渲染时查表），值 = 已格式化文本。
///
/// 纯函数（可单测）：一行一字段，顺序 = 阅读顺序（标识 → 行为 → 状态 →
/// 输入输出 → 格式 → 时间 → 体量 → 进度）。
pub(crate) fn detail_fields(row: &crate::viewmodel::JobRow) -> Vec<(&'static str, String)> {
    let ui = crate::ui::binding::jobs_row_ui(row);
    let mut fields: Vec<(&'static str, String)> = vec![
        // 名称（"解压 demo.zip"）是**数据项**而非弹窗标题（用户口径 2026-10-09）
        ("jobs.col_title", ui.title.clone()),
        ("jobs.detail_id", ui.job_id.to_string()),
        ("jobs.detail_action", ui.action_text.clone()),
        ("jobs.detail_state", ui.state_label.clone()),
        ("jobs.detail_input", ui.input_text.clone()),
        ("jobs.detail_output", ui.output_text.clone()),
        ("jobs.detail_format", ui.format_text.clone()),
        ("jobs.detail_started", ui.started_text.clone()),
        ("jobs.detail_elapsed", ui.elapsed_text.clone()),
        ("jobs.detail_size", ui.size_text.clone()),
        ("jobs.detail_progress", progress_text(row)),
        ("jobs.detail_entries", entries_text(row)),
    ];
    // 当前条目（只在跑动中/等待输入时有值；T-PW 的 `current_entry`）
    if let Some(current) = &row.current_entry {
        fields.push(("jobs.detail_current_entry", current.clone()));
    }
    fields
}

/// 失败原因行（终局原文 + 逐条目明细；**已查表、已去重**）。
///
/// 与任务中心「失败原因」列同口径；去重是因为 `jobs_row_ui` 在**无逐条目
/// 明细**时会用终局摘要兜底填 `failures` ⇒ 与终局原文那条重复。
pub(crate) fn detail_failures(row: &crate::viewmodel::JobRow) -> Vec<String> {
    let ui = crate::ui::binding::jobs_row_ui(row);
    let mut lines: Vec<String> = Vec::new();
    if let Some(final_line) = &ui.final_line
        && !final_line.is_empty()
    {
        lines.push(final_line.clone());
    }
    for (entry, message) in &ui.failures {
        // 条目名为空 = `jobs_row_ui` 用终局摘要**兜底**填进来的那条（逐条目明细
        // 为空时才有）——终局原文已在上面单独成行，这里跳过避免同一条印两遍。
        if entry.is_empty() {
            continue;
        }
        // message 通常是 i18n key（`Error::i18n_key()`），也可能是人读原文
        //（打开记录的失败原因／带 HZ 码的整档失败）——`tr` 对非 key 原样返回。
        let message = crate::tr(message);
        let line = format!("{entry}: {message}");
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines
}

/// 进度文案：有总量 → 百分比；无总量（未知）→ 未知占位（不冒充 0%）。
fn progress_text(row: &crate::viewmodel::JobRow) -> String {
    match row.progress {
        Some(p) => format!("{:.0}%", p * 100.0),
        None => crate::tr("jobs.detail_progress_unknown"),
    }
}

/// 条目文案：`已完成 / 总数`；总数未知（队列中）→ `—`。
fn entries_text(row: &crate::viewmodel::JobRow) -> String {
    if row.entries_total > 0 {
        format!("{} / {}", row.entries_done, row.entries_total)
    } else {
        NA.to_string()
    }
}

/// 打开任务详情弹窗（`row` = 源行值快照）。
pub fn open_job_detail_dialog(window: &mut Window, cx: &mut App, row: crate::viewmodel::JobRow) {
    // 尺寸首开落默认值（此后跨开窗按拖动记忆；进程内，不落配置）
    if !cx.has_global::<JobDetailSize>() {
        cx.set_global(JobDetailSize::default());
    }
    let fields = detail_fields(&row);
    let failures = detail_failures(&row);
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    window.open_dialog(cx, move |dialog, window, cx| {
        // 尺寸每帧从 `Global` 现读（抓手拖动改了值 → `refresh_windows` → 本闭包
        // 重跑即生效），并钳进当前视口（窗口变小后旧尺寸不得越界）
        let vp = window.viewport_size();
        let cur = *cx.global::<JobDetailSize>();
        // 只做钳制（dx/dy = 0）：窗口变小后旧尺寸不得越界
        let (w, h) = resized_by(
            (cur.w, cur.h),
            0.,
            0.,
            (vp.width.as_f32(), vp.height.as_f32()),
        );

        // 内容 + 固定尺寸外框：明细可能很长（逐条目失败），滚动收在框内。
        // 内边距挂内容自己（Dialog 的 padding 已归零——见 `resize_grip` 文档）。
        let mut body = v_flex().gap(sp(1.5)).px(sp(4.)).py(sp(3.));
        for (label, value) in &fields {
            body = body.child(
                h_flex()
                    // label 与 value **同一字号** + 顶部对齐 ⇒ 行框等高、文字基线
                    // 自然对齐（用户实报 2026-10-09：旧版 label `text_xs` 比 value
                    // `text_sm` 小，顶部对齐时小字号墨迹看起来「偏上一点点」）。
                    .items_start()
                    .gap(sp(2.))
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(96.))
                            .text_sm()
                            .text_color(muted)
                            .child(SharedString::from(crate::tr(label))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .child(SharedString::from(value.clone())),
                    ),
            );
        }
        if !failures.is_empty() {
            body = body.child(
                div()
                    .pt(sp(2.))
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(crate::tr("jobs.detail_failures"))),
            );
            for line in &failures {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(danger)
                        .overflow_x_hidden()
                        .child(SharedString::from(line.clone())),
                );
            }
        }
        // 固定尺寸外框：内容在框内滚动；右下角抓手拖动改尺寸
        let frame = v_flex()
            .debug_selector(|| "job-detail-body".into())
            .w(px(w))
            .h(px(h))
            // 滚动区吃满剩余高度（`flex_1 + min_h_0`），底行留给右下抓手
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(body),
            )
            // 拖动面（整条底边）+ 右下装饰点阵——见 `resize_grip`
            .child(resize_grip(muted, (w, h)));
        dialog
            .p(px(0.))
            // 标题 = 固定文案（"任务详情"）；任务名称作**数据项**列在正文首行
            .title(
                div()
                    .pl(sp(4.))
                    // 右侧让开 kit 自带的 ✕（padding 归零后它落在 top/right = 8px）
                    .pr(sp(8.))
                    .pt(sp(3.))
                    .pb(sp(1.))
                    .child(crate::tr("jobs.detail_title")),
            )
            .width(px(w))
            .child(frame)
    });
}

#[cfg(test)]
#[path = "tests/job_detail.rs"]
mod tests;
