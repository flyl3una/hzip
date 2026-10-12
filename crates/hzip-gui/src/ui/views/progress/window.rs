//! 进度窗口实体与渲染（`ProgressWindow` + `Render` + 渲染侧纯派生件）。
//!
//! 从 `views/progress/mod.rs` 迁出（T-STD：`mod.rs` 只做声明与再导出）；内容逐字未改。
//! 数据面 [`ProgressWindow::apply`]：行数据纯拷贝，文本/颜色全部渲染侧派生。

use crate::ui::binding::{JobStateSemantic, JobsRowUi, fmt_bytes};
use crate::ui::constant::{CONTENT_MAX_W, PROGRESS_CONTENT_MAX_H};
use crate::ui::spacing::{component_size, sp};
use crate::ui::widgets::AppIcon;
use crate::viewmodel::{JobRow, JobRowState};
use gpui_kit::component::badge::Badge;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme, Sizable, Size, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, IntoElement, Render, SharedString, Styled, Window, div};

use super::file_list::{self, err_text, output_area};

/// 取消回调类型（`job_id` + gpui App 现场；type_complexity 治理别名）。
pub type CancelHandler = Box<dyn Fn(u64, &mut App)>;

/// 复选框回调类型（新勾选值 + gpui App 现场；shell 层落「完成后退出」配置）。
pub type ExitToggleHandler = Box<dyn Fn(bool, &mut App)>;

/// 进度窗口实体（数据面 = JobRow 纯拷贝快照 + 开窗静态文案）。
pub struct ProgressWindow {
    /// 任务 ID（注册表键；终局自动关判定）
    pub job_id: u64,
    /// 窗口标题（开窗时静态：行为 + 档案名）
    pub title: String,
    /// 概要行（源 → 目标；开窗时静态）
    pub summary: (String, String),
    /// 最新行数据（泵 10 Hz 灌入）
    pub row: JobRow,
    /// 输出区虚拟列表数据面（三页签行集 + 滚动句柄；窗口闭包内回填；
    /// None = 窗口未构建完成，渲染显示空输出区）
    pub(crate) output: Option<file_list::OutputState>,
    /// 当前选中输出页签
    pub(crate) output_tab: file_list::OutputTab,
    /// 取消回调（与任务中心取消同一通道；`job_id` + gpui App 现场）
    pub on_cancel: CancelHandler,
    /// 「完成后退出」勾选显示态（开窗时读 `[jobs].exit_on_finish`；
    /// 泵分发时回灌——设置页改动同步到已开的窗口）
    pub exit_on_finish: bool,
    /// 复选框回调（shell 层经 `apply_setting` 落「完成后退出」——与设置页
    /// 同一配置同一链路；AppData 触碰收口在 shell 层，views 只消费注入闭包）
    pub on_exit_toggle: ExitToggleHandler,
    /// 本进程是否允许「完成后退出」（= `ALLOW_QUIT_ON_FINISH`，开窗时定值）：
    /// 主界面 false ⇒ 复选框**不渲染**（用户口径 2026-09-29 第 ⑧ 条：业务操作
    /// 完成后不得把 hzip-gui 带走；显示一个必然无效的开关＝谎报）。
    pub allow_quit: bool,
}

impl ProgressWindow {
    /// 泵数据灌入（行数据纯拷贝 + 输出区行集增量推入并滚底）。
    pub fn apply(&mut self, row: &JobRow) {
        self.row = row.clone();
        if let Some(out) = &mut self.output {
            out.apply(row);
        }
    }

    /// 语言切换后重建输出区（既有行文案已定稿 → 必须整表重算）。
    pub fn relocalize(&mut self, row: &JobRow) {
        self.row = row.clone();
        if let Some(out) = &mut self.output {
            out.sync_full(row);
        }
    }
}

