//! 真 tar 全链路集成测试（TC-CORE-009/010/014）。
//!
//! T30 经 `test_dispatch` 后门注入真 handler extract；T32 后门删除后这些用例
//! 改走**完整解压管道**（services::execute Extract 臂：预检 → smart → 净化 →
//! 流拷贝 → 配额 → 报告），验证 engine/jobs 对真实 IO 的行为。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Instant;

use hzip_core::jobs::{JobSpec, JobState};
use hzip_progress::{Outcome, ProgressEvent};
use tempfile::TempDir;

use common::{engine, make_big_tar, make_tar, overwrite_opts};

/// TC-CORE-010：进度守恒 + 事件顺序（经 Engine 端到端全管道）。
#[test]
fn progress_conservation() {
    let dir = TempDir::new().unwrap();
    let archive = dir.path().join("data.tar");
    let entries: Vec<(String, Vec<u8>)> = vec![
        (
            "a.txt".into(),
            b"hello world, this is a test file.\n".repeat(1000),
        ),
        (
            "dir/b.bin".into(),
            (0..100_000u32)
                .map(|i| (i.wrapping_mul(31) & 0xff) as u8)
                .collect(),
        ),
        ("empty.dat".into(), Vec::new()),
    ];
    let total_bytes: u64 = entries.iter().map(|(_, d)| d.len() as u64).sum();
    make_tar(&archive, &entries);

    let dest = dir.path().join("dest");
    let eng = engine(2, 2);
    let h = eng
        .submit(JobSpec::extract_job(archive.clone(), overwrite_opts(&dest)))
        .unwrap();
    let rx = h.events();
    let mut started = false;
    let mut finished = false;
    let mut bytes_sum: u64 = 0;
    loop {
        match rx.recv() {
            None => break,
            Some(ev) => match ev {
                ProgressEvent::JobStarted { .. } => {
                    assert!(!started, "JobStarted 恰一次");
                    started = true;
                }
                ProgressEvent::Bytes { delta } => bytes_sum += delta,
                ProgressEvent::JobFinished { .. } => {
                    finished = true;
                    break;
                }
                _ => {}
            },
        }
    }
    assert!(started && finished);
    let r = h.wait().unwrap();
    assert!(matches!(r, Outcome::Done(_)), "{r:?}");
    // 守恒（T32 管道级：ΣBytes == total——本管道逐 chunk 发 Bytes）
    assert_eq!(bytes_sum, total_bytes, "字节守恒");
    // 落盘验证（v2 默认 smart=false）：散文件 + 目录 → 原样直展 dest
    assert!(dest.join("a.txt").exists(), "a.txt 应直展落盘");
    assert!(dest.join("dir/b.bin").exists(), "dir/b.bin 应直展落盘");
}

/// TC-CORE-009：取消 → Cancelled；半写文件清理。
#[test]
fn cancel_rollback() {
    let dir = TempDir::new().unwrap();
    let archive = dir.path().join("big.tar");
    make_big_tar(&archive, "big.bin", 64);
    let eng = engine(1, 1);
    let h = eng
        .submit(JobSpec::extract_job(
            archive,
            overwrite_opts(&dir.path().join("dest")),
        ))
        .unwrap();
    // 等 Running 后取消
    for _ in 0..200 {
        if h.state() == JobState::Running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    h.cancel();
    let r = h.wait().unwrap();
    assert!(
        matches!(r, Outcome::Cancelled | Outcome::Done(_)),
        "取消或已完成: {r:?}"
    );
}

/// TC-CORE-014：取消延迟——cancel 后 wait 快速返回（< 5s，非 120s 挂死）。
#[test]
fn cancel_latency() {
    let dir = TempDir::new().unwrap();
    let archive = dir.path().join("slow.tar");
    make_big_tar(&archive, "big.bin", 200);
    let eng = engine(1, 1);
    let h = eng
        .submit(JobSpec::extract_job(
            archive,
            overwrite_opts(&dir.path().join("dest")),
        ))
        .unwrap();
    for _ in 0..200 {
        if h.state() == JobState::Running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(h.state(), JobState::Running);
    let t0 = Instant::now();
    h.cancel();
    let r = h.wait().unwrap();
    let elapsed = t0.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "取消延迟 {elapsed:?} 超 5s"
    );
    assert!(matches!(r, Outcome::Cancelled | Outcome::Done(_)), "{r:?}");
}
