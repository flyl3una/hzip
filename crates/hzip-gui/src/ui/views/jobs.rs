//! 任务中心页内容区（T-G21 → R-GUI4 #4 列重设计）。
//!
//! 任务表格 = gpui-kit `DataTable`（十一列：任务/输入/输出/行为/格式/开始
//! 时间/耗时/大小/进度/状态/操作）；状态列 = 语义色圆点 + 文字；操作列取消
//! 按钮仅活跃任务；进度列 `Progress`（`loading` = indeterminate 不确定态）。
//! 行数据 = `JobsRowUi` 快照（`jobs_root` 参数携带；**不持 VM 句柄**）。
//! 进度刷新只能由信号驱动 `cx.notify()`（10 Hz 语义红线——本模块无轮询）。
//!
//! 失败下钻行展开保留手拼（展开区属应用特定布局，T-G56 §3.2）。
//!
//! # 泵装配分工
//!
//! - T-G13 `pump_host::spawn_job_pump`（恢复件）：每任务阻塞消费线程；
//! - T-G21 [`pump_assembly`]: notifier = `SignalTx` 版（`signal_notifier`
//!   桥），10 Hz 节流合并后仅 10 Hz 触发 UI 刷新（C2 语义不变）。

use crate::ui::constant::{JOB_COL_MIN_W, JOB_PROGRESS_W};
use crate::ui::spacing::{component_size, sp};
use gpui_kit::component::badge::Badge;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_kit::component::{ActiveTheme, Sizable, Size, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, IntoElement, SharedString, Styled, Window, div, px};

use crate::ui::binding::{JobStateSemantic, JobsRowUi};
use crate::ui::contracts::{AppStateUiSnapshot, DialogEvent, ShellHandlers};
use crate::ui::widgets::AppIcon;

