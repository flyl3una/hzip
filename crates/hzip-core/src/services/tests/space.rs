use super::*;

/// 磁盘满错误：码必须是 1002（文案/修复指引都由码派生——两侧别漂移）。
#[test]
fn disk_full_error_maps_to_hz_1002() {
    use hzip_error::code;
    let e = disk_full_error(Some(PathBuf::from("/tmp")));
    assert_eq!(e.code(), code::io::DISK_FULL);
    assert!(
        e.to_string().contains("/tmp"),
        "Display 必须带落点路径（用户要知道哪个目录满了）：{e}"
    );
}

/// 预检判定：不足（含 1.05 系数边界）与充足两侧。
#[test]
fn space_shortfall_boundary() {
    assert!(space_shortfall(1000, 0));
    assert!(space_shortfall(1000, 1049)); // 1049 < 1050
    assert!(!space_shortfall(1000, 1050)); // 恰好满足
    assert!(!space_shortfall(1000, u64::MAX));
    assert!(!space_shortfall(0, 0)); // need == 0 → 不放行也不拦（交给调用方跳过）
}

/// 非组合源不触发预检（单格式档案打开路径不受影响）。
#[test]
fn single_source_skips_precheck() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.zip");
    std::fs::write(&p, b"PK\x03\x04").unwrap();
    let reg = hzip_format::FormatRegistry::new();
    // 空注册表：sniff 报 Unsupported？——直接断言「不因预检报磁盘错」：
    // 预检只认 File+Composite，这里根本走不到可用空间查询
    let r = precheck_open_temp_space(&reg, &Source::File(p));
    assert!(
        !matches!(&r, Err(HzipError::Io { .. })),
        "非组合源不得因磁盘预检失败：{r:?}"
    );
}

/// 孤儿回收：只删**超龄**的 `hzip-spool-*`，新缓冲与非前缀文件不动。
#[test]
fn sweep_removes_only_stale_spools() {
    let dir = tempfile::tempdir().unwrap();
    let stale = dir.path().join(format!("{SPOOL_PREFIX}stale"));
    let fresh = dir.path().join(format!("{SPOOL_PREFIX}fresh"));
    let other = dir.path().join("hzip-other.tmp");
    for p in [&stale, &fresh, &other] {
        std::fs::write(p, b"x").unwrap();
    }
    let old = std::time::SystemTime::now() - Duration::from_secs(48 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let removed = sweep_stale_spools_in(dir.path(), SPOOL_STALE_AFTER);
    assert_eq!(removed, 1, "恰好回收一个孤儿");
    assert!(!stale.exists(), "超龄孤儿必须被回收");
    assert!(fresh.exists(), "新鲜缓冲不得被回收");
    assert!(other.exists(), "非前缀文件不得被回收");
    // 幂等：再扫一遍无事发生
    assert_eq!(sweep_stale_spools_in(dir.path(), SPOOL_STALE_AFTER), 0);
}
