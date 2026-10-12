use super::{JobsGlobal, jobs_root};
use crate::ui::binding::jobs_row_ui;
use crate::viewmodel::{JobRow, JobRowState};
use gpui_kit::component::table::{TableDelegate, TableEvent};
use gpui_kit::{IntoElement, Window};

/// 选中任务行画**统一选中色层**（与文件列表同色——kit 在
/// `active_highlight=false` 下画的是全量 accent；0.7.1 位置见
/// `table/state.rs:2242-2250` 的 `.when(is_selected, …)`，`is_selected` 判定在
/// `:2012`）。
///
/// 边界探针双向：未选中无该层；`set_selected_row` 后该层出现。
#[gpui_kit::test]
fn selected_job_row_paints_unified_selection_layer(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::{AppStateUiSnapshot, ShellHandlers};

    struct Host;
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            let snap = AppStateUiSnapshot {
                job_rows: std::sync::Arc::new(vec![row(JobRowState::Done)]),
                ..Default::default()
            };
            jobs_root(
                cx,
                window,
                &snap,
                &ShellHandlers::default(),
                std::rc::Rc::new(|_, _| {}),
                None,
            )
        }
    }

    let (host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        Host
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("job-row-selected-layer").is_none(),
        "未选中行不得有选中层"
    );

    let table = vtcx.update(|_window, cx| cx.global::<JobsGlobal>().0.read(cx).table.clone());
    vtcx.update(|_window, cx| {
        table.update(cx, |t, cx| t.set_selected_row(0, cx));
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("job-row-selected-layer").is_some(),
        "选中任务行必须画出统一选中色层（否则退回 kit 的全量 accent）"
    );
    drop(host);
}

/// 列宽拖动（用户实测 2026-09-29「任务中心列宽拖不动」）。
///
/// kit 在**鼠标松开**时上抛 `ColumnWidthsChanged`（全量列宽）——此前本表格
/// 不订阅该事件，`delegate.widths` 永远是空的：`col_groups` 一旦重建（`refresh`）
/// 就按 `column()` 回到默认宽。本用例锚住「拖动值落进委托 + `refresh` 后仍是
/// 拖动值」这条契约（修复前 `widths` 为空 ⇒ 断言必失败）。
#[gpui_kit::test]
fn column_widths_changed_persists_into_delegate(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::{AppStateUiSnapshot, ShellHandlers};

    struct Host;
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            let snap = AppStateUiSnapshot {
                job_rows: std::sync::Arc::new(vec![row(JobRowState::Done)]),
                ..Default::default()
            };
            jobs_root(
                cx,
                window,
                &snap,
                &ShellHandlers::default(),
                std::rc::Rc::new(|_, _| {}),
                None,
            )
        }
    }

    let (host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        Host
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let table = vtcx.update(|_window, cx| cx.global::<JobsGlobal>().0.read(cx).table.clone());

    // 模拟 kit 拖动结束：第 0 列（详情钮列）56 → 360（其余保持原值）
    let mut ws: Vec<gpui_kit::Pixels> = super::JobsTableDelegate::COLUMNS
        .iter()
        .map(|(_, _, w)| gpui_kit::px(*w))
        .collect();
    ws[0] = gpui_kit::px(360.);
    vtcx.update(|_window, cx| {
        table.update(cx, |_t, cx| {
            cx.emit(TableEvent::ColumnWidthsChanged(ws.clone()));
        });
    });
    let (key0, w0) = vtcx.update(|_window, cx| {
        let d = table.read(cx).delegate();
        (d.column_order[0], d.column(0, cx).width)
    });
    assert_eq!(key0, "detail", "列序基线（详情钮列排首位）");
    assert_eq!(
        w0,
        gpui_kit::px(360.),
        "拖动结束的列宽必须落进委托（否则 col_groups 一重建就弹回默认值）"
    );
    // col_groups 重建（refresh）后仍是拖动值 —— 「会话内保持」的契约
    vtcx.update(|_window, cx| {
        table.update(cx, |t, cx| t.refresh(cx));
    });
    let w0_after = vtcx.update(|_window, cx| table.read(cx).delegate().column(0, cx).width);
    assert_eq!(
        w0_after,
        gpui_kit::px(360.),
        "refresh（col_groups 重建）后不得弹回默认列宽"
    );
    drop(host);
}