/// 任务中心表格委托：快照行 + 十一列渲染（R-GUI4 #4 重设计）。
///
/// 列交互（2026-09-15 用户要求，与浏览表格同款）：列宽可拖（`widths`
/// 会话内保持）、列序可拖（`column_order` 重排；刷新重灌不回退）。
pub(crate) struct JobsTableDelegate {
    /// 派生行快照（**`Arc` 共享**：身份可比 → 未变时不重灌、不触发重建）
    rows: std::sync::Arc<Vec<JobsRowUi>>,
    /// 源行集（`JobRow`）：与 `rows` 同下标 1:1（`derive_job_rows` 逐行投影）。
    /// 详情弹窗要展示**原始字段**（条目数、当前条目等派生行未携带的项）故留一份。
    src_rows: std::sync::Arc<Vec<crate::viewmodel::JobRow>>,
    /// 行内动作上行（单任务取消；`None` = 未接线）。与工具栏批量按钮同一通道
    ///（`BatchHandler` → shell → `AppData::consume_dialog_event`）。
    on_batch: Option<BatchHandler>,
    /// 列 key 的实时顺序（拖动列头重排；column() 以下标取定义）
    column_order: Vec<&'static str>,
    /// 拖动后的列宽（col key → px；col_groups 重建时回填）
    widths: Vec<(&'static str, gpui_kit::Pixels)>,
    /// 选中行镜像（`TableEvent::SelectRow` 订阅回写）。
    ///
    /// 为何要镜像：选中底色要自绘（与文件列表同源 `selection_row_bg`，见
    /// `render_tr`），而 `render_tr` 在 `Context<TableState<Self>>` 的渲染期
    /// 被调用——**重入读同一实体 panic**（实测 `entity_map.rs:164`），
    /// 故不能现读 `TableState::selected_row()`，只能由事件订阅回写委托。
    selected_row: Option<usize>,
}

impl JobsTableDelegate {
    /// 列定义（详情/任务/输入/输出/行为/格式/开始时间/耗时/大小/进度/状态/失败原因/操作；
    /// 速度·剩余列已移除——耗时 + 大小覆盖其语义）。
    ///
    /// 三元组 =（逻辑列 id / 文案表键 / 列宽）：列 id 是表格列标识（逻辑键，
    /// 不翻译），列名在 [`TableDelegate::column`] 内经 `crate::tr` 查表。
    const COLUMNS: [(&'static str, &'static str, f32); 13] = [
        // 行内「详情」图标钮（用户口径 2026-10-09）：点它弹 modal 展示该任务
        // 全量字段。列窄、无数据语义 ⇒ 排在任务列**前方**第一位。
        ("detail", "jobs.col_detail", 56.),
        ("title", "jobs.col_title", 220.),
        ("input", "jobs.col_input", 160.),
        ("output", "jobs.col_output", 160.),
        ("action", "jobs.col_action", 64.),
        ("format", "jobs.col_format", 64.),
        ("started", "jobs.col_started", 88.),
        ("elapsed", "jobs.col_elapsed", 72.),
        ("size", "jobs.col_size", 88.),
        ("progress", "jobs.col_progress", 170.),
        ("state", "jobs.col_state", 96.),
        // 失败原因**独立成列**（用户口径 2026-09-24）：此前把失败行挂在行根
        // 上渲染，单元格被挤到后面 ⇒ 整行各列内容错位；现在原因有自己的列，
        // 列宽可拖、列序可调（TableDelegate 既有能力）。
        ("failure", "jobs.col_failure", 220.),
        ("ops", "jobs.col_ops", 72.),
    ];
}

impl TableDelegate for JobsTableDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        Self::COLUMNS.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let key = self.column_order[col_ix];
        let (_, name_key, default_w) = Self::COLUMNS
            .iter()
            .find(|(k, _, _)| *k == key)
            .unwrap_or(&Self::COLUMNS[0]);
        let width = self
            .widths
            .iter()
            .find(|(c, _)| *c == key)
            .map(|(_, w)| *w)
            .unwrap_or(px(*default_w));
        Column::new(key, crate::tr(name_key))
            .width(width)
            .min_width(JOB_COL_MIN_W)
            .max_width(width * 2.)
    }

    // 单元格渲染（R-GUI4 #4）：状态列 = 语义色圆点 + 文字；进度列 Progress +
    // 百分比；操作列取消按钮仅活跃任务（终态空）；其余文本列 muted 色。
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let row = &self.rows[row_ix];
        // 行为/状态文案由 binding 层查表产出（`String`）；此处绑定一次 `&str`，
        // 供闭包与单元格渲染复用（避免逐处 clone）。
        let action_text: &str = &row.action_text;
        let state_label: &str = &row.state_label;
        let muted = cx.theme().muted_foreground;
        let text = |v: &str| {
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(v.to_string()))
        };
        // 列序可拖：col_ix 是当前可见序——换算回逻辑列 key 再分流
        let key = self.column_order[col_ix];
        let content: AnyElement = match key {
            // 详情钮（用户口径 2026-10-09）：点它弹 modal 展示该任务全量字段。
            // 事件闭包自带 `&mut Window + &mut App`（kit 点击签名）⇒ 弹窗就地开，
            // 无需 shell 转发；行快照取源行（`src_rows`）而非派生行。
            "detail" => {
                let button = Button::new(("job-detail", row.job_id))
                    .debug_selector({
                        let id = row.job_id;
                        move || format!("job-detail-{id}")
                    })
                    .xsmall()
                    .ghost()
                    .icon(AppIcon::Info)
                    .tooltip(crate::tr("jobs.detail_title"));
                match self.src_rows.get(row_ix) {
                    Some(src) => {
                        let source = src.clone();
                        button
                            .on_click(move |_, window, cx| {
                                crate::ui::dialogs::open_job_detail_dialog(
                                    window,
                                    cx,
                                    source.clone(),
                                );
                            })
                            .into_any_element()
                    }
                    // 源行缺失（行集与派生行瞬时错位）→ 只画钮，不开空弹窗
                    None => button.into_any_element(),
                }
            }
            "title" => div()
                .text_sm()
                .overflow_x_hidden()
                .child(SharedString::from(row.title.clone()))
                .into_any_element(),
            "input" => text(&row.input_text).into_any_element(),
            "output" => text(&row.output_text).into_any_element(),
            "action" => text(action_text).into_any_element(),
            "format" => text(&row.format_text).into_any_element(),
            "started" => text(&row.started_text).into_any_element(),
            "elapsed" => text(&row.elapsed_text).into_any_element(),
            "size" => text(&row.size_text).into_any_element(),
            "progress" => {
                h_flex()
                    .gap(sp(1.))
                    .items_center()
                    .child(
                        // 队列中（progress==0）无意义百分比 → indeterminate 滑动动画
                        Progress::new(("job-progress", row_ix))
                            .value(row.progress.unwrap_or(0.0) * 100.)
                            .loading(is_indeterminate(row))
                            .w(JOB_PROGRESS_W),
                    )
                    .child(div().text_xs().text_color(state_color(cx, row)).child(
                        SharedString::from(match row.progress {
                            Some(p) => format!("{:.0}%", p * 100.0),
                            // 未知总量：显示「…」（与进度窗同款），不冒充 0%
                            None => "…".to_string(),
                        }),
                    ))
                    .into_any_element()
            }
            "state" => h_flex()
                .gap(sp(1.))
                .items_center()
                // 语义色圆点（T-GUI T23：Badge dot 替代手绘 rounded_full；
                // Badge 是 absolute 定位——需要 relative 容器包裹）
                .child(
                    div()
                        .flex_shrink_0()
                        .relative()
                        .child(Badge::new().dot().color(state_color(cx, row))),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(state_color(cx, row))
                        .child(SharedString::from(state_label)),
                )
                .into_any_element(),
            "failure" => {
                // 失败原因（**整档失败原文** + 逐条目失败明细；空 = 无失败）。
                //
                // 用户口径 2026-09-29：重命名档案保存失败后「任务中心看不出来具体
                // 失败原因」——整档失败的原因在 `final_line` 里（此前只渲染
                // `failures`，逐条目明细对编辑/整档类任务是空的 ⇒ 单元格空白）。
                let mut lines: Vec<(String, String)> = Vec::new();
                if let Some(text) = &row.final_line {
                    lines.push((String::new(), text.clone()));
                }
                lines.extend(row.failures.clone());
                if lines.is_empty() {
                    div().into_any_element()
                } else {
                    let danger = cx.theme().danger;
                    v_flex()
                        .gap(sp(0.5))
                        .text_xs()
                        .text_color(danger)
                        .overflow_x_hidden()
                        .children(lines.into_iter().map(|(entry, message)| {
                            // message 通常是 i18n key（`Error::i18n_key()`），也可能是
                            // 人读原文（打开记录的失败原因 / 带 HZ 码的整档失败）——
                            // `tr` 对非 key 输入原样返回
                            let msg = crate::tr(&message);
                            div().overflow_x_hidden().child(SharedString::from(
                                if entry.is_empty() {
                                    msg
                                } else {
                                    format!("{entry}: {msg}")
                                },
                            ))
                        }))
                        .into_any_element()
                }
            }
            "ops" => {
                // 取消按钮仅活跃任务（终态空——R-GUI4 #4）
                if row.is_active {
                    // 上行通道：与工具栏「全部取消」同一条（shell → AppData）。
                    // 旧实现只画钮不接 on_click ⇒ 点了没反应（用户实报 2026-10-09）。
                    let job_id = row.job_id;
                    let on_batch = self.on_batch.clone();
                    Button::new(SharedString::from(format!("job-cancel-{job_id}")))
                        .debug_selector({
                            let id = job_id;
                            move || format!("job-cancel-{id}")
                        })
                        .with_size(component_size())
                        .label(crate::tr("jobs.cancel"))
                        .with_size(Size::XSmall)
                        .on_click(move |_, _, cx| {
                            if let Some(on_batch) = &on_batch {
                                on_batch(DialogEvent::JobsCancel(job_id), cx);
                            }
                        })
                        .into_any_element()
                } else {
                    div().into_any_element()
                }
            }
            _ => div().into_any_element(),
        };
        // 统一垂直居中（见 [`cell_shell`]）：各列字号不同，不套壳时小字号列看起来
        // 「靠上」——用户实报 2026-10-09「任务列居中、其他列都没居中」。
        cell_shell(row_ix, key, content)
    }

    // 失败下钻（展开区：失败条目 i18n key 直出）——手拼保留（应用特定布局）。
    //
    // 本元素即**行根**（kit `render_tr` 返回值上再挂 h_flex/单元格/选中底色），
    // 故选中底色在此自绘：与文件列表同源（`selection_row_bg`：背景 × 30%
    // 主题色，比主题色浅、深色模式自适应）。kit 在 `list.active_highlight=false`
    // 下对选中行画的是**全量 accent**（0.7.1 `table/state.rs:2242-2250`：
    // `.when(is_selected, …)` 取 `tokens.accent`，`is_selected` 判定见 `:2012`）
    // ——绝对定位子层盖掉，单元格是 kit 后追加的 child，故画在它们之下
    // （同浏览表格 `render_tr` 手法）。
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        // 选中镜像（事件订阅回写；渲染期不可重入读 TableState——见字段注释）
        let selected = self.selected_row == Some(row_ix);
        let mut row = div()
            .id(("row", row_ix))
            // 测试锚点（仅测试态）：单元格垂直居中用例要对齐行盒上沿/下沿
            .when(cfg!(test), |d| {
                d.debug_selector(move || format!("job-row-{row_ix}"))
            });
        if selected {
            row = row.relative().child(
                div()
                    .absolute()
                    .inset_0()
                    .bg(crate::ui::theme::selection_row_bg(cx))
                    .debug_selector(|| "job-row-selected-layer".into()),
            );
        }
        // 失败明细**不再挂在行根**上：行根上的子元素会被表布局当成额外单元格，
        // 把后续列整体挤后一位（用户实测 2026-09-24「各列内容位置错乱」）。
        // 现在失败原因有自己的列（`failure`），行根只画选中底色。
        row
    }

    /// 列头拖动换序（2026-09-15 用户要求；与浏览表格同款委托钩子）。
    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _window: &mut Window,
        _cx: &mut gpui_kit::Context<TableState<Self>>,
    ) {
        if col_ix == to_ix || col_ix >= self.column_order.len() {
            return;
        }
        let to_ix = to_ix.min(self.column_order.len() - 1);
        let key = self.column_order[col_ix];
        self.column_order.remove(col_ix);
        self.column_order.insert(to_ix, key);
    }
}

