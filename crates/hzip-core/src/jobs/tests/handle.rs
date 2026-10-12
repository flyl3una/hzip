use super::*;
use crate::jobs::request::ConflictDecision;
use crate::jobs::{JobSlot, JobSpec, JobState, RequestId};
use std::sync::{Arc, Mutex};

/// 构造最小槽（只测 `answer` 的门禁，不起 worker）。
fn handle(state: JobState, pending: Option<RequestId>) -> JobHandle {
    let (req_tx, req_rx) = crossbeam_channel::bounded(1);
    let (ans_tx, ans_rx) = crossbeam_channel::bounded(1);
    let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);
    let pending_req = Arc::new(Mutex::new(pending));
    let bus = Arc::new(hzip_progress::ProgressBus::new());
    let slot = Arc::new(Mutex::new(JobSlot {
        state,
        spec: Arc::new(JobSpec::new(
            crate::jobs::JobKind::Test,
            std::path::PathBuf::from("x"),
        )),
        child: hzip_progress::CancellationToken::default(),
        bus: Arc::clone(&bus),
        req_tx,
        ans_tx: ans_tx.clone(),
        ans_rx: Some(ans_rx),
        outcome: None,
        done_tx,
        stats: hzip_progress::JobStats::new(0, 0),
        pending_req,
    }));
    JobHandle {
        job_id: 1,
        bus,
        req_rx,
        ans_tx,
        done_rx,
        slot,
    }
}

/// 竞态回归（2026-09-29）：请求已上抛、`WaitingInput` 还没落账的窗口里，
/// 应答**必须**被接受——只看状态会把用户刚输的密码当 stale 丢掉（任务
/// 然后干等 120 s，界面表现为「输了密码没反应」）。
#[test]
fn answer_accepted_during_waiting_input_window() {
    // 窗口内：状态还是 Running，但请求已登记
    let h = handle(JobState::Running, Some(0));
    assert!(
        h.answer(
            0,
            JobResponse::Password(zeroize::Zeroizing::new("pw".into()))
        )
        .is_ok(),
        "请求已登记时不得判 stale"
    );
    // 无待决请求 + 非 WaitingInput → 仍必须拒绝（防串到下一个问题）
    let h = handle(JobState::Running, None);
    assert!(
        h.answer(
            0,
            JobResponse::Password(zeroize::Zeroizing::new("pw".into()))
        )
        .is_err(),
        "无待决请求必须拒绝"
    );
    // 常规态（状态已落账）照旧放行
    let h = handle(JobState::WaitingInput, None);
    assert!(
        h.answer(0, JobResponse::Conflict(ConflictDecision::Skip))
            .is_ok()
    );
}
