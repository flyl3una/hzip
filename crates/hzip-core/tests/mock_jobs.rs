//! Engine/jobs 机制集成测试（TC-CORE-005/006/011/012/013 等机制半场）。
//!
//! T30 原以 `EngineConfig.test_dispatch` 后门注入 Scripted 行为验证机制；
//! T32 合入后后门删除——本文件改以**真实 tar 档案 + 真实解压管道**驱动机制
//! 断言（后门依赖的纯脚本用例已删/移交 extract_pipeline.rs 真实档案半场）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use hzip_core::jobs::{ConflictDecision, JobKind, JobRequest, JobResponse, JobSpec, JobState};
use hzip_format::ConflictPolicy;
use hzip_progress::Outcome;
use tempfile::TempDir;

use common::{engine, make_tar, overwrite_opts};

/// TC-CORE-005：冲突 Ask 请求-应答（真实 tar 管道上抛）——
/// WaitingInput 态、应答 Overwrite 后继续 → Done。
#[test]
fn conflict_ask_callback() {
    let dir = TempDir::new().unwrap();
    // 档案：单文件 a.txt（smart 判定 Bare → dest 根直落）
    let archive = dir.path().join("data.tar");
    make_tar(&archive, &[("a.txt".into(), b"hello".to_vec())]);
    // dest 预置同名 a.txt（触发冲突）
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("a.txt"), b"old").unwrap();
    let eng = engine(1, 1);
    // Ask 策略（默认）+ 预置同名 → 冲突上抛
    let opts = overwrite_opts(&dest);
    let opts = hzip_core::services::extract::ExtractOptions {
        conflict: ConflictPolicy::Ask,
        ..opts
    };
    let h = eng
        .submit(JobSpec::extract_job(archive.clone(), opts.clone()))
        .unwrap();
    // 收请求
    let req = h.requests().recv().expect("收到 JobRequest");
    assert!(
        matches!(req, JobRequest::Conflict { .. }),
        "应 Conflict 请求，实得 {req:?}"
    );
    // WaitingInput 迁移经消息通道异步落账——轮询等待（need 先发请求后置态）
    for _ in 0..200 {
        if h.state() == JobState::WaitingInput {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.state(), JobState::WaitingInput, "等待应答期间状态");
    // 应答 Overwrite → 继续 → Done
    h.answer(0, JobResponse::Conflict(ConflictDecision::Overwrite))
        .unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("应答后应 Done，实得 {other:?}"),
    }
    // 覆盖生效：内容为新
    assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"hello");
    let _ = opts;
}

/// TC-CORE-006 核心语义（120s 超时无法全链等待）：Ask 无应答 + 任务取消 →
/// 取消路径（need 感知取消令牌中断）；决策超时默认 Skip 由 conflict 单测 +
/// ExtractOptions 短超时注入在 T40 覆盖（此处验证取消中断不挂死）。
#[test]
fn conflict_waiting_cancel() {
    let dir = TempDir::new().unwrap();
    let archive = dir.path().join("data.tar");
    make_tar(&archive, &[("a.txt".into(), b"hello".to_vec())]);
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("a.txt"), b"old").unwrap();
    let eng = engine(1, 1);
    let opts = hzip_core::services::extract::ExtractOptions {
        dest_root: Some(dest.clone()),
        conflict: ConflictPolicy::Ask,
        ..Default::default()
    };
    let h = eng.submit(JobSpec::extract_job(archive, opts)).unwrap();
    // 等 WaitingInput
    for _ in 0..200 {
        if h.state() == JobState::WaitingInput {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.state(), JobState::WaitingInput, "应 WaitingInput");
    h.cancel();
    match h.wait().unwrap() {
        Outcome::Cancelled => {}
        other => panic!("取消后应 Cancelled，实得 {other:?}"),
    }
    // 半写无残留（未应答 → 未覆盖写入）
    assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"old");
}