/// 行数据未变时**不重灌**（列宽拖动失效的根因半边）。
///
/// 复用分支此前每帧无条件 `delegate.rows = rows; table.refresh(cx)`——`refresh`
/// = `prepare_col_groups` = 按 `column()` 重建列宽 ⇒ 拖动中的列宽每帧被拽回。
/// 现在只在源行集 `Arc` 身份变化时重灌，且**不 refresh**（行数据不参与
/// col_groups），列宽是拖动值就一直是拖动值。
#[gpui_kit::test]
fn unchanged_rows_skip_reload(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::{AppStateUiSnapshot, ShellHandlers};

    /// 宿主快照跨帧复用**同一 `job_rows` Arc**（缓存键身份稳定）。
    struct Host {
        jobs: std::sync::Arc<Vec<JobRow>>,
    }
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            let snap = AppStateUiSnapshot {
                job_rows: self.jobs.clone(),
                ..Default::default()
            };
            jobs_root(
                cx,
                window,
                &snap,
                &ShellHandlers::default(),
                std::rc::Rc::new(|_, _| {}),
                None,
            )
        }
    }

    let src_done = std::sync::Arc::new(vec![row(JobRowState::Done)]);
    let (host, vtcx) = cx.add_window_view({
        let jobs = src_done.clone();
        move |_window, cx| {
            gpui_kit::component::init(cx);
            crate::ui::theme::init(cx);
            Host { jobs }
        }
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let table = vtcx.update(|_window, cx| cx.global::<JobsGlobal>().0.read(cx).table.clone());

    // 先把第 0 列「拖宽」（kit 松开鼠标时上抛 → 订阅回存委托）
    vtcx.update(|_window, cx| {
        let mut ws: Vec<gpui_kit::Pixels> = super::JobsTableDelegate::COLUMNS
            .iter()
            .map(|(_, _, w)| gpui_kit::px(*w))
            .collect();
        ws[0] = gpui_kit::px(300.);
        table.update(cx, |_t, cx| {
            cx.emit(TableEvent::ColumnWidthsChanged(ws.clone()));
        });
    });

    let (ptr_before, w_before) = vtcx.update(|_window, cx| {
        let t = table.read(cx);
        (t.delegate().rows.as_ptr(), t.delegate().column(0, cx).width)
    });
    assert_eq!(w_before, gpui_kit::px(300.), "拖动值先落账");
    // 同一源行集（身份未变）再走一次复用分支 → 行集不换、宽度不动
    let (ptr_same, w_same) = vtcx.update(|window, cx| {
        super::jobs_hold(window, cx, &src_done, None, None);
        let t = table.read(cx);
        (t.delegate().rows.as_ptr(), t.delegate().column(0, cx).width)
    });
    assert_eq!(ptr_before, ptr_same, "源行集未变 → 不重灌（同一 `Arc`）");
    assert_eq!(w_same, gpui_kit::px(300.), "不重灌不得改动列宽");

    // 换一源行集（身份变化）→ 重灌为新派生 `Arc`，列宽仍保持
    let src_run = std::sync::Arc::new(vec![row(JobRowState::Running)]);
    let (ptr_new, w_new, changed) = vtcx.update(|window, cx| {
        super::jobs_hold(window, cx, &src_run, None, None);
        let t = table.read(cx);
        (
            t.delegate().rows.as_ptr(),
            t.delegate().column(0, cx).width,
            t.delegate().rows.len(),
        )
    });
    assert!(changed == 1, "新行集已重灌");
    assert_ne!(ptr_before, ptr_new, "源行集变化 → 换新的派生 `Arc`");
    assert_eq!(
        w_new,
        gpui_kit::px(300.),
        "重灌行数据不得改动列宽（拖过的宽度会话内保持）"
    );
    drop(host);
}

// —— R-GUI4 #4：状态 → 圆点语义色映射（标签全枚举覆盖）——

fn row(state: JobRowState) -> JobRow {
    JobRow {
        job_id: 1,
        kind: "extract",
        state,
        progress: Some(0.5),
        speed_bps: 1024.0 * 118.0,
        eta: Some(std::time::Duration::from_secs(32)),
        done_bytes: 0,
        total_bytes: 0,
        entries_done: 0,
        entries_total: 0,
        current_entry: None,
        final_line: None,
        recent_entries: std::collections::VecDeque::new(),
        failures: Vec::new(),
        finished: None,
        src: "demo.zip".into(),
        dest: None,
        format: "zip".into(),
        started_at: None,
        elapsed: None,
    }
}

// 8 态全覆盖：标签非空 + 取消按钮显隐（活跃 4 态 true / 终态 4 态 false）。
#[test]
fn job_state_label_and_cancel_visibility_covers_all() {
    let cases: [(JobRowState, &str, bool); 8] = [
        (JobRowState::Queued, "队列中", true),
        (JobRowState::Running, "运行中", true),
        (JobRowState::WaitingInput, "等待输入", true),
        (JobRowState::Cancelling, "取消中", true),
        (JobRowState::Done, "成功", false),
        (JobRowState::PartialDone, "部分完成", false),
        (JobRowState::Failed, "失败", false),
        (JobRowState::Cancelled, "已取消", false),
    ];
    for (state, want_label, want_active) in cases {
        let ui = jobs_row_ui(&row(state));
        assert_eq!(ui.state_label.as_str(), want_label, "{state:?} 标签");
        assert_eq!(ui.is_active, want_active, "{state:?} 取消按钮显隐");
        assert!(!ui.state_label.is_empty());
    }
}

// indeterminate 不确定态：仅队列中（无进度快照）触发 loading 动画。
#[test]
fn indeterminate_only_for_queued() {
    assert!(super::is_indeterminate(&jobs_row_ui(&row(
        JobRowState::Queued
    ))));
    assert!(!super::is_indeterminate(&jobs_row_ui(&row(
        JobRowState::Running
    ))));
    assert!(!super::is_indeterminate(&jobs_row_ui(&row(
        JobRowState::Done
    ))));
}

// 标题 = 行为 + 源对象名（R-GUI4 #4：明确任务对象）；输入/输出/格式投影
#[test]
fn job_row_title_composes_action_and_source() {
    let r = row(JobRowState::Running);
    let ui = jobs_row_ui(&r);
    assert_eq!(ui.title, "解压 demo.zip", "title={}", ui.title);
    assert_eq!(ui.input_text, "demo.zip");
    assert_eq!(ui.output_text, "—", "dest=None → 占位");
    assert_eq!(ui.format_text, "zip");
    assert_eq!(ui.action_text, "解压");
    assert_eq!(ui.elapsed_text, "—", "未开始 → 占位");
    assert_eq!(ui.started_text, "—");
}

// 10 Hz 节流语义：合成高频 Bytes → 冲刷次数远小于事件数（T42 用例平移）。
// Bytes{delta} 无时间戳；WINDOW_MS 门控在快速循环内必然合并。
#[test]
fn throttle_10hz_merged_signals() {
    use hzip_progress::{ProgressEvent, Throttle};
    let mut throttle = Throttle::new();
    let started = ProgressEvent::JobStarted {
        job_id: 1,
        total_bytes: 2_000_000,
        total_entries: 4,
    };
    assert!(throttle.push(&started).is_some(), "JobStarted 首冲必发");
    let mut flushes = 0usize;
    let events = 2000usize;
    for _ in 0..events {
        let ev = ProgressEvent::Bytes { delta: 100 };
        if throttle.push(&ev).is_some() {
            flushes += 1;
        }
    }
    assert!(
        flushes * 100 <= events / 10,
        "10 Hz 节流应把 {events} 次高频事件合并（实测冲刷 {flushes} 次）"
    );
}

/// 单元格内容**垂直居中**（用户实报 2026-10-09：「任务列是垂直居中的，其他列都
/// 没居中、有点靠上」）。
///
/// 根因：kit `render_cell` 是 `h_full` 的**块容器**，内容默认顶对齐；本表各列
/// 字号不一（标题 `text_sm`、其余 `text_xs`）⇒ 行框更矮的小字号列墨迹更贴顶。
/// 修复 = 单元格统一套壳 [`super::cell_shell`]（`h_full + items_center`）。
///
/// 断言口径：每个逻辑列都有 `job-cell-0-<key>` 壳、壳的竖直中心与行盒中心一致、
/// 各列中心彼此一致（去掉壳 ⇒ 锚点不存在，用例直接失败 = 回归锚）。
#[gpui_kit::test]
fn all_cells_are_vertically_centered_in_row(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::{AppStateUiSnapshot, ShellHandlers};
    use gpui_kit::IntoElement;

    struct Host;
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            let snap = AppStateUiSnapshot {
                job_rows: std::sync::Arc::new(vec![row(JobRowState::Running)]),
                ..Default::default()
            };
            jobs_root(
                cx,
                window,
                &snap,
                &ShellHandlers::default(),
                std::rc::Rc::new(|_, _| {}),
                None,
            )
        }
    }

    let (_host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        Host
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let row_box = vtcx.debug_bounds("job-row-0").expect("行盒锚点");
    let row_center = row_box.center().y;
    let near = |a: gpui_kit::Pixels, b: gpui_kit::Pixels| {
        a > b - gpui_kit::px(1.) && a < b + gpui_kit::px(1.)
    };

    let cols = [
        "detail", "title", "input", "output", "action", "format", "started", "elapsed", "size",
        "progress", "state", "failure", "ops",
    ];
    let mut centers = Vec::new();
    for key in cols {
        // `debug_bounds` 收 `&'static str` —— 泄漏一个短字符串换取动态 selector
        //（测试态惯例，见 `views/tests/settings_layout.rs`）。
        let sel: &'static str = Box::leak(format!("job-cell-0-{key}").into_boxed_str());
        let cell = vtcx
            .debug_bounds(sel)
            .unwrap_or_else(|| panic!("{key} 列缺单元格壳（{sel}）"));
        assert!(
            near(cell.center().y, row_center),
            "{key} 列内容盒须在行盒内垂直居中（实得 {:?}，行中心 {:?}）",
            cell.center().y,
            row_center
        );
        centers.push(cell.center().y);
    }
    let first = centers[0];
    assert!(
        centers.iter().all(|y| near(*y, first)),
        "各列内容盒中心须彼此一致（实得 {centers:?}）"
    );
    drop(_host);
}
