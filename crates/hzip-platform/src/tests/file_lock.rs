use super::*;

fn tmp_lock() -> (crate::testutil::TempDir, PathBuf) {
    let d = crate::testutil::temp_dir("file-lock");
    let p = d.join("test.lock");
    (d, p)
}

#[test]
fn exclusive_second_fails_then_releases() {
    let (_d, p) = tmp_lock();
    // 首锁成功
    let g1 = try_lock_exclusive(&p).expect("first lock");
    // 二次锁失败（排他已持——单实例语义）
    assert!(
        try_lock_exclusive(&p).is_err(),
        "同进程二次排他应失败（已有实例语义）"
    );
    drop(g1);
    // 释放后可重获（进程退出自动释放语义）
    let _g2 = try_lock_exclusive(&p).expect("relock after drop");
}

#[test]
fn lock_file_owner_written() {
    let (_d, p) = tmp_lock();
    let g = try_lock_exclusive(&p).expect("lock");
    drop(g); // 释放锁后可读（锁区域读写互斥是 LockFileEx 预期）
    let content = std::fs::read_to_string(&p).expect("read");
    let pid: u32 = content
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .expect("pid in lock");
    assert_eq!(pid, std::process::id());
}
