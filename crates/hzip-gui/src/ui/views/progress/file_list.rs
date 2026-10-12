//! 输出区域（T-PW 输出 Tabs；结果 / 成功项 / 失败项 三页，uniform_list 虚拟滚动）。
//!
//! # 结构
//!
//! ```text
//! TabBar（pill；结果 / 成功项 / 失败项）
//! └ uniform_list 虚拟列表（行高 20px 固定；渲染 O(可见行)——输出万行级不卡；
//!    追加实时滚底；切页各持滚动句柄保持位置）
//! ```
//!
//! # 性能（R-GUI 走查：Textarea 全量 set_value 的 O(N²) 追加 → 本方案）
//!
//! 原实现每追加一行经 Textarea `set_value(全文本)` 重建编辑器（含重高亮），
//! 全程 O(N²)——输出越多越卡。本方案：行数据 = `Vec<OutLine>` 追加 O(1)，
//! 渲染只画可见行；终局后静态。
//!
//! # 数据面（纯数据，不依赖 gpui）
//!
//! [`out_lines`] 从 `JobRow` 快照投影结构化行——`OutputState::apply` 以
//! 「上次快照游标」对齐增量，把新增行推入三页签行集（结果/成功/失败），
//! 并把各页滚动句柄定位到末行（实时滚底）。
//!
//! # 输出语义（对齐 Bandizip/7-Zip）
//!
//! - **结果**：持续追加「✓/✗ + 文件名 + 尾注（大小/失败原因）」，失败原因
//!   在发现即追加（运行中 CRC 失败 / best_effort 失败经 `attach_failure_reason`
//!   并入 recent_entries）；终局追加一行结果描述（成功 / 部分成功 / 失败
//!   （原因）/ 已取消）。
//! - **成功项**：仅成功条目（`name (size)`）。
//! - **失败项**：仅失败条目（`name — 原因`）。

use crate::ui::constant::{MONO_ROW_H, OUTPUT_BOX_MIN_H};
use crate::ui::spacing::sp;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Selectable as _, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Div, SharedString, Styled, UniformListScrollHandle, div, uniform_list,
};

use crate::ui::binding::fmt_bytes;
use crate::viewmodel::JobRow;

/// 输出页签（下标 = TabBar 选中位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OutputTab {
    /// 结果（成功 + 失败 + 结果信息，持续追加）
    #[default]
    Result,
    /// 成功项
    Ok,
    /// 失败项
    Fail,
}

impl OutputTab {
    /// 页签文案（TabBar 渲染契约；顺序 = 下标）。
    ///
    /// 经 `crate::tr` 查表（`progress.tab_*`）——locale 切换后窗口重渲染
    /// 即取新文案（输出区每次渲染重建 TabBar）。
    pub fn labels() -> [String; 3] {
        [
            crate::tr("progress.tab_result"),
            crate::tr("progress.tab_ok"),
            crate::tr("progress.tab_fail"),
        ]
    }

    /// TabBar 下标 → 页签（越界回落结果页）。
    pub fn from_index(ix: usize) -> Self {
        match ix {
            0 => Self::Result,
            1 => Self::Ok,
            _ => Self::Fail,
        }
    }

    /// TabBar 下标。
    pub fn index(self) -> usize {
        match self {
            Self::Result => 0,
            Self::Ok => 1,
            Self::Fail => 2,
        }
    }
}

/// 单行输出投影（纯数据；name + 尾注 + 失败标记）。
#[derive(Debug, Clone)]
pub(crate) struct OutLine {
    pub name: String,
    pub tail: String,
    pub failed: bool,
}