impl Render for ProgressWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui: JobsRowUi = crate::ui::binding::jobs_row_ui(&self.row);
        let (src, dest) = self.summary.clone();
        let current = self.row.current_entry.clone();
        let progress = self.row.progress;
        let speed = self.row.speed_bps;
        let eta = self.row.eta;
        let entries_done = self.row.entries_done;
        let entries_total = self.row.entries_total.max(self.row.entries_done);
        let is_active = !crate::viewmodel::jobs_vm::is_terminal(self.row.state);
        let state_color = semantic_color(cx, ui.state_semantic);
        let job_id = self.job_id;

        // Esc 关窗（T-PW README §5：✕/Esc/后台 = 仅关窗，任务继续）
        v_flex()
            .id("pw-root")
            .on_key_down(|ev: &gpui_kit::KeyDownEvent, window, _| {
                if ev.keystroke.key == "escape" {
                    window.remove_window();
                }
            })
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // 尺寸软约束（T-PW README §2 偏差登记）：根铺满窗口挂主题背景，
            // 内容列 max 720×640 居中——拖大不露灰底、不超上限。
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .w_full()
                    .h_full()
                    .max_w(CONTENT_MAX_W)
                    .max_h(PROGRESS_CONTENT_MAX_H)
                    .p(sp(4.))
                    .gap(sp(2.))
                    .debug_selector(|| "pw-content".into())
                    // 概要行：Archive 图标 + 源 → 目标
                    .child(
                        h_flex()
                            .gap(sp(2.))
                            .items_center()
                            .flex_shrink_0()
                            .overflow_x_hidden()
                            .child(gpui_kit::component::Icon::new(AppIcon::Archive).size_4())
                            .child(
                                div()
                                    .text_sm()
                                    .child(SharedString::from(if dest.is_empty() {
                                        src
                                    } else {
                                        format!("{src} → {dest}")
                                    })),
                            ),
                    )
                    // 大号百分比（Bandizip 形态；运行中语义色）
                    .child(
                        v_flex().flex_shrink_0().gap(sp(1.)).child(
                            div()
                                .text_3xl()
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .text_color(state_color)
                                .child(SharedString::from(match progress {
                                    Some(p) => format!("{:.0}%", p * 100.0),
                                    None => "…".into(),
                                })),
                        ),
                    )
                    .child(
                        // 无 total（队列中/压缩不确定态）→ indeterminate 滑动
                        Progress::new(("pw-progress", job_id))
                            .value((progress.unwrap_or(0.0) * 100.0) as f32)
                            .loading(progress.is_none() && is_active)
                            .w_full(),
                    )
                    // 状态 + 统计行（R-GUI 走查：自底部上移到进度条正下方，
                    // 同行左右分布——左状态语义色，右速度/剩余/条数 muted）
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .justify_between()
                            .items_center()
                            .child(
                                h_flex()
                                    .gap(sp(2.))
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .relative()
                                            .child(Badge::new().dot().color(state_color)),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(state_color)
                                            .child(SharedString::from(ui.state_label)),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(stat_line(
                                        speed,
                                        eta,
                                        entries_done,
                                        entries_total,
                                    ))),
                            ),
                    )
                    // 当前条目（File 图标 + muted 标签）
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap(sp(1.))
                            .items_center()
                            .overflow_x_hidden()
                            .child(gpui_kit::component::Icon::new(AppIcon::File).size_3())
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(match (&current, is_active) {
                                        (Some(name), _) => crate::tr_with(
                                            "progress.current",
                                            &[("name", name.as_str())],
                                        ),
                                        (None, false) => match &self.row.final_line {
                                            Some(fl) => err_text(fl.code, &fl.message),
                                            None => match self.row.state {
                                                JobRowState::Done => {
                                                    crate::tr("progress.state_done")
                                                }
                                                JobRowState::Cancelled => {
                                                    crate::tr("progress.state_cancelled")
                                                }
                                                _ => "—".into(),
                                            },
                                        },
                                        (None, true) => active_state_line(self.row.state),
                                    })),
                            ),
                    )
                    // 输出区（Tabs：结果/成功项/失败项 + 只读 Textarea 自适应高度——
                    // flex_1 吃满剩余空间，随窗口伸缩）。
                    // 直接挂 output_area 的 v_flex（自带 flex_1/min_h_0）——不得
                    // 套裸 div 包装层：gpui 裸 div 默认 Display::Block，子元素
                    // flex_1 失效 → 输出区塌回 min 高（R-GUI 走查布局断链根因）
                    .child(match &self.output {
                        Some(states) => output_area(cx, states, self.output_tab).into_any_element(),
                        None => div().flex_1().min_h_0().into_any_element(),
                    })
                    // 按钮行：左「完成后自动关闭窗口」复选框（与设置页同一配置）
                    // + 右取消（danger）→ 终局「关闭」
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .justify_between()
                            .items_center()
                            .gap(sp(2.))
                            // 「完成后退出」只在**允许自动退出的进程**（Direct 模式 =
                            // 右键动词进程，无主界面）出现：主界面里该开关不生效，
                            // 显示出来只会谎报（用户口径 2026-09-29 第 ⑧ 条）。
                            .children(self.allow_quit.then(|| {
                                // kit Checkbox 把 id 只当 keyed state 键（不注册
                                // 可观测元素）→ 外包 debug_selector 容器供测试定位
                                //（同 dialogs/compress.rs switch_row 的既知约束）。
                                div()
                                    .id("pw-exit-cell")
                                    .debug_selector(|| "pw-exit-on-finish".into())
                                    .child(
                                        crate::ui::widgets::checkbox(
                                            "pw-exit-on-finish",
                                            crate::tr("progress.exit_on_finish"),
                                        )
                                        .checked(self.exit_on_finish)
                                        .on_click(
                                            cx.listener(|this, checked: &bool, _, cx| {
                                                this.exit_on_finish = *checked;
                                                (this.on_exit_toggle)(*checked, cx);
                                            }),
                                        ),
                                    )
                                    .into_any_element()
                            }))
                            .child(if is_active {
                                Button::new("pw-cancel")
                                    .with_size(component_size())
                                    .label(crate::tr("progress.cancel"))
                                    .with_size(Size::Small)
                                    .danger()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        (this.on_cancel)(this.job_id, cx);
                                    }))
                                    .into_any_element()
                            } else {
                                Button::new("pw-close-finish")
                                    .with_size(component_size())
                                    .label(crate::tr("progress.close"))
                                    .with_size(Size::Small)
                                    .on_click(|_, window, _| window.remove_window())
                                    .into_any_element()
                            }),
                    ),
            )
    }
}

