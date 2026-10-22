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

    // 年龄视作 0：覆盖「无 pid 文件 ⇒ 判死」分支（默认年龄见下条用例）
    let removed = cleanup_orphans_with(&root, std::time::Duration::ZERO);
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

/// 「刚 mkdir、还没写 session.json」的目录不得被误删（`new_session` 先建目录后写
/// pid 文件，另一实例此刻扫盘会撞上）——过了存活时间仍没有才算孤儿。
#[test]
fn fresh_dir_without_pid_file_is_kept_until_aged() {
    let dir = crate::testutil::temp_dir("temp-area-orphan-fresh");
    let root = dir.path().to_path_buf();
    let fresh = root.join("session-deadbeef-1-0");
    std::fs::create_dir_all(&fresh).unwrap();
    assert_eq!(
        cleanup_orphans(&root),
        0,
        "太新的无 pid 目录不得误删（进行中会话窗口）"
    );
    assert!(fresh.exists());
    assert_eq!(
        cleanup_orphans_with(&root, std::time::Duration::ZERO),
        1,
        "年龄视为 0 ⇒ 按孤儿清理（判死分支）"
    );
    assert!(!fresh.exists());
}

/// 生产场景规模（2026-10-19 用户实测 2115 个死会话 / 47 GB）：成规模的死会话
/// 必须被整批回收——pid 不存在（`is_alive` 判死）或无 pid 文件的都算。
#[test]
fn orphan_sweep_reclaims_dead_sessions_at_scale() {
    let dir = crate::testutil::temp_dir("temp-area-orphan-scale");
    let root = dir.path().to_path_buf();
    for i in 0..300 {
        // ① 有 pid 文件但**创建时间不符**（本进程 pid + created_ms=1 ⇒ 必判死：
        //    与用户机上「旧会话的 pid 早已退出/被复用」在判死口径上等价，
        //    且不依赖各环境对不存在 pid 的权限差异）
        let dead = root.join(format!("session-{:x}-{i:x}-0", std::process::id()));
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(
            dead.join("session.json"),
            format!("{{\"pid\": {}, \"created_ms\": 1}}", std::process::id()),
        )
        .unwrap();
        // ② 完全没有 pid 文件（强杀在 mkdir 与写文件之间）
        let bare = root.join(format!("session-{:x}-bare-{i:x}-0", std::process::id()));
        std::fs::create_dir_all(&bare).unwrap();
    }
    let removed = cleanup_orphans_with(&root, std::time::Duration::ZERO);
    assert_eq!(removed, 600, "成批死会话应全部回收");
    let left = std::fs::read_dir(&root).unwrap().flatten().count();
    assert_eq!(left, 0, "扫完不留目录，实留 {left}");
}

/// `adopt_file`：把已在盘上的一份副本**移动**进会话目录（不复制、不额外占空间），
/// 源文件随之消失；目标内容逐字节一致。
#[test]
fn adopt_file_moves_existing_copy() {
    let (dir, ta) = temp_session();
    let src = dir.path().join("spool.bin");
    std::fs::write(&src, b"decoded-entry-bytes").unwrap();
    let e = EntryNameSafe::sanitize("disk.qcow2").unwrap();

    let landed = ta
        .adopt_file(Path::new("/data/a.zip"), &e, &src)
        .expect("adopt 应成功");

    assert!(landed.starts_with(ta.session_dir()), "必须落在会话目录");
    assert_eq!(landed.file_name().unwrap(), "disk.qcow2");
    assert_eq!(std::fs::read(&landed).unwrap(), b"decoded-entry-bytes");
    assert!(!src.exists(), "移动后源副本必须消失（否则临时盘双份占用）");
    ta.cleanup().unwrap();
}

/// 物化写盘失败时必须删掉半截文件：否则几 GB 残留在临时盘里，下一次尝试继续撞
/// 空间不足（用户实测 2026-10-22）。
#[test]
fn materialize_removes_partial_file_on_write_error() {
    let (_d, ta) = temp_session();
    let e = EntryNameSafe::sanitize("broken.bin").unwrap();
    struct Fail;
    impl std::io::Read for Fail {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full (simulated)"))
        }
    }
    let err = ta
        .materialize(Path::new("x.zip"), &e, Fail)
        .expect_err("写盘失败必须报错");
    assert!(err.to_string().contains("disk full"));
    let target = ta
        .session_dir()
        .join(super::archive_hash(Path::new("x.zip")));
    let leftovers: Vec<_> = std::fs::read_dir(&target)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "不得留下半截文件：{leftovers:?}");
    ta.cleanup().unwrap();
}