/// 失败原因文案（02 §12.2：key = `err.hz_{code}` 查 locales/errors.toml；
/// 查表未命中回落 HZ-xxxx 码串——locale 无关，测试可断言）。
pub fn err_text(code: u32, key: &str) -> String {
    let key_name = format!("err.hz_{code:04}");
    if key.is_empty() || key == key_name {
        let translated = rust_i18n::t!(&key_name, code = format!("HZ-{code:04}")).to_string();
        if translated == key_name || translated.is_empty() {
            format!("HZ-{code:04}")
        } else {
            translated
        }
    } else {
        key.to_string()
    }
}

/// 条目行集投影：已处理条目（运行中增量，含并入原因的失败行）。
///
/// 顺序 = `recent_entries` 原序（时序即输出序）；不含终局描述行（
/// [`summary_text`] 由 [`OutputState::apply`] 殿后追加，不重复置顶）。
/// （终局 `failures` 全集经 `attach_failure_reason` 已并入 recent_entries——
/// 无 EntryDone 事件的 best_effort 跳过条目以下方补齐逻辑覆盖。）
pub(super) fn out_lines(row: &JobRow) -> Vec<OutLine> {
    let mut lines: Vec<OutLine> = row
        .recent_entries
        .iter()
        .map(|e| {
            let (tail, failed) = if e.ok {
                (fmt_bytes(e.bytes), false)
            } else {
                (err_text(e.code, &e.reason), true)
            };
            OutLine {
                name: e.name.clone(),
                tail,
                failed,
            }
        })
        .collect();
    // 终局 failures 中未出现在已处理列表的条目（best_effort 跳过无
    // EntryDone 事件）按序补齐——防重复（EntryDone 上报过的不重复输出）。
    let seen: std::collections::HashSet<&str> =
        row.recent_entries.iter().map(|e| e.name.as_str()).collect();
    for f in &row.failures {
        if !seen.contains(f.entry.as_str()) {
            lines.push(OutLine {
                name: f.entry.clone(),
                tail: err_text(f.code, &f.message),
                failed: true,
            });
        }
    }
    lines
}

/// 终局结果描述行（终局后才非空；失败含原因）。
pub(super) fn summary_text(row: &JobRow) -> String {
    use crate::viewmodel::JobRowState as S;
    match row.state {
        S::Done => crate::tr("progress.summary_done"),
        S::PartialDone => match &row.final_line {
            Some(fl) => {
                let n = row.failures.len().to_string();
                crate::tr_with(
                    "progress.summary_partial_entry",
                    &[("entry", fl.entry.as_str()), ("n", n.as_str())],
                )
            }
            None => {
                let n = row.failures.len().to_string();
                crate::tr_with("progress.summary_partial", &[("n", n.as_str())])
            }
        },
        S::Failed => match &row.final_line {
            Some(fl) => {
                let reason = err_text(fl.code, &fl.message);
                if fl.entry.is_empty() {
                    crate::tr_with(
                        "progress.summary_failed_reason",
                        &[("reason", reason.as_str())],
                    )
                } else {
                    crate::tr_with(
                        "progress.summary_failed_entry",
                        &[("entry", fl.entry.as_str()), ("reason", reason.as_str())],
                    )
                }
            }
            None => crate::tr("progress.summary_failed"),
        },
        S::Cancelled => crate::tr("progress.summary_cancelled"),
        _ => String::new(),
    }
}

// ===================== 数据面（三页签行集 + 滚动句柄） =====================

/// 输出区状态（窗口实体持有；apply 增量推入行集并滚底）。
pub(crate) struct OutputState {
    /// 结果页行（成功 + 失败 + 终局描述行殿后）
    pub result: Vec<OutLine>,
    /// 成功项页行
    pub ok: Vec<OutLine>,
    /// 失败项页行
    pub fail: Vec<OutLine>,
    /// 游标：已推入的 out_lines 行数（10 Hz 快照可能丢事件——按行数对齐增量）
    cursor: usize,
    /// 终局描述行（final/summary 只追加一次）
    summary_done: bool,
    /// 三页签滚动句柄（切页保持位置；追加实时滚底）
    scrolls: [UniformListScrollHandle; 3],
}

