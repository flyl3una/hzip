use super::*;
use crate::viewmodel::testutil;
use hzip_progress::JobStats;

fn row(job_id: JobId) -> JobRow {
    JobRow {
        job_id,
        kind: "test",
        state: JobRowState::Queued,
        progress: None,
        speed_bps: 0.0,
        eta: None,
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
        dest: Some("D:/out".into()),
        format: "zip".into(),
        started_at: None,
        elapsed: None,
    }
}

/// 会话密码入缓存的终局判定（用户口径 2026-09-29）：只有 Done/Partial 算成功；
/// 运行中要等；失败/取消/已清行一律不算成功（保守 → 不外泄没验过的密码）。
#[test]
fn verify_of_classifies_job_terminal() {
    let mut vm = JobsViewModel::new();
    assert_eq!(vm.verify_of(7), JobVerify::Gone, "未知 id");
    vm.register(row(7));
    assert_eq!(
        vm.verify_of(7),
        JobVerify::Active,
        "Queued/Running 等待终局"
    );
    vm.on_finished(7, &Outcome::Cancelled);
    assert_eq!(vm.verify_of(7), JobVerify::Failed, "取消 ≠ 密码正确");
    vm.register(row(8));
    vm.on_finished(
        8,
        &Outcome::Partial(
            hzip_progress::JobStats::new(2, 3),
            vec![hzip_progress::EntryFailure {
                entry: hzip_progress::EntryName::from("a.txt"),
                error: Box::new(HzipError::Password {
                    target: hzip_error::PasswordTarget::Entry,
                }),
            }],
        ),
    );
    assert_eq!(
        vm.verify_of(8),
        JobVerify::Succeeded,
        "部分成功 = 密码已验过"
    );
    vm.register(row(9));
    vm.on_finished(9, &Outcome::Done(hzip_progress::JobStats::new(1, 1)));
    assert_eq!(vm.verify_of(9), JobVerify::Succeeded);
    // 清行后不再有结论（保守：不入缓存）
    vm.clear_finished();
    assert_eq!(vm.verify_of(9), JobVerify::Gone);
}

fn snap(done: u64, total: u64) -> ProgressSnapshot {
    ProgressSnapshot {
        done,
        total,
        speed: Some(1234.0),
        eta: Some(hzip_progress::Eta::AlmostDone),
    }
}

/// 打开记录（用户口径 2026-09-24）：成功记 Done、失败记 Failed **带原因**；
/// 记录是终态、无句柄（不可取消）、不占活跃数（任务中心徽标）。
#[test]
fn record_open_marks_result_and_reason() {
    let mut vm = JobsViewModel::new();
    let ok: Result<(), String> = Ok(());
    let id_ok = vm.record_open("a.zip", "C:/tmp/a.zip", &ok);
    let row = vm.row(id_ok).expect("行已登记");
    assert_eq!(row.kind, "open");
    assert_eq!(row.state, JobRowState::Done);
    assert!(row.final_line.is_none(), "成功无失败原因");
    assert!(!vm.has_active(), "打开记录不占活跃任务数（无句柄/终态）");

    let err: Result<(), String> = Err("HZ-1002: IO 错误: 磁盘空间不足。 (C:/Temp)".into());
    let id_bad = vm.record_open("b.cpio.gz", "C:/tmp/b.cpio.gz", &err);
    let row = vm.row(id_bad).expect("行已登记");
    assert_eq!(row.state, JobRowState::Failed);
    let fl = row.final_line.as_ref().expect("失败必须带原因");
    assert!(fl.entry.is_empty(), "整体失败（非逐条目）→ entry 空");
    assert!(
        fl.message.contains("磁盘空间不足") && fl.message.contains("HZ-1002"),
        "原因必须是明确原文（含错误码）：{}",
        fl.message
    );
    assert!(row.src.contains("b.cpio.gz"), "输入列记录档案：{}", row.src);
    // id 命名空间不与真实任务冲突（真实 id 自 1 起）
    assert!(id_ok >= OPEN_ROW_ID_BASE && id_bad > id_ok);
}

