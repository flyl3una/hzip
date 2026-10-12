use super::*;
use hzip_format::{EntryAttrs, EntryInfo, EntryName, NameEncoding};

fn info(name: &str, is_dir: bool) -> EntryInfo {
    EntryInfo {
        id: 0,
        name: EntryName::new(name.to_string()),
        is_dir,
        is_symlink: false,
        symlink_target: None,
        size_raw: 0,
        size_packed: 0,
        mtime: None,
        mode: None,
        attributes: EntryAttrs::default(),
        encrypted: false,
        header_encrypted: false,
        crc32: None,
        codec: None,
        name_encoding: NameEncoding::Utf8,
        encryption: None,
        compression_level: None,
    }
}

/// v2 默认（wrap=false）：单顶层目录 → Direct，绝不剥壳。
#[test]
fn default_single_top_dir_direct() {
    let entries = vec![info("root/a", false), info("root/b", false)];
    assert_eq!(plan(&entries, false), DestPlan::Direct);
    // 显式目录条目 + 其下文件混合
    let entries2 = vec![
        info("root", true),
        info("root/a", false),
        info("root/b", false),
    ];
    assert_eq!(plan(&entries2, false), DestPlan::Direct);
    // 单顶层目录 + wrap=true 也 Direct（不包壳不剥壳）
    assert_eq!(plan(&entries2, true), DestPlan::Direct);
    // resolve_dest 恒返回 dest_root + strip None
    let tmp = tempfile::tempdir().unwrap();
    let (dest, strip) = resolve_dest(&entries2, tmp.path(), Some("root"), true).unwrap();
    assert_eq!(dest, tmp.path().to_path_buf());
    assert_eq!(strip, None);
}

/// 散文件（a.txt + dir/b.txt）→ wrap=true 时 Wrap（防污染）；false 直展。
#[test]
fn scattered_files_wrap_only_when_smart() {
    let entries = vec![info("a.txt", false), info("dir/b.txt", false)];
    assert_eq!(plan(&entries, false), DestPlan::Direct);
    assert!(matches!(plan(&entries, true), DestPlan::Wrap { .. }));
}

/// 单文件 → Direct（wrap 与否都直展）。
#[test]
fn single_file_direct() {
    let entries = vec![info("a.txt", false)];
    assert_eq!(plan(&entries, false), DestPlan::Direct);
    assert_eq!(plan(&entries, true), DestPlan::Direct);
}

/// 空档案 → Direct。
#[test]
fn empty_archive_direct() {
    assert_eq!(plan(&[], false), DestPlan::Direct);
    assert_eq!(plan(&[], true), DestPlan::Direct);
}

/// 单顶层目录 + 顶层散文件 → wrap=true Wrap（C 不满足：有散文件）。
#[test]
fn dir_plus_scattered_file_wrap() {
    let entries = vec![
        info("root/a", false),
        info("root/b", false),
        info("readme.txt", false),
    ];
    assert_eq!(plan(&entries, false), DestPlan::Direct);
    assert!(matches!(plan(&entries, true), DestPlan::Wrap { .. }));
}

/// 2+ 顶层目录 → wrap=true Wrap（污染目标根）；false 直展。
#[test]
fn multi_top_dirs_wrap() {
    let entries = vec![info("d1/a.txt", false), info("d2/b.txt", false)];
    assert_eq!(plan(&entries, false), DestPlan::Direct);
    assert!(matches!(plan(&entries, true), DestPlan::Wrap { .. }));
}

/// Wrap stem 剥后缀（`a.tar.gz`/`a.tar.bz2` → `a`；剥空兜底）。
#[test]
fn wrap_stem_stripping() {
    let suffixes = &["gz", "bz2", "xz", "zst", "tar", "zip", "7z", "rar"];
    assert_eq!(strip_suffixes("a.tar.gz", suffixes), "a");
    assert_eq!(strip_suffixes("a.tar.bz2", suffixes), "a");
    assert_eq!(strip_suffixes("a.tar.xz", suffixes), "a");
    assert_eq!(strip_suffixes("a.tar.zst", suffixes), "a");
    assert_eq!(strip_suffixes("a.zip", suffixes), "a");
    assert_eq!(strip_suffixes("A.TAR.GZ", suffixes), "A");
    assert_eq!(strip_suffixes("archive.data", suffixes), "archive.data");
    assert_eq!(strip_suffixes(".gz", suffixes), ".gz");
}

/// resolve_dest：Direct 返回 dest_root + None；Wrap 创建 dest_root/stem。
#[test]
fn resolve_dest_wrap_creates_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Direct：单顶层目录，wrap=true 也不包
    let entries = vec![info("root/a", false)];
    let (dest, strip) = resolve_dest(&entries, root, Some("a"), true).unwrap();
    assert_eq!(dest, root.to_path_buf());
    assert_eq!(strip, None);
    // Wrap：散文件 → dest/stem 创建
    let scattered = vec![info("a.txt", false), info("d/b.txt", false)];
    let stem = strip_suffixes("data.tar.gz", &["gz", "tar"]);
    let (dest, strip) = resolve_dest(&scattered, root, Some(&stem), true).unwrap();
    assert!(dest.is_dir(), "Wrap 目录应已创建");
    assert!(dest.ends_with("data"), "stem=data，实得 {dest:?}");
    assert_eq!(strip, None);
}

/// apply_strip 兼容层：v2 strip 恒 None → 原名返回。
#[test]
fn apply_strip_none_identity() {
    assert_eq!(apply_strip("root/a", None), "root/a");
    // 显式 Some（旧路径残留不会触发——v2 恒 None）
    assert_eq!(apply_strip("root/a", Some("root")), "a");
}