impl Default for OutputState {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputState {
    /// 创建（空行集；无窗口依赖——虚拟列表滚动句柄非实体）。
    pub fn new() -> Self {
        Self {
            result: Vec::new(),
            ok: Vec::new(),
            fail: Vec::new(),
            cursor: 0,
            summary_done: false,
            scrolls: [
                UniformListScrollHandle::new(),
                UniformListScrollHandle::new(),
                UniformListScrollHandle::new(),
            ],
        }
    }

    /// 指定页签的行集。
    pub fn lines_of(&self, tab: OutputTab) -> &[OutLine] {
        match tab {
            OutputTab::Result => &self.result,
            OutputTab::Ok => &self.ok,
            OutputTab::Fail => &self.fail,
        }
    }

    /// 泵灌入：以行数游标对齐增量（10 Hz 快照会丢事件——必须按内容对齐）。
    ///
    /// 新增行推入三页签行集（结果页全量、成功/失败页各归其位），
    /// 追加后各页滚动句柄定位末行（实时滚底）。
    pub fn apply(&mut self, row: &JobRow) {
        let lines = out_lines(row);
        if lines.len() > self.cursor {
            for l in &lines[self.cursor..] {
                self.result.push(l.clone());
                if l.failed {
                    self.fail.push(l.clone());
                } else {
                    self.ok.push(l.clone());
                }
            }
            self.cursor = lines.len();
            self.scroll_all_to_bottom();
        }
        // 终局描述行（仅追加一次；Failed/Partial 的失败原因行已含于上面）
        if !self.summary_done {
            let summary = summary_text(row);
            if !summary.is_empty() {
                self.result.push(OutLine {
                    name: String::new(),
                    tail: summary,
                    failed: false,
                });
                self.summary_done = true;
                self.scroll_all_to_bottom();
            }
        }
    }

    /// 全量重建（开窗时行已有数据：任务中心重开/快照领先——直接重建三页行集
    /// 并滚底）。
    pub fn sync_full(&mut self, row: &JobRow) {
        let lines = out_lines(row);
        self.cursor = lines.len();
        let (mut ok, mut fail) = (Vec::new(), Vec::new());
        for l in &lines {
            if l.failed {
                fail.push(l.clone());
            } else {
                ok.push(l.clone());
            }
        }
        let summary = summary_text(row);
        self.summary_done = !summary.is_empty();
        self.result = lines;
        if !summary.is_empty() {
            // 终局描述行殿后（成功/失败页不含描述行——同 apply 语义）
            self.result.push(OutLine {
                name: String::new(),
                tail: summary,
                failed: false,
            });
        }
        self.ok = ok;
        self.fail = fail;
        self.scroll_all_to_bottom();
    }

