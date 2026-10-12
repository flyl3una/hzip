//! hzip-core 集成测试共享工具（T32 新增；mock_jobs/tar_jobs 后门移除后共用）。
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::io::Write;
use std::path::Path;

use hzip_core::engine::{Engine, EngineConfig};
use hzip_format::{EntryMeta, FormatHandler, PasswordRef, Sink};
use tempfile::TempDir;

/// 最小引擎（池/并发可配；无信号/清理副作用——测试免污染）。
pub fn engine(pool_size: usize, max_parallel: u8) -> Engine {
    let cfg = EngineConfig {
        pool_size: Some(pool_size),
        max_parallel,
        cancel_on_signal: false,
        orphan_cleanup: false,
    };
    Engine::new(cfg).expect("engine 构建")
}

/// 产 tar 档案（真实 TarHandler 写；entries: name → data）。
pub fn make_tar(path: &Path, entries: &[(String, Vec<u8>)]) {
    let mut w = hzip_fmt_tar::TarHandler::new()
        .create_writer(Sink::File(path.to_path_buf()), &Default::default())
        .unwrap();
    for (name, data) in entries {
        let meta = EntryMeta {
            name: name.as_str().into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(data).unwrap();
        s.finish().unwrap();
    }
    w.finish().unwrap();
}

/// 产大 tar（单条目，size_mib MiB——慢任务/取消窗口用；tar 不压缩真实写盘）。
pub fn make_big_tar(path: &Path, name: &str, size_mib: u64) {
    let mut w = hzip_fmt_tar::TarHandler::new()
        .create_writer(Sink::File(path.to_path_buf()), &Default::default())
        .unwrap();
    let meta = EntryMeta {
        name: name.into(),
        size_raw: Some(size_mib * 1024 * 1024),
        ..Default::default()
    };
    let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
    let chunk = vec![0x5Au8; 1024 * 1024];
    for _ in 0..size_mib {
        s.write_all(&chunk).unwrap();
    }
    s.finish().unwrap();
    w.finish().unwrap();
}

/// 单文件 gz 流（SingleHandler；a.txt.gz → 解出 a.txt）。
pub fn make_gz(path: &Path, data: &[u8]) {
    let mut w = hzip_fmt_single::gz()
        .create_writer(Sink::File(path.to_path_buf()), &Default::default())
        .unwrap();
    let meta = EntryMeta {
        name: "a.txt".into(),
        size_raw: Some(data.len() as u64),
        ..Default::default()
    };
    let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
    s.write_all(data).unwrap();
    s.finish().unwrap();
    w.finish().unwrap();
}

/// 构造「目标已存在」的解压选项（conflict 覆盖）。
pub fn overwrite_opts(dest: &Path) -> hzip_core::services::extract::ExtractOptions {
    hzip_core::services::extract::ExtractOptions {
        dest_root: Some(dest.to_path_buf()),
        conflict: hzip_format::ConflictPolicy::Overwrite,
        ..Default::default()
    }
}

/// 临时档案目录（自动清理）。
pub fn tmp_dir(_tag: &str) -> TempDir {
    TempDir::new().expect("tmpdir")
}