#[test]
fn submit_registers_queued_row() {
    // 经真实 Engine 提交真压缩任务（tempfile 源 → tar）：登记 Queued 行
    // + handle 入表（cancel 可用）。真实事件流 → Done 的完整闭环由
    // `pump`/`state_transitions_legal`/`on_finished` 单测覆盖（本测试只
    // 验 submit 登记语义——真实压缩执行在 Engine 后台线程异步完成）。
    let d = tempfile::TempDir::new().unwrap();
    let src = d.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.txt"), vec![7u8; 4096]).unwrap();
    let out = d.path().join("out.tar");
    let eng = testutil::engine();
    let mut vm = JobsViewModel::new();
    let mut spec = hzip_core::jobs::JobSpec::new(hzip_core::jobs::JobKind::Compress, src.clone());
    spec.compress = Some(hzip_core::services::compress::CompressSpec {
        inputs: vec![src],
        format: hzip_format::FormatId::from("tar"),
        out: out.clone(),
        level: hzip_core::Level::Store,
        ..Default::default()
    });
    let id = vm.submit(&eng, spec).expect("submit");
    assert_eq!(vm.rows().len(), 1);
    assert_eq!(vm.row(id).expect("row").state, JobRowState::Queued);
    assert!(vm.has_active());
    // cancel 走内部 handle（登记验证——不实际取消）
    assert!(vm.handles.contains_key(&id), "handle 已登记");
}

/// 回归：状态投影**不得**写终局（终局归 `on_finished`，否则失败明细丢失）。
#[test]
fn core_state_projection_leaves_terminal_to_on_finished() {
    use super::project_core_state as proj;
    use hzip_core::jobs::JobState as C;
    assert_eq!(proj(C::Queued), Some(JobRowState::Queued));
    assert_eq!(proj(C::Running), Some(JobRowState::Running));
    assert_eq!(proj(C::WaitingInput), Some(JobRowState::WaitingInput));
    assert_eq!(proj(C::Cancelling), Some(JobRowState::Cancelling));
    for terminal in [C::Done, C::PartialDone, C::Failed, C::Cancelled] {
        assert_eq!(proj(terminal), None, "终局不得由投影写入: {terminal:?}");
    }
}

/// 回归：`JobStarted` 落账即 Running——不得等第一个进度事件
/// （否则无进度的索引阶段界面显示「等待开始…」，与实际解压情况不符）。
#[test]
fn job_started_marks_row_running_before_any_progress() {
    let mut vm = JobsViewModel::new();
    vm.register(row(1));
    assert_eq!(vm.row_mut(1).unwrap().state, JobRowState::Queued);

    vm.on_event(
        1,
        &ProgressEvent::JobStarted {
            job_id: 1,
            total_bytes: 0,
            total_entries: 0,
        },
    );

    let r = vm.row_mut(1).unwrap();
    assert_eq!(r.state, JobRowState::Running, "JobStarted 应立刻 Running");
    assert!(r.progress.is_none(), "此时仍无进度数据（窗口走准备中文案）");
    assert!(r.started_at.is_some(), "开始时刻应回填");
}

#[test]
fn on_event_counts_entries_live() {
    // JobTotal 总量补报 + EntryDone 活计数（任务中心「x/y 条」实时数据源；
    // 未登记 job 的事件静默丢弃不 panic）
    let mut vm = JobsViewModel::new();
    vm.register(row(1));
    vm.on_event(
        1,
        &ProgressEvent::JobTotal {
            job_id: 1,
            total_bytes: 100,
            total_entries: 3,
        },
    );
    for n in 1..=2u64 {
        vm.on_event(
            1,
            &ProgressEvent::EntryDone {
                entry: hzip_progress::EntryName::from("a.txt"),
                bytes: n,
                crc_ok: true,
            },
        );
    }
    let r = vm.row(1).unwrap();
    assert_eq!(r.entries_total, 3, "JobTotal 补报条目总数即时生效");
    assert_eq!(r.entries_done, 2, "EntryDone 逐条活计数");
    // 未登记 job → row_mut None → 静默返回
    vm.on_event(
        9,
        &ProgressEvent::JobTotal {
            job_id: 9,
            total_bytes: 0,
            total_entries: 0,
        },
    );
}