    /// 三页签滚动句柄定位末行（Bottom 策略：追加实时滚底——控制台语义）。
    fn scroll_all_to_bottom(&self) {
        for (tab, len) in [
            (OutputTab::Result, self.result.len()),
            (OutputTab::Ok, self.ok.len()),
            (OutputTab::Fail, self.fail.len()),
        ] {
            if let Some(last) = len.checked_sub(1) {
                self.scrolls[tab.index()].scroll_to_item(last, gpui_kit::ScrollStrategy::Bottom);
            }
        }
    }
}

// ===================== 组件（TabBar + uniform_list 虚拟列表） =====================

/// 输出区组件（TabBar + uniform_list 虚拟列表；自适应高度吃满剩余空间）。
///
/// `tab` 选中态由调用方（ProgressWindow）持有；行集由 `apply` 增量维护。
/// 行渲染经持有方实体读取（闭包只捕实体句柄——零拷贝访问行集）。
pub(super) fn output_area(
    cx: &Context<super::ProgressWindow>,
    state: &OutputState,
    tab: OutputTab,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let view = cx.entity();
    let scroll = &state.scrolls[tab.index()];
    let item_count = state.lines_of(tab).len();
    v_flex()
        .flex_1()
        .min_h_0()
        .gap(sp(1.))
        .debug_selector(|| "pw-output-area".into())
        .child(
            TabBar::new("pw-output-tabs")
                .pill()
                .selected_index(tab.index())
                .on_click(cx.listener(|this, ix: &usize, _, cx| {
                    this.output_tab = OutputTab::from_index(*ix);
                    cx.notify();
                }))
                .children(
                    OutputTab::labels()
                        .into_iter()
                        .enumerate()
                        .map(|(ix, label)| Tab::new().label(label).selected(ix == tab.index())),
                ),
        )
        .child(
            div()
                .flex_1()
                // 自适应高度（R-GUI 走查：吃满剩余空间 + 120px 小保底防塌陷）
                .min_h(OUTPUT_BOX_MIN_H)
                .relative()
                .debug_selector(|| "pw-output-box".into())
                .child(
                    // 虚拟滚动（R-GUI 走查：Textarea 全量 set_value O(N²) 追加
                    // → uniform_list 只画可见行；行渲染零拷贝读持有方实体）
                    uniform_list(
                        ("pw-output-list", tab.index()),
                        item_count,
                        move |range, _window, cx| {
                            let w = view.read(cx);
                            let Some(out) = w.output.as_ref() else {
                                return Vec::new();
                            };
                            let items = out.lines_of(tab);
                            range.map(|ix| out_row(tab, &items[ix], cx)).collect()
                        },
                    )
                    .track_scroll(scroll)
                    .h_full()
                    .w_full()
                    // 右侧让位滚动条（R-GUI 走查：覆盖层滚动条会压住行尾数据——
                    // 行内容右收 8px，滚动条画在让出的栏内不再重叠）
                    .pr(sp(3.)),
                )
                // 默认显示垂直滚动条（覆盖层；滚动句柄与列表共享——
                // 拖滚动条/滚轮/滚底三者同一滚动源）
                .vertical_scrollbar(scroll)
                .border_1()
                .border_color(muted)
                .rounded_sm(),
        )
}

/// 单行渲染（行高 20px 固定——uniform_list 契约）。
///
/// 格式对齐旧文本投影：结果页 `✓ name (size)` / `✗ name — 原因`；
/// 成功/失败页无符号、尾注同款；`name` 空行 = 终局描述（文本自带 ✔/⚠/✘）。
fn out_row(tab: OutputTab, l: &OutLine, cx: &App) -> Div {
    let theme = cx.theme();
    let text = theme.foreground;
    let tail_color = if l.failed {
        theme.danger
    } else {
        theme.muted_foreground
    };
    let sym_color = if l.failed {
        theme.danger
    } else {
        theme.success
    };
    let mut row = div()
        .h(MONO_ROW_H)
        .w_full()
        .flex()
        .items_center()
        .gap(sp(1.))
        .px(sp(1.));
    if l.name.is_empty() {
        // 终局/独立描述行
        row = row.child(
            div()
                .text_xs()
                .font_family("Consolas")
                .truncate()
                .text_color(text)
                .child(SharedString::from(l.tail.clone())),
        );
    } else {
        if tab == OutputTab::Result {
            row = row.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .font_family("Consolas")
                    .text_color(sym_color)
                    .child(SharedString::from(if l.failed { "✗" } else { "✓" })),
            );
        }
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .font_family("Consolas")
                .truncate()
                .text_color(text)
                .child(SharedString::from(l.name.clone())),
        );
        let tail = if l.failed {
            format!("— {}", l.tail)
        } else {
            format!("({})", l.tail)
        };
        row = row.child(
            div()
                .flex_shrink_0()
                .text_xs()
                .font_family("Consolas")
                .text_color(tail_color)
                .child(SharedString::from(tail)),
        );
    }
    row
}

#[cfg(test)]
#[path = "tests/file_list.rs"]
mod tests;