/// TC-CORE-011：级联取消——cancel_all 后 Running 任务取消、Queued 直接移除。
#[test]
fn cascade_cancel() {
    let dir = TempDir::new().unwrap();
    // 慢任务（大 tar 解压）
    let mk_slow = || {
        let p = dir.path().join(format!("slow-{}.tar", std::process::id()));
        common::make_big_tar(&p, "big.bin", 128);
        p
    };
    let a1 = mk_slow();
    let a2 = mk_slow();
    let a3 = mk_slow();
    let eng = engine(1, 1);
    let h1 = eng
        .submit(JobSpec::extract_job(
            a1,
            overwrite_opts(&dir.path().join("d1")),
        ))
        .unwrap();
    let h2 = eng
        .submit(JobSpec::extract_job(
            a2,
            overwrite_opts(&dir.path().join("d2")),
        ))
        .unwrap();
    let h3 = eng
        .submit(JobSpec::extract_job(
            a3,
            overwrite_opts(&dir.path().join("d3")),
        ))
        .unwrap();
    // 池 1：等 h1 Running（h2/h3 必 Queued）
    for _ in 0..200 {
        if h1.state() == JobState::Running {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h1.state(), JobState::Running, "h1 应 Running");
    assert_eq!(
        h2.state(),
        JobState::Queued,
        "h2 应 Queued（池 1 被 h1 占），实得 {:?}",
        h2.state()
    );
    assert_eq!(
        h3.state(),
        JobState::Queued,
        "h3 应 Queued（池 1 被 h1 占），实得 {:?}",
        h3.state()
    );
    // 级联取消：h1 Running 取消；h2/h3 Queued 直接移除
    eng.cancel_all();
    let r1 = h1.wait().unwrap();
    let r2 = h2.wait().unwrap();
    let r3 = h3.wait().unwrap();
    assert!(
        matches!(r1, Outcome::Cancelled | Outcome::Done(_)),
        "h1 {r1:?}"
    );
    assert!(matches!(r2, Outcome::Cancelled), "h2（Queued 移除）{r2:?}");
    assert!(matches!(r3, Outcome::Cancelled), "h3（Queued 移除）{r3:?}");
}

/// TC-CORE-012：并发上限——任意时刻 running ≤ 2（真实解压驱动）。
#[test]
fn parallel_limit() {
    let dir = TempDir::new().unwrap();
    let mk_slow = |i: usize| {
        // 每任务**独立档案**：同键（同文件）任务按调度策略串行，本用例要验的是
        // 全局并发上限，故不能让资源键把它们串起来。
        let p = dir
            .path()
            .join(format!("pl-{}-{i}.tar", std::process::id()));
        common::make_big_tar(&p, "big.bin", 48);
        p
    };
    let eng = engine(2, 2);
    let mut handles = Vec::new();
    for i in 0..5 {
        let a = mk_slow(i);
        let d = dir.path().join(format!("pl-dest-{i}"));
        std::fs::create_dir_all(&d).unwrap();
        handles.push(
            eng.submit(JobSpec::extract_job(a, overwrite_opts(&d)))
                .unwrap(),
        );
    }
    // 轮询断言 running ≤ 2
    for _ in 0..50 {
        let running = eng
            .snapshot()
            .iter()
            .filter(|(_, s)| matches!(s, JobState::Running | JobState::WaitingInput))
            .count();
        assert!(running <= 2, "running {running} 超上限 2");
        std::thread::sleep(Duration::from_millis(20));
    }
    for h in handles {
        let r = h.wait().unwrap();
        assert!(matches!(r, Outcome::Done(_) | Outcome::Cancelled), "{r:?}");
    }
}

/// submit 校验：源不存在 → 即时 Err（不排队）。
#[test]
fn submit_validation() {
    let eng = engine(1, 1);
    let spec = JobSpec::new(
        JobKind::Extract,
        "F:/nonexistent/definitely-missing.bin".into(),
    );
    match eng.submit(spec) {
        Err(err) => {
            assert!(
                err.to_string().contains("hz-5004"),
                "源不存在应报 5004: {err}"
            );
        }
        Ok(_) => panic!("源不存在应即时 Err"),
    }
}

/// 状态机迁移合法性（真实 tar 解压 Queued→Running→Done 顺序）。
#[test]
fn state_transitions_ok() {
    let dir = TempDir::new().unwrap();
    let archive = dir.path().join("t.tar");
    make_tar(&archive, &[("a.txt".into(), b"x".to_vec())]);
    let eng = engine(1, 1);
    let h = eng
        .submit(JobSpec::extract_job(
            archive,
            overwrite_opts(&dir.path().join("dest")),
        ))
        .unwrap();
    assert!(matches!(h.state(), JobState::Queued | JobState::Running));
    let r = h.wait().unwrap();
    assert!(matches!(r, Outcome::Done(_)), "{r:?}");
}

/// 事件通路：JobStarted/JobFinished 顺序 + 条目事件（真实管道发出）。
#[test]
fn progress_event_order() {
    let dir = TempDir::new().unwrap();
    let archive = dir.path().join("ev.tar");
    make_tar(&archive, &[("a.txt".into(), b"hello world".to_vec())]);
    let eng = engine(1, 1);
    let h = eng
        .submit(JobSpec::extract_job(
            archive,
            overwrite_opts(&dir.path().join("dest")),
        ))
        .unwrap();
    let rx = h.events();
    let mut seen_entry_started = false;
    let mut seen_entry_done = false;
    let mut seen_finished = false;
    loop {
        match rx.recv() {
            None => break,
            Some(ev) => match ev {
                hzip_progress::ProgressEvent::JobFinished { .. } => {
                    seen_finished = true;
                    break; // Finished 必达且最后
                }
                hzip_progress::ProgressEvent::EntryStarted { .. } => seen_entry_started = true,
                hzip_progress::ProgressEvent::EntryDone { .. } => seen_entry_done = true,
                _ => {}
            },
        }
    }
    assert!(
        seen_entry_started && seen_entry_done,
        "条目事件 {seen_entry_started}/{seen_entry_done}"
    );
    assert!(seen_finished, "JobFinished 必达");
    let r = h.wait().unwrap();
    assert!(matches!(r, Outcome::Done(_)), "{r:?}");
}

/// TC-CORE-013 panic 隔离语义的后门替代：worker 对坏档案（非 tar 魔数）→
/// Failed（Format 错误）而非 panic；进程存活 + 后续健康任务正常。
#[test]
fn bad_archive_fails_not_panics() {
    let dir = TempDir::new().unwrap();
    let bad = dir.path().join("bad.tar");
    std::fs::write(&bad, b"not a tar archive at all").unwrap();
    let eng = engine(1, 1);
    let h1 = eng
        .submit(JobSpec::extract_job(
            bad,
            overwrite_opts(&dir.path().join("d1")),
        ))
        .unwrap();
    match h1.wait().unwrap() {
        Outcome::Failed(_) => {}
        other => panic!("坏档案应 Failed，实得 {other:?}"),
    }
    // 进程存活 + 后续健康任务正常（三件套后两项）
    let good = dir.path().join("good.tar");
    make_tar(&good, &[("a.txt".into(), b"ok".to_vec())]);
    let h2 = eng
        .submit(JobSpec::extract_job(
            good,
            overwrite_opts(&dir.path().join("d2")),
        ))
        .unwrap();
    match h2.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("后续任务应 Done，实得 {other:?}"),
    }
}
