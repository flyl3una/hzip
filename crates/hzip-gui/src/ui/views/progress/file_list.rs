//! 输出区域（T-PW 输出 Tabs；结果 / 成功项 / 失败项 三页，可变行高虚拟滚动）。
//!
//! # 结构
//!
//! ```text
//! TabBar（pill；结果 / 成功项 / 失败项）
//! └ list（可变行高虚拟列表；行默认 20px、内容折行则长高——长路径/长原因不再被截断；
//!    渲染 O(可见行)——输出万行级不卡；追加实时滚底；切页各持 ListState 保持位置）
//! ```
//!
//! # 性能（R-GUI 走查：Textarea 全量 set_value 的 O(N²) 追加 → 本方案）
//!
//! 原实现每追加一行经 Textarea `set_value(全文本)` 重建编辑器（含重高亮），
//! 全程 O(N²)——输出越多越卡。本方案：行数据 = `Vec<OutLine>` 追加 O(1)，
//! 渲染只画可见行（`ListState::splice` 增量登记，不重测旧行）；终局后静态。
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

use crate::ui::constant::{MONO_ROW_H, OUTPUT_BOX_MIN_H, OUTPUT_LIST_OVERDRAW};
use crate::ui::spacing::sp;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Selectable as _, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Div, ListAlignment, ListState, SharedString, Styled, div, list, relative,
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
    /// 三页签滚动句柄（切页保持位置；追加实时滚底）。
    ///
    /// `ListState` 同时是**行高可变**列表的滚动源（SumTree 逐项测量）与 kit 滚动条
    /// 句柄（`gpui_base::ScrollbarHandle` 的 `ListState` 实现）。
    scrolls: [ListState; 3],
}

impl Default for OutputState {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputState {
    /// 创建（空行集；无窗口依赖——虚拟列表滚动句柄非实体）。
    pub fn new() -> Self {
        let fresh = || ListState::new(0, ListAlignment::Top, OUTPUT_LIST_OVERDRAW);
        Self {
            result: Vec::new(),
            ok: Vec::new(),
            fail: Vec::new(),
            cursor: 0,
            summary_done: false,
            scrolls: [fresh(), fresh(), fresh()],
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
            let (mut add_result, mut add_ok, mut add_fail) = (0usize, 0, 0);
            for l in &lines[self.cursor..] {
                self.result.push(l.clone());
                add_result += 1;
                if l.failed {
                    self.fail.push(l.clone());
                    add_fail += 1;
                } else {
                    self.ok.push(l.clone());
                    add_ok += 1;
                }
            }
            self.cursor = lines.len();
            self.splice_tabs(add_result, add_ok, add_fail);
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
                self.scrolls[OutputTab::Result.index()]
                    .splice(self.result.len() - 1..self.result.len() - 1, 1);
                self.scroll_all_to_bottom();
            }
        }
    }

    /// 三页签各追加 `n` 行（`splice` 是 `ListState` 的**增量**登记口：
    /// 只把新项标为待测，不重算已测项——与旧 `uniform_list` 的 O(可见行) 同量级）。
    fn splice_tabs(&self, add_result: usize, add_ok: usize, add_fail: usize) {
        for (tab, added) in [
            (OutputTab::Result, add_result),
            (OutputTab::Ok, add_ok),
            (OutputTab::Fail, add_fail),
        ] {
            if added == 0 {
                continue;
            }
            let st = &self.scrolls[tab.index()];
            let len = st.item_count();
            st.splice(len..len, added);
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
        // 全量重建 = 行集整体换过 ⇒ `reset`（旧行全部重测；此时行集通常已在
        // 数百行内，一次重测可接受，且比逐个 splice 少一次 O(N) 遍历差值）
        for tab in [OutputTab::Result, OutputTab::Ok, OutputTab::Fail] {
            self.scrolls[tab.index()].reset(self.lines_of(tab).len());
        }
        self.scroll_all_to_bottom();
    }

    /// 三页签滚动句柄定位末行（追加即滚底——控制台语义）。
    ///
    /// `scroll_to_end`（而非 `scroll_to_reveal_item`）以**条目总数**为锚点：末行还在
    /// 长高（折行、流式追加）时也照样贴底，这正是输出区要的形态。
    fn scroll_all_to_bottom(&self) {
        for tab in [OutputTab::Result, OutputTab::Ok, OutputTab::Fail] {
            self.scrolls[tab.index()].scroll_to_end();
        }
    }
}

// ===================== 组件（TabBar + 可变行高 list 虚拟列表） =====================

/// 输出区组件（TabBar + 可变行高虚拟列表；自适应高度吃满剩余空间）。
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
    let scroll = state.scrolls[tab.index()].clone();
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
                    // → 只画可见行；行渲染零拷贝读持有方实体）。
                    //
                    // **可变行高**（用户口径 2026-10-16）：`uniform_list` 要求行高一致，
                    // 长路径/长原因只能被 `.truncate()` 截断——改用 `list`（SumTree
                    // 逐项测量），行内容折行后高度自然增长，内容不再显示不全。
                    list(scroll.clone(), move |ix, _window, cx| {
                        let w = view.read(cx);
                        let Some(out) = w.output.as_ref() else {
                            return div().into_any_element();
                        };
                        out_row(tab, &out.lines_of(tab)[ix], cx).into_any_element()
                    })
                    .h_full()
                    .w_full()
                    // 右侧让位滚动条（R-GUI 走查：覆盖层滚动条会压住行尾数据——
                    // 行内容右收 8px，滚动条画在让出的栏内不再重叠）
                    .pr(sp(3.)),
                )
                // 默认显示垂直滚动条（覆盖层；滚动句柄与列表共享——
                // 拖滚动条/滚轮/滚底三者同一滚动源）
                .vertical_scrollbar(&scroll)
                .border_1()
                .border_color(muted)
                // 大框圆角 = 设置圆角 × 2（用户口径 2026-10-16；0 = 直角同样成立）
                .rounded(crate::ui::theme::big_radius(cx)),
        )
}

/// 单行渲染（行高**可变**：默认一行 20px，内容折行则按行数增长）。
///
/// 格式对齐旧文本投影：结果页 `✓ name (size)` / `✗ name — 原因`；
/// 成功/失败页无符号、尾注同款；`name` 空行 = 终局描述（文本自带 ✔/⚠/✘）。
///
/// # 折行（用户口径 2026-10-16）
///
/// 三处文本都**不**再 `.truncate()`（= `nowrap + ellipsis`，正是「内容显示不全」的
/// 直接原因），改为容器内折行：
/// - 名字列 `flex_1 + min_w_0`：可被压到比内容窄 ⇒ 长路径按字符折行（`LineWrapper`
///   逐字符断行，无空格的长路径也折得开）；
/// - 尾注列 `flex_shrink_0 + max_w(50%)`：正常宽度下不动（保持旧观感），只有它自己
///   就超长时才折行，绝不横向溢出把名字列挤没；
/// - 描述行 `flex_1 + min_w_0`：整行折行。
///
/// `items_start`（原 `items_center`）：折行后符号/尾注与首行对齐，而不是飘在块中间。
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
        .min_h(MONO_ROW_H)
        .w_full()
        .flex()
        .items_start()
        .gap(sp(1.))
        .px(sp(1.));
    if l.name.is_empty() {
        // 终局/独立描述行
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .font_family("Consolas")
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
                .max_w(relative(0.5))
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
