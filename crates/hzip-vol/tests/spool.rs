//! `hzip-vol::materialize` 的 spool 契约（集成测试；以公开 API 驱动）。
//!
//! 回归锚点（2026-10-17 CI 实测）：**spool 目录不存在时也要能开工**。
//! `SpoolPolicy::default().dir` = `hzip-platform::dirs::temp_root()`，而该函数按契约
//! 只算路径、不 mkdir；此前只有 GUI/CLI 的会话区（`TempArea::new_session`）会把它
//! 建出来，于是没建会话的宿主（集成测试 / 嵌入式调用）一 spool 就 `NotFound`：
//! Linux runner（`XDG_RUNTIME_DIR=/run/user/<uid>` 在、其下 `hzip` 不在）上
//! `PathError { path: "/run/user/<uid>/hzip/hzip-spool-…", err: NotFound }`，
//! 三个平台共 8 个 test target 全红，而开发机因为历史运行早已建好该目录、本地恒绿。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

use std::io::Cursor;

use hzip_vol::materialize::spool_from_stream;
use hzip_vol::{SpoolPolicy, VolError};

/// 目录**不存在**（含多层）时：spool 自己把它建出来，内容逐字节落盘。
#[test]
fn spool_creates_the_directory_when_it_is_missing() {
    let base = tempfile::tempdir().unwrap();
    // 三层都不存在：连父级也要建（`temp_root()` 的父级在 CI 上存在，但没理由假设）
    let missing = base.path().join("run").join("user").join("hzip");
    assert!(!missing.exists(), "前置：目录必须不存在");

    let policy = SpoolPolicy {
        dir: missing.clone(),
        ..SpoolPolicy::default()
    };
    let data = b"qcow2-ish payload".repeat(1024);
    let sp = spool_from_stream(&mut Cursor::new(data.clone()), &policy, None).expect("spool 成功");

    assert!(missing.is_dir(), "spool 必须把目录建出来");
    assert_eq!(sp.len(), data.len() as u64, "长度 = 源长度");
    assert_eq!(std::fs::read(sp.path()).unwrap(), data, "内容逐字节一致");
    // 文件名前缀不可改（孤儿回收按前缀识别）
    let name = sp
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("hzip-spool-"),
        "文件名必须带回收前缀：{name}"
    );
    assert_eq!(
        sp.path().parent().unwrap(),
        missing.as_path(),
        "落盘位置 = policy.dir"
    );
}

/// 目录**已存在**时幂等（不报错、不清理已有文件）。
#[test]
fn spool_is_idempotent_when_the_directory_exists() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("keep.txt");
    std::fs::write(&marker, b"keep").unwrap();

    let policy = SpoolPolicy {
        dir: dir.path().to_path_buf(),
        ..SpoolPolicy::default()
    };
    let sp =
        spool_from_stream(&mut Cursor::new(b"abc".to_vec()), &policy, None).expect("spool 成功");
    assert_eq!(sp.len(), 3);
    assert!(marker.exists(), "不得动目录里已有的无关文件");
}

/// 目录建不出来（父级是文件）⇒ 报 IO 错误而不是 panic（调用方按「临时盘不可用」处理）。
#[test]
fn spool_reports_io_error_when_the_directory_cannot_be_created() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"i am a file").unwrap();

    let policy = SpoolPolicy {
        dir: blocker.join("hzip"), // 父级是文件 ⇒ create_dir_all 必失败
        ..SpoolPolicy::default()
    };
    match spool_from_stream(&mut Cursor::new(b"x".to_vec()), &policy, None) {
        Err(VolError::Io(_)) => {}
        other => panic!("应为 IO 错误：{other:?}"),
    }
}