// T-PW1：EntryStarted/EntryDone → current_entry/recent_entries 投影 + cap 淘汰 + 终局清空。
#[test]
fn on_event_projects_current_entry_and_recent_list() {
    let mut vm = JobsViewModel::new();
    vm.register(row(7));
    vm.on_event(
        7,
        &ProgressEvent::EntryStarted {
            entry: hzip_progress::EntryName::from("docs/src/报告.txt"),
            bytes: 10,
        },
    );
    assert_eq!(
        vm.row(7).unwrap().current_entry.as_deref(),
        Some("docs/src/报告.txt"),
        "EntryStarted 置当前条目"
    );
    assert!(vm.row(7).unwrap().recent_entries.is_empty());
    vm.on_event(
        7,
        &ProgressEvent::EntryDone {
            entry: hzip_progress::EntryName::from("docs/src/报告.txt"),
            bytes: 1258291,
            crc_ok: true,
        },
    );
    let r = vm.row(7).unwrap();
    assert_eq!(r.recent_entries.len(), 1, "EntryDone 追加已处理条目");
    assert_eq!(r.recent_entries[0].name, "docs/src/报告.txt");
    assert_eq!(r.recent_entries[0].bytes, 1258291);
    assert!(r.recent_entries[0].ok);
    assert_eq!(r.current_entry.as_deref(), Some("docs/src/报告.txt"));
}

#[test]
fn recent_entries_capped_at_500() {
    let mut vm = JobsViewModel::new();
    vm.register(row(8));
    for i in 0..(RECENT_ENTRIES_CAP + 250) {
        vm.on_event(
            8,
            &ProgressEvent::EntryDone {
                entry: hzip_progress::EntryName::from(format!("f{i}.txt").as_str()),
                bytes: i as u64,
                crc_ok: true,
            },
        );
    }
    let r = vm.row(8).unwrap();
    assert_eq!(r.recent_entries.len(), RECENT_ENTRIES_CAP, "cap 淘汰最旧");
    assert_eq!(r.recent_entries.front().unwrap().name, "f250.txt");
    assert_eq!(
        r.recent_entries.back().unwrap().name,
        "f749.txt",
        "末尾保留最新"
    );
}

#[test]
fn on_finished_clears_current_entry() {
    let mut vm = JobsViewModel::new();
    vm.register(row(9));
    vm.on_event(
        9,
        &ProgressEvent::EntryStarted {
            entry: hzip_progress::EntryName::from("a.txt"),
            bytes: 1,
        },
    );
    assert!(vm.row(9).unwrap().current_entry.is_some());
    vm.on_finished(9, &Outcome::Cancelled);
    assert!(
        vm.row(9).unwrap().current_entry.is_none(),
        "终局清当前条目（进度窗口「当前：…」退场）"
    );
    // recent_entries 终局保留（失败停留形态的明细展示）
    assert!(vm.row(9).unwrap().recent_entries.is_empty());
}

#[test]
fn state_transitions_legal() {
    // 纯状态机：Queued → Running → Done 穷举迁移合法（事件驱动落账）
    let mut vm = JobsViewModel::new();
    vm.register(row(1));
    // JobStarted（无快照 total → 行 Running + total 由快照给）
    vm.apply(1, &snap(0, 100));
    assert_eq!(vm.row(1).unwrap().state, JobRowState::Running);
    assert_eq!(vm.row(1).unwrap().total_bytes, 100);
    vm.apply(1, &snap(60, 100));
    let r = vm.row(1).unwrap();
    assert_eq!(r.done_bytes, 60);
    assert!((r.progress.unwrap() - 0.6).abs() < 1e-9);
    assert_eq!(r.speed_bps, 1234.0);
    // 终局
    let mut s = JobStats::new(100, 1);
    s.bytes_done = 100;
    s.entries_done = 1;
    vm.on_finished(1, &Outcome::Done(s));
    let r = vm.row(1).unwrap();
    assert_eq!(r.state, JobRowState::Done);
    assert_eq!(r.done_bytes, 100);
    assert!(vm.row(1).unwrap().eta.is_some());
    assert!(!vm.has_active());
}

