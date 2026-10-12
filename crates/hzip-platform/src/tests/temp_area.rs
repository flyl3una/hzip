use super::*;

fn temp_session() -> (crate::testutil::TempDir, TempArea) {
    let dir = crate::testutil::temp_dir("temp-area");
    let ta = TempArea::new_session(Some(dir.path().to_path_buf())).unwrap();
    (dir, ta)
}

#[test]
fn path_allocation() {
    let (_d, ta) = temp_session();
    let archive = Path::new(r"C:\archives\a.zip");
    let e1 = EntryNameSafe::sanitize("x.txt").unwrap();
    let p1 = ta
        .materialize(archive, &e1, std::io::Cursor::new(b"a"))
        .unwrap();
    let p2 = ta
        .materialize(archive, &e1, std::io::Cursor::new(b"b"))
        .unwrap();
    assert_eq!(p1, p2, "同一档案路径两次 materialize 目标路径稳定");
    let archive2 = Path::new(r"C:\archives\b.zip");
    let p3 = ta
        .materialize(archive2, &e1, std::io::Cursor::new(b"c"))
        .unwrap();
    assert_ne!(p1.parent(), p3.parent(), "不同档案 hash 目录不同");
    assert!(p1.starts_with(ta.session_dir()));
    ta.cleanup().unwrap();
}

#[test]
fn sanitize_escape() {
    for evil in [
        "../../evil",
        r"..\..\evil",
        "/etc/passwd",
        r"C:\Windows\evil",
        "a\0b",
        "a\rb",
    ] {
        let r = EntryNameSafe::sanitize(evil);
        assert!(r.is_err(), "应拒绝 {evil:?}");
    }
    let ok = EntryNameSafe::sanitize("normal name.txt").unwrap();
    assert_eq!(ok.as_str(), "normal name.txt");
}

#[test]
fn sanitize_materialize_no_escape() {
    let (_d, ta) = temp_session();
    let r = EntryNameSafe::sanitize("../../evil");
    assert!(r.is_err());
    // 合法名无法逃出会话目录（hash 目录下）
    let e = EntryNameSafe::sanitize("a.txt").unwrap();
    let p = ta
        .materialize(Path::new("a.zip"), &e, std::io::Cursor::new(b"x"))
        .unwrap();
    assert!(p.starts_with(ta.session_dir()));
    assert_eq!(
        p.components().count(),
        ta.session_dir().components().count() + 2
    );
    ta.cleanup().unwrap();
}

#[test]
fn state_machine() {
    let (_d, ta) = temp_session();
    // Created → materialize 合法
    let e = EntryNameSafe::sanitize("f.txt").unwrap();
    ta.materialize(Path::new("a.zip"), &e, std::io::Cursor::new(b"x"))
        .unwrap();
    // Created → Watching 合法
    ta.watch(Box::new(|_| {})).unwrap();
    // Watching → materialize 合法（编辑中再物化）
    ta.materialize(Path::new("a.zip"), &e, std::io::Cursor::new(b"y"))
        .unwrap();
    // Watching → Cleaning 合法
    ta.cleanup().unwrap();
    // Cleaning 后 materialize / watch 报错
    let e2 = EntryNameSafe::sanitize("g.txt").unwrap();
    assert!(
        ta.materialize(Path::new("a.zip"), &e2, std::io::Cursor::new(b"z"))
            .is_err()
    );
    assert!(ta.watch(Box::new(|_| {})).is_err());
    // cleanup 幂等
    ta.cleanup().unwrap();
}

#[test]
fn orphan_cleanup() {
    let dir = crate::testutil::temp_dir("temp-area-orphan");
    let root = dir.path().to_path_buf();

    // 活会话（当前 pid + self_created_ms）→ 保留
    let live = TempArea::new_session(Some(root.clone())).unwrap();
    drop(live); // Drop 不清理（模拟未 cleanup 的存活会话）

    // 伪造死会话 1：创建时间不符（pid 为本进程但 created_ms 极小）。
    // 注：不用「pid 不存在」构造——本测试环境 OpenProcess 对不存在 pid 返回
    // ACCESS_DENIED → is_alive 走「不可证死则判活」保守保留（防误删设计）；
    // 该环境差异不影响判死路径的验证（时间不符必判死）。
    let dead1 = root.join("dead1");
    std::fs::create_dir_all(&dead1).unwrap();
    let pid = std::process::id();
    std::fs::write(
        dead1.join("session.json"),
        format!("{{\"pid\": {pid}, \"created_ms\": 1}}"),
    )
    .unwrap();

    // 伪造死会话 2：创建时间不符
    let dead2 = root.join("dead2");
    std::fs::create_dir_all(&dead2).unwrap();
    let pid = std::process::id();
    let created_ms = crate::proc::self_created_ms();
    std::fs::write(
        dead2.join("session.json"),
        format!(
            "{{\"pid\": {pid}, \"created_ms\": {}}}",
            created_ms + 60_000
        ),
    )
    .unwrap();

    // 无 pid 文件目录 → 删除
    let nofile = root.join("nofile");
    std::fs::create_dir_all(&nofile).unwrap();

    let removed = cleanup_orphans(&root);
    assert_eq!(removed, 3, "死会话 2 + 无文件 1 全部清理");
    assert!(!dead1.exists());
    assert!(!dead2.exists());
    assert!(!nofile.exists());
    // 活会话目录保留
    let live_dirs: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(live_dirs.len(), 1, "活会话保留");
}