/// 单元格壳（与浏览表格 `browse.rs::render_td` 的 `shell` 同款）：
/// `h_full + items_center` ⇒ 内容在**行盒内垂直居中**。
///
/// 为什么必需（用户实报 2026-10-09：「任务列内容垂直居中，其他列都没居中、
/// 有点靠上」）：kit `render_cell` 是 `h_full` 的**块容器**，单元格内容默认
/// **顶对齐**；本表各列字号不一（标题 `text_sm`、其余 `text_xs`）⇒ 行框更矮的
/// 小字号列墨迹更贴顶，看上去「没居中」。套壳后内容盒撑满行高再居中，全表一致。
///
/// 测试锚点：`job-cell-<row>-<逻辑列 key>`（`cfg!(test)` 才挂，发布零开销）
/// ——见 `views/tests/jobs.rs::all_cells_are_vertically_centered_in_row`。
fn cell_shell(row_ix: usize, key: &'static str, content: AnyElement) -> AnyElement {
    h_flex()
        .h_full()
        .items_center()
        .child(content)
        .when(cfg!(test), |d| {
            d.debug_selector(move || format!("job-cell-{row_ix}-{key}"))
        })
        .into_any_element()
}

/// 状态语义色（渲染侧决策；binding 层只出语义分组，T-GUI T02）。
fn state_color(cx: &App, row: &JobsRowUi) -> gpui_kit::Hsla {
    let theme = cx.theme();
    match row.state_semantic {
        JobStateSemantic::Neutral => theme.muted_foreground,
        JobStateSemantic::Accent => theme.primary,
        JobStateSemantic::Warning => theme.warning,
        JobStateSemantic::Danger => theme.danger,
        JobStateSemantic::Success => theme.success,
    }
}