/// 状态语义色（渲染侧决策；binding 层只出语义分组——同 jobs.rs state_color）。
fn semantic_color(cx: &App, semantic: JobStateSemantic) -> gpui_kit::Hsla {
    let theme = cx.theme();
    match semantic {
        JobStateSemantic::Neutral => theme.muted_foreground,
        JobStateSemantic::Accent => theme.primary,
        JobStateSemantic::Warning => theme.warning,
        JobStateSemantic::Danger => theme.danger,
        JobStateSemantic::Success => theme.success,
    }
}

/// 统计行文本（速度 · 剩余 · 条数；fmt_bytes 复用）。
pub(super) fn stat_line(
    speed_bps: f64,
    eta: Option<std::time::Duration>,
    done: u64,
    total: u64,
) -> String {
    let speed = if speed_bps > 0.0 {
        format!("{}/s", fmt_bytes(speed_bps as u64))
    } else {
        // 无速度 → 中性占位符（— 为 locale 无关符号，不进文案表）
        "—".into()
    };
    let eta = match eta {
        Some(d) => {
            let s = d.as_secs();
            let time = format!("{:02}:{:02}", s / 60, s % 60);
            crate::tr_with("progress.eta_remaining", &[("time", time.as_str())])
        }
        None => crate::tr("progress.eta_unknown"),
    };
    let (done, total) = (done.to_string(), total.to_string());
    crate::tr_with(
        "progress.stat_line",
        &[
            ("speed", speed.as_str()),
            ("eta", eta.as_str()),
            ("done", done.as_str()),
            ("total", total.as_str()),
        ],
    )
}

/// 活跃任务、**尚无进度数据**时的状态行文案。
///
/// 无进度数据有两种截然不同的处境，文案必须区分，否则界面会撒谎：
///
/// - `Queued`：真排队（引擎并发门控 / 等待输入），此时「等待开始…」是实话；
/// - `Running`：**已经在干活**，只是总量还没算出来——流式归档（tar.gz 等）
///   要先走完整个流才能建出条目索引，实测 57 MB / 16092 条目的 tar.gz 索引
///   阶段耗时 3.6 s，期间零进度事件。此时说「等待开始…」= 把正在解压的任务
///   说成没开始（用户实测症状：窗口显示等待，后台其实在解压）。
///
/// 进度条在 `(progress.is_none() && is_active)` 时已经是不确定态滑动动画，
/// 文案与之一致即可：不确定态 = 「准备中」。
pub(crate) fn active_state_line(state: JobRowState) -> String {
    match state {
        JobRowState::Queued => crate::tr("progress.waiting_start"),
        JobRowState::WaitingInput => crate::tr("job.state_waiting_input"),
        JobRowState::Cancelling => crate::tr("job.state_cancelling"),
        _ => crate::tr("progress.preparing"),
    }
}
