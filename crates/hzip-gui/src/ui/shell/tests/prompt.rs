//! `prompt::handle_dialog_event`（views 上行总入口）测试。
//!
//! 用户实报 2026-10-09：任务中心「全部取消 / 清除已完成」点了没反应 ——
//! 事件确实上行到了这个函数，但旧实现只走请求-应答链（`answer_prompt` 对
//! 这两个既非密码也非冲突的事件落 `_ => {}`），**从未交给数据面**。
//! 本用例锚住「上行事件必须落 `AppData`」这条链。

use super::handle_dialog_event;
use crate::app::state::AppData;
use crate::ui::contracts::DialogEvent;
use crate::viewmodel::{JobRow, JobRowState};

/// 行（只关心 id/state：本用例断言行集增删）。
fn row(job_id: u64, state: JobRowState) -> JobRow {
    JobRow {
        job_id,
        kind: "extract",
        state,
        progress: None,
        speed_bps: 0.0,
        eta: None,
        done_bytes: 0,
        total_bytes: 0,
        entries_done: 0,
        entries_total: 0,
        failures: Vec::new(),
        finished: None,
        src: "demo.zip".into(),
        current_entry: None,
        final_line: None,
        recent_entries: std::collections::VecDeque::new(),
        dest: None,
        format: "zip".into(),
        started_at: None,
        elapsed: None,
    }
}

/// 装配：真引擎 + 预置行（`JobRow` 无句柄 ⇒ 取消类事件按契约静默）。
fn data_with_rows(rows: Vec<JobRow>) -> crate::ui::shell::AppDataHandle {
    let engine = std::sync::Arc::new(
        hzip_core::engine::Engine::new(hzip_core::engine::EngineConfig {
            cancel_on_signal: false,
            orphan_cleanup: false,
            ..Default::default()
        })
        .expect("engine"),
    );
    let data = AppData::new(engine);
    {
        let jobs = data.jobs.clone();
        let mut jobs = jobs.lock().expect("jobs 锁");
        for r in rows {
            jobs.register(r);
        }
    }
    std::sync::Arc::new(std::sync::Mutex::new(data))
}

fn row_ids(data: &crate::ui::shell::AppDataHandle) -> Vec<u64> {
    let jobs = data.lock().expect("state 锁").jobs.clone();
    let jobs = jobs.lock().expect("jobs 锁");
    jobs.rows().iter().map(|r| r.job_id).collect()
}

/// 「清除已完成」上行必须落数据面：终态行移除、活跃行保留。
///
/// 修复前：`handle_dialog_event` 只走应答链 ⇒ 行集原样 `[1, 2, 3]`（按钮
/// 点了毫无反应）。修复后：`[2]`（Running 保留）。
#[test]
fn clear_finished_event_reaches_data_plane() {
    let data = data_with_rows(vec![
        row(1, JobRowState::Done),
        row(2, JobRowState::Running),
        row(3, JobRowState::Cancelled),
    ]);
    assert_eq!(row_ids(&data), vec![1, 2, 3], "前置：三行齐备");

    handle_dialog_event(&data, &DialogEvent::JobsClearFinished);

    assert_eq!(
        row_ids(&data),
        vec![2],
        "终态行应被清掉、活跃行保留（未落数据面时行集不会变）"
    );
}

/// 「全部取消」上行**不动行集**：取消只下发令牌，行状态等 core 的
/// `JobFinished{Cancelled}` 落账（`JobsVm::cancel` 契约）。
#[test]
fn cancel_all_event_keeps_rows_until_terminal() {
    let data = data_with_rows(vec![
        row(1, JobRowState::Running),
        row(2, JobRowState::Queued),
    ]);

    handle_dialog_event(&data, &DialogEvent::JobsCancelAll);

    assert_eq!(
        row_ids(&data),
        vec![1, 2],
        "取消不删行（行由 core 终局落账）；未落数据面时同样如此 ⇒ 该断言锚的是「不得误删」"
    );
}

/// 密码/冲突应答等**既有**事件仍走原路（数据面追加不得改变其语义）。
#[test]
fn unrelated_events_leave_data_plane_untouched() {
    let data = data_with_rows(vec![row(1, JobRowState::Done)]);

    handle_dialog_event(&data, &DialogEvent::JobsCancel(4242));
    handle_dialog_event(&data, &DialogEvent::SettingsClose);

    assert_eq!(row_ids(&data), vec![1], "与本用例无关的事件不得动行集");
}