#[test]
fn partial_outcome_records_failures() {
    let mut vm = JobsViewModel::new();
    vm.register(row(2));
    let mut s = JobStats::new(50, 2);
    s.entries_done = 1;
    s.bytes_done = 20;
    let fs = vec![hzip_progress::EntryFailure {
        entry: "bad.txt".into(),
        error: Box::new(hzip_error::HzipError::Checksum {
            entry: hzip_format::EntryName::new("bad.txt"),
            expected: 1,
            actual: 2,
        }),
    }];
    vm.on_finished(2, &Outcome::Partial(s.clone(), fs));
    let r = vm.row(2).unwrap();
    assert_eq!(r.state, JobRowState::PartialDone);
    assert_eq!(r.failures.len(), 1);
    assert_eq!(r.failures[0].entry, "bad.txt");
    assert_eq!(r.failures[0].code, 3001);
    assert_eq!(r.failures[0].message, "err.hz_3001");
    assert!(matches!(r.finished, Some(JobRowOutcome::Partial { .. })));
    // attach_failure_reason：EntryDone 上报过的条目 → 原位并入 reason；
    // 终局摘要行 final_line 落账（T-PW 文件列表持续输出失败结果）
    assert_eq!(r.final_line.as_ref().unwrap().code, 3001);
    assert!(r.final_line.as_ref().unwrap().entry.contains("1 项"));
    vm.on_event(
        2,
        &ProgressEvent::EntryDone {
            entry: hzip_progress::EntryName::from("bad.txt"),
            bytes: 10,
            crc_ok: false,
        },
    );
    vm.on_finished(2, &Outcome::Partial(s.clone(), vec![]));
    let r = vm.row(2).unwrap();
    let e = r
        .recent_entries
        .iter()
        .find(|e| e.name == "bad.txt")
        .expect("EntryDone 已上报的失败条目保留在列表");
    assert!(!e.ok);
    assert_eq!(e.code, 3001);
    assert_eq!(e.reason, "err.hz_3001");
}

#[test]
fn failed_and_cancelled_terminal() {
    let mut vm = JobsViewModel::new();
    vm.register(row(3));
    vm.on_finished(
        3,
        &Outcome::Failed(hzip_error::HzipError::Password {
            target: hzip_error::PasswordTarget::Archive,
        }),
    );
    let r = vm.row(3).unwrap();
    assert_eq!(r.state, JobRowState::Failed);
    match &r.finished {
        Some(JobRowOutcome::Failed {
            code,
            i18n_key,
            detail,
        }) => {
            assert_eq!(*code, 3101);
            assert_eq!(i18n_key, "err.hz_3101");
            assert!(!detail.is_empty(), "失败行必须带原文（任务中心显示用）");
        }
        other => panic!("{other:?}"),
    }
    let mut vm2 = JobsViewModel::new();
    vm2.register(row(4));
    vm2.on_finished(4, &Outcome::Cancelled);
    assert_eq!(vm2.row(4).unwrap().state, JobRowState::Cancelled);
    assert!(matches!(
        vm2.row(4).unwrap().finished,
        Some(JobRowOutcome::Cancelled)
    ));
}

#[test]
fn sync_state_bridges_waiting() {
    let mut vm = JobsViewModel::new();
    vm.register(row(5));
    vm.apply(5, &snap(10, 100));
    vm.sync_state(5, "waiting-input");
    assert_eq!(vm.row(5).unwrap().state, JobRowState::WaitingInput);
    vm.sync_state(5, "cancelling");
    assert_eq!(vm.row(5).unwrap().state, JobRowState::Cancelling);
    // 终态后不同步（以 JobFinished 为准）
    vm.on_finished(5, &Outcome::Cancelled);
    assert_eq!(vm.row(5).unwrap().state, JobRowState::Cancelled);
}

// R-GUI4 #4：任务元数据链——apply 置 started_at；终局定格 elapsed
#[test]
fn job_metadata_started_and_elapsed_lifecycle() {
    let mut vm = JobsViewModel::new();
    vm.register(row(6));
    assert!(vm.row(6).unwrap().started_at.is_none());
    vm.apply(6, &snap(10, 100));
    let started = vm.row(6).unwrap().started_at;
    assert!(started.is_some(), "首帧快照置开始时间");
    // 终局定格耗时
    let mut stats = JobStats::new(100, 1);
    stats.bytes_done = 100;
    vm.on_finished(6, &Outcome::Done(stats));
    let r = vm.row(6).unwrap();
    assert!(r.elapsed.is_some(), "终局落账耗时");
    assert!(r.elapsed.unwrap() <= std::time::Duration::from_secs(5));
}