/// indeterminate 判定：队列中（无进度快照）→ 无限滑动动画。
///
/// 以**语义分组**判定（Queued 是唯一 Neutral 态）而非比中文字面量——
/// 状态标签接入 i18n 后随 locale 变化，字符串比较会失效。
fn is_indeterminate(row: &JobsRowUi) -> bool {
    row.state_semantic == JobStateSemantic::Neutral
}

/// 任务中心稳态句柄（Global 单例；R-GUI4）。
///
/// `TableState` 跨帧复用（选中/滚动/列宽列序全保持）；行数据**只在源行集变化时**
/// 重灌为派生行（`delegate.rows`）——**不 `refresh`**（那会按 `delegate.column()`
/// 重建 col_groups，把拖动中的列宽拽回，见 [`jobs_hold`]）。10 Hz 泵刷新期不再
/// 整表重建。
pub(crate) struct JobsHold {
    /// 任务表格交互态（选中/滚动）
    pub(crate) table: Entity<TableState<JobsTableDelegate>>,
    /// 派生行缓存：键 = 源行集 `Arc` 身份（见 [`jobs_hold`]）。
    ///
    /// **每窗口一份**（挂在 Global 上，随 `App` 生命周期）：进程级 `static` 会让
    /// 两个窗口互相顶掉缓存条目 ⇒ 每帧都 miss ⇒ 派生开销（50 行 × 12 列 × 每格
    /// 数次字符串分配）原样回来，还会让并发渲染的用例互相干扰。
    derived_src: std::sync::Arc<Vec<crate::viewmodel::JobRow>>,
    /// 订阅保活（T-PW5 双击重开订阅）
    _subscriptions: Vec<gpui_kit::Subscription>,
}

