use super::*;
use crate::viewmodel::JobRow;
use std::sync::{Arc, Mutex};

// 退出钩子：无活跃任务 → 立即返回（远小于 2 s 宽限）+ TempArea 清理断言
#[test]
fn graceful_shutdown_immediate_when_idle_and_cleans_temp() {
    let engine = Engine::new(Default::default()).unwrap();
    let jobs = Arc::new(Mutex::new(JobsViewModel::new()));
    let temp = TempArea::new_session(None).unwrap();
    let dir = temp.session_dir().to_path_buf();
    assert!(dir.exists());
    let start = std::time::Instant::now();
    graceful_shutdown(&engine, &jobs, Some(&temp));
    assert!(start.elapsed() < std::time::Duration::from_millis(500));
    let _ = std::fs::remove_dir_all(&dir); // cleanup 后残留目录兜底删除（Windows 句柄延迟）
}

// 退出钩子：有非终局活跃行 → 等待宽限收敛（≤ 2 s + 余量）
#[test]
fn graceful_shutdown_waits_within_grace_for_active() {
    let engine = Engine::new(Default::default()).unwrap();
    let jobs = Arc::new(Mutex::new(JobsViewModel::new()));
    jobs.lock().unwrap().register(JobRow {
        job_id: 1,
        kind: "extract",
        state: crate::viewmodel::JobRowState::Running,
        progress: None,
        speed_bps: 0.0,
        eta: None,
        done_bytes: 0,
        total_bytes: 0,
        entries_done: 0,
        entries_total: 0,
        failures: Vec::new(),
        finished: None,
        current_entry: None,
        final_line: None,
        recent_entries: std::collections::VecDeque::new(),
        src: "demo.zip".into(),
        dest: None,
        format: "zip".into(),
        started_at: None,
        elapsed: None,
    });
    // register 需 JobRow 全字段——用 is_terminal=false 的 Running 行
    let start = std::time::Instant::now();
    graceful_shutdown(&engine, &jobs, None);
    // 行不会被真实 worker 推进为终局（无 handle 事件）→ 等满宽限
    let elapsed = start.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_millis(1900)
            && elapsed < std::time::Duration::from_millis(2500),
        "宽限等待应落在 [1.9s, 2.5s)，实测 {elapsed:?}"
    );
}
