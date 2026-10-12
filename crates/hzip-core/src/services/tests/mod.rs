//! services 管道单元测试（T33；02 §9.5 过滤链/预统计 + report 聚合纯函数）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hzip_format::{EntryAttrs, EntryInfo, WriteReport};
use hzip_progress::JobStats;

use crate::report;

/// WriteReport → JobStats 聚合（entries_written/bytes_in/bytes_out 映射）。
#[test]
fn report_stats_from_write() {
    let r = WriteReport {
        entries_written: 7,
        bytes_in: 1234,
        bytes_out: 99,
        volume_paths: vec![],
    };
    let s: JobStats = report::stats_from_write(&r);
    assert_eq!(s.entries_done, 7);
    assert_eq!(s.entries_total, 7);
    assert_eq!(s.bytes_done, 99);
    assert_eq!(s.bytes_total, 1234);
}

/// EditDiff 新旧对比（old=条目编辑前；new=重写报告）。
#[test]
fn report_edit_diff() {
    let before = EntryInfo {
        id: 0,
        name: "a.txt".into(),
        is_dir: false,
        is_symlink: false,
        symlink_target: None,
        size_raw: 10,
        size_packed: 8,
        mtime: None,
        mode: None,
        attributes: EntryAttrs::default(),
        encrypted: false,
        header_encrypted: false,
        crc32: None,
        codec: None,
        name_encoding: hzip_format::NameEncoding::Utf8,
        encryption: None,
        compression_level: None,
    };
    let after = WriteReport {
        entries_written: 1,
        bytes_in: 99,
        bytes_out: 50,
        volume_paths: vec![],
    };
    let d = report::edit_diff(&before, &after);
    assert_eq!(d.old_size, 10);
    assert_eq!(d.new_size, 99);
    assert_eq!(d.old_entries, 1);
    assert_eq!(d.new_entries, 1);
}

/// WalkedFile 预统计（total 源头）：普通文件计字节；目录/缺失不计。
#[test]
fn walk_pre_stats_counting() {
    use crate::services::walk::{WalkedFile, pre_stats};
    use std::path::PathBuf;
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.bin");
    std::fs::write(&f, vec![1u8; 300]).unwrap();
    let inputs = vec![
        WalkedFile {
            abs: f.clone(),
            rel: "a.bin".into(),
            meta: Default::default(),
        },
        WalkedFile {
            abs: PathBuf::from("C:/nonexistent/x"),
            rel: "x".into(),
            meta: Default::default(),
        },
    ];
    let s = pre_stats(&inputs);
    assert_eq!(s.entries, 2);
    assert_eq!(s.total_bytes, 300, "缺失文件不计字节");
}