/// Global 存储壳（Global 绑定在值类型上——包一层持 Entity 句柄）。
struct JobsGlobal(Entity<JobsHold>);

impl gpui_kit::Global for JobsGlobal {}

/// 双击活跃行重开进度窗口回调（T-PW5；shell 层装配——AppData 触碰收口）。
pub type ReopenHandler = std::rc::Rc<dyn Fn(u64, &mut App)>;

/// 取稳态句柄（不存在则创建；创建需 Window——TableState 构造）。
fn jobs_hold(
    window: &mut Window,
    cx: &mut App,
    job_rows: &std::sync::Arc<Vec<crate::viewmodel::JobRow>>,
    on_reopen: Option<ReopenHandler>,
    on_batch: Option<BatchHandler>,
) -> Entity<JobsHold> {
    if cx.has_global::<JobsGlobal>() {
        let hold = cx.global::<JobsGlobal>().0.clone();
        // 派生行（i18n + 格式化）按**源行集的 `Arc` 身份**缓存：未变 → 连克隆都
        // 省掉（旧实现在此处无条件重建 + 重灌 + `refresh`，后者每帧把拖动中的
        // 列宽拽回默认值——用户实测 2026-09-29「任务中心列宽拖不动」）。
        let (rows, rows_changed): (std::sync::Arc<Vec<JobsRowUi>>, bool) = {
            let h = hold.read(cx);
            if std::sync::Arc::ptr_eq(&h.derived_src, job_rows) {
                // 命中：派生结果就在委托里（同一 `Arc`），原样复用
                (h.table.read(cx).delegate().rows.clone(), false)
            } else {
                (std::sync::Arc::new(derive_job_rows(job_rows)), true)
            }
        };
        hold.update(cx, |hold, _| hold.derived_src = job_rows.clone());
        // 行数据重灌（跨帧复用表格实体——滚动/选中保持）。
        //
        // **绝不在这里 `refresh()`**（用户实测 2026-09-29「任务中心列宽拖不动」）：
        // `TableState::refresh` = `prepare_col_groups`——按 `delegate.column()`
        // 逐个**重建** col_groups，把拖动中的列宽拽回委托里的旧值；本表格每个
        // 渲染帧都会走到这里，于是拖动一帧都活不过。行数据不参与 col_groups，
        // 重灌无需重建；列宽只在鼠标松开时经 `ColumnWidthsChanged` 落回委托
        // （会话内保持），列序同理已由 `move_column` 落进委托。
        if rows_changed {
            hold.update(cx, |hold, cx| {
                hold.table.update(cx, |table, cx| {
                    table.delegate_mut().rows = rows.clone();
                    // 源行同批跟上（详情弹窗按 row_ix 取原始行）
                    table.delegate_mut().src_rows = job_rows.clone();
                    cx.notify();
                });
            });
        }
        return hold;
    }
    let column_order = JobsTableDelegate::COLUMNS
        .iter()
        .map(|(k, _, _)| *k)
        .collect();
    let rows = std::sync::Arc::new(derive_job_rows(job_rows));
    let delegate = JobsTableDelegate {
        rows,
        src_rows: job_rows.clone(),
        on_batch,
        column_order,
        widths: Vec::new(),
        selected_row: None,
    };
    let table = cx.new(|cx| {
        TableState::new(delegate, window, cx)
            // 行可选（T-PW5：双击活跃行 → 重开进度窗口）；列宽/列序可拖
            //（2026-09-15 用户要求，与浏览表格同款）
            .row_selectable(true)
            .col_selectable(false)
            .sortable(false)
            .col_movable(true)
            .col_resizable(true)
    });
    let hold = cx.new(|_| JobsHold {
        table,
        derived_src: job_rows.clone(),
        _subscriptions: Vec::new(),
    });
    // 事件订阅（Global 复用期保活）：
    // - `SelectRow` → 回写选中行镜像（`render_tr` 自绘选中底色用；渲染期
    //   不可重入读 TableState，只能经事件镜像）；
    // - `DoubleClickedRow`（T-PW5 重开入口）：活跃任务行 → 窗口已关则重开
    //   （回调经 `on_reopen` 上行 shell 层——AppData 触碰收口 07 §4.4）。
    hold.update(cx, |hold, cx| {
        let table = hold.table.clone();
        hold._subscriptions.push(cx.subscribe(
            &table,
            move |_hold: &mut JobsHold,
                  table,
                  event: &TableEvent,
                  cx: &mut gpui_kit::Context<JobsHold>| {
                match event {
                    TableEvent::SelectRow(ix) => {
                        let ix = *ix;
                        table.update(cx, |t, _| t.delegate_mut().selected_row = Some(ix));
                    }
                    // 拖动列宽 → 委托 `widths` 回存（会话内保持）。
                    //
                    // kit 在**鼠标松开**时上抛全量列宽；此后任何 `refresh()`
                    // （col_groups 重建）都按 `delegate.column()` 回填 —— 即拖过的
                    // 宽度不会被重置回默认值（列序同理由 `move_column` 落账）。
                    TableEvent::ColumnWidthsChanged(ws) => {
                        let order = table.read(cx).delegate().column_order.clone();
                        table.update(cx, |t, _| {
                            t.delegate_mut().widths =
                                order.into_iter().zip(ws.iter().copied()).collect();
                        });
                    }
                    TableEvent::DoubleClickedRow(ix) => {
                        let Some(on_reopen) = on_reopen.clone() else {
                            return;
                        };
                        let row = table.read(cx).delegate().rows.get(*ix);
                        if let Some(row) = row
                            && row.is_active
                        {
                            on_reopen(row.job_id, cx);
                        }
                    }
                    _ => {}
                }
            },
        ));
    });
    cx.set_global(JobsGlobal(hold.clone()));
    hold
}

