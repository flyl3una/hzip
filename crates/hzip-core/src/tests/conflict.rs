use super::*;

/// 命名格式属性（`{stem} ({n}){ext}`）。
#[test]
fn format_canonical() {
    assert_eq!(
        insert_suffix(Path::new("a.txt"), 2),
        PathBuf::from("a (2).txt")
    );
    assert_eq!(
        insert_suffix(Path::new("a.txt"), 3),
        PathBuf::from("a (3).txt")
    );
    assert_eq!(insert_suffix(Path::new("a"), 2), PathBuf::from("a (2)"));
    assert_eq!(
        insert_suffix(Path::new(".env"), 2),
        PathBuf::from(".env (2)")
    );
    assert_eq!(
        insert_suffix(Path::new("a.tar.gz"), 2),
        PathBuf::from("a.tar (2).gz")
    );
    assert_eq!(
        insert_suffix(Path::new("/d/x/a.txt"), 2),
        PathBuf::from("/d/x/a (2).txt")
    );
}

/// TC-CORE-004：目标已有 a.txt、a (2).txt → 新文件命名为 a (3).txt；
/// 串行重复分配（新台账）恒等（属性②）。
#[test]
fn conflict_rename_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("a.txt");
    std::fs::write(&base, b"1").unwrap();
    std::fs::write(tmp.path().join("a (2).txt"), b"2").unwrap();
    let next = AllocatedNames::new().allocate(&base, |p| p.exists());
    assert_eq!(next, tmp.path().join("a (3).txt"));
    // 串行重复（新台账）恒等
    let next2 = AllocatedNames::new().allocate(&base, |p| p.exists());
    assert_eq!(next, next2);
}

/// 台账原子分配：a (2).txt 分配后，下一次 allocate 跳至 a (3).txt。
#[test]
fn allocated_ledger_skip() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("a.txt");
    let allocated = AllocatedNames::new();
    let first = allocated.allocate(&base, |p| p.exists());
    assert_eq!(first, tmp.path().join("a (2).txt"));
    let second = allocated.allocate(&base, |p| p.exists());
    assert_eq!(second, tmp.path().join("a (3).txt"));
    let third = allocated.allocate(&base, |p| p.exists());
    assert_eq!(third, tmp.path().join("a (4).txt"));
}

/// allocate 边界：无扩展名/点文件。
#[test]
fn probe_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let a = AllocatedNames::new();
    let p = tmp.path().join("noext");
    assert_eq!(a.allocate(&p, |_| false), tmp.path().join("noext (2)"));
    let d = tmp.path().join(".hidden");
    assert_eq!(a.allocate(&d, |_| false), tmp.path().join(".hidden (2)"));
}

/// 折叠存在性（win 大小写不敏感默认）：A.txt 已存在 → a.txt 探测折叠命中。
#[test]
#[cfg(windows)]
fn case_fold_probe_windows() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("A.txt"), b"x").unwrap();
    assert!(fold_exists_dir(&tmp.path().join("a.txt")), "折叠命中 A.txt");
    // insert_suffix 探测跳过既有 a.txt（折叠）→ a (2).txt
    let allocated = AllocatedNames::new();
    let next = allocated.allocate(&tmp.path().join("a.txt"), |p| {
        p.exists() || fold_exists_dir(p)
    });
    assert_eq!(next, tmp.path().join("a (2).txt"));
}