#[test]
fn unknown_job_events_ignored() {
    let mut vm = JobsViewModel::new();
    vm.apply(99, &snap(1, 2)); // 无此行 → 静默
    vm.on_finished(99, &Outcome::Cancelled);
    assert!(vm.rows().is_empty());
    assert_eq!(vm.active_count(), 0, "无行 → 无活跃（pump 退出判据）");
}

/// 轮询等真实 core 句柄落到目标状态（worker 异步；超时即断言失败）。
fn wait_core_state(vm: &JobsViewModel, id: JobId, want: JobState) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while vm.handle(id).map(|h| h.state()) != Some(want) {
        assert!(
            std::time::Instant::now() < deadline,
            "任务 {id} 10s 未到达 {want:?}（当前 {:?}）",
            vm.handle(id).map(|h| h.state())
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 回归：`sync_states_from_core` 无变化时**不得**深拷贝 `rows`。
///
/// 真实引擎单 worker：A 解压到「已存在同名文件」的目标 → core 停在
/// `WaitingInput`（稳定，未应答则 120 s 才超时），B 恒 `Queued`。UI/快照持
/// `rows_arc()` 的 Arc 克隆（引用计数 ≥ 2），旧实现每个唤醒都 `make_mut`
/// 深拷贝整个 `Vec<JobRow>`——本测试以 `Arc::ptr_eq` 钉死该不变量。
#[test]
fn sync_states_from_core_avoids_deep_clone_when_unchanged() {
    use hzip_core::jobs::{ConflictDecision, JobResponse};

    // 样本：tar 含一条目 + 目标目录预先存在同名文件 → Ask 冲突。
    let d = tempfile::TempDir::new().unwrap();
    let sample = |stem: &str, entry: &str| {
        let tar = d.path().join(format!("{stem}.tar"));
        testutil::write_ustar(&tar, &[(entry, &b"new"[..])]);
        let dest = d.path().join(format!("dest_{stem}"));
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join(entry), b"old").unwrap();
        (tar, dest)
    };
    let opts = |dest: std::path::PathBuf| hzip_core::services::extract::ExtractOptions {
        dest_root: Some(dest),
        conflict: hzip_format::ConflictPolicy::Ask,
        ..Default::default()
    };

    let eng = testutil::engine();
    let mut vm = JobsViewModel::new();
    let (tar_a, dest_a) = sample("a", "a.txt");
    let (tar_b, dest_b) = sample("b", "b.txt");
    let a = vm
        .submit(&eng, JobSpec::extract_job(tar_a, opts(dest_a)))
        .expect("submit A");
    let b = vm
        .submit(&eng, JobSpec::extract_job(tar_b, opts(dest_b)))
        .expect("submit B");

    // A 占住唯一 worker 停在冲突；B 恒排队（行/句柄一致 → 无变化）。
    wait_core_state(&vm, a, JobState::WaitingInput);
    assert_eq!(
        vm.handle(b).map(|h| h.state()),
        Some(JobState::Queued),
        "A 占住 worker → B 排队"
    );

    // 首次同步把 A 行推进到 WaitingInput（消耗真实变化，A 已确定 WaitingInput）。
    assert!(vm.sync_states_from_core());
    assert_eq!(vm.row(a).unwrap().state, JobRowState::WaitingInput);

    // 无变化：持 Arc 克隆调同步 → false 且 rows 指针不变（未深拷贝）。
    let held = vm.rows_arc();
    assert!(!vm.sync_states_from_core(), "core 无新状态 → false");
    assert!(
        std::sync::Arc::ptr_eq(&held, &vm.rows_arc()),
        "无变化不得 make_mut 深拷贝 rows（UI 持 Arc 克隆）"
    );

    // 有变化：应答 A 的冲突 → A 收尾、worker 释放、B 起跑停在自身冲突。
    vm.handle(a)
        .unwrap()
        .answer(0, JobResponse::Conflict(ConflictDecision::Overwrite))
        .expect("应答冲突");
    wait_core_state(&vm, b, JobState::WaitingInput);
    assert!(
        vm.sync_states_from_core(),
        "B Queued → WaitingInput → 有变化"
    );
    assert_eq!(
        vm.row(b).unwrap().state,
        JobRowState::WaitingInput,
        "行反映 core 新状态"
    );
}