/// 源行集 → 派生 UI 行（i18n + 格式化；缓存未命中时调用）。
fn derive_job_rows(src: &std::sync::Arc<Vec<crate::viewmodel::JobRow>>) -> Vec<JobsRowUi> {
    src.iter().map(crate::ui::binding::jobs_row_ui).collect()
}

/// 批量操作上行回调（DialogEvent + gpui App 现场调用权）。
pub type BatchHandler = std::rc::Rc<dyn Fn(DialogEvent, &mut App)>;

/// 任务中心根渲染（T-G10 内容插槽按 `MainTab::Jobs` 分支调用）。
///
/// `on_batch`：批量操作上行通道（T-GUI T31；AppShell 侧经
/// `Rc<dyn Fn(DialogEvent)>` 注入 `on_dialog_event` 链——Box 不可 Clone，
/// 闭包需 'static，故以 Rc 显式传参）。
/// `on_reopen`：双击活跃任务行重开进度窗口（T-PW5 可选；None = 不订阅）。
pub fn jobs_root(
    cx: &mut App,
    window: &mut Window,
    snap: &AppStateUiSnapshot,
    handlers: &ShellHandlers,
    on_batch: BatchHandler,
    on_reopen: Option<ReopenHandler>,
) -> AnyElement {
    // 派生行（i18n + 格式化）按**源行集的 `Arc` 身份**缓存（2026-09-24 审 ⑲）：
    // 旧实现每次渲染都全量重建（50 行 × 12 列 × 每格 3~5 次字符串分配），
    // 60 fps 下就是稳定可见的帧内开销。行集是写时复制 `Arc`，未变时 `ptr_eq`
    // 相等 ⇒ 直接复用上次派生结果。
    // 批量操作行（右上：全部取消 / 清除已完成）
    let ops = h_flex()
        .flex_shrink_0()
        .justify_end()
        .gap(sp(2.))
        .px(sp(2.))
        .py(sp(1.))
        .child(
            Button::new("jobs-cancel-all")
                .with_size(component_size())
                .label(crate::tr("jobs.cancel_all"))
                .with_size(Size::Small)
                .on_click({
                    // T-GUI T31：经 on_dialog_event 链上行 → AppData 消费
                    //（cancel_all 逐活跃任务幂等取消）
                    let on = on_batch.clone();
                    move |_, _, cx| on(DialogEvent::JobsCancelAll, cx)
                }),
        )
        .child(
            Button::new("jobs-clear-done")
                .with_size(component_size())
                .label(crate::tr("jobs.clear_finished"))
                .with_size(Size::Small)
                .on_click({
                    let on = on_batch.clone();
                    move |_, _, cx| on(DialogEvent::JobsClearFinished, cx)
                }),
        );

    // 稳态表格（Global 句柄；行数据每帧重灌——刷新由泵信号驱动 cx.notify）
    let hold = jobs_hold(
        window,
        cx,
        &snap.job_rows,
        on_reopen,
        Some(on_batch.clone()),
    );
    let table = DataTable::new(&hold.read(cx).table.clone())
        .with_size(component_size())
        .bordered(true)
        .stripe(false)
        .scrollbar_visible(true, false);

    v_flex()
        .size_full()
        .bg(cx.theme().background)
        .text_color(cx.theme().foreground)
        .child(ops)
        .child(div().flex_1().min_h_0().child(table))
        .child({
            let _ = handlers;
            div()
        })
        .into_any_element()
}

#[cfg(test)]
#[path = "tests/jobs.rs"]
mod tests;
