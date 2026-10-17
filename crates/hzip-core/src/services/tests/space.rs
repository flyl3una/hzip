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

/// 孤儿回收**扫两处**：`temp_dir()`（zip/cpio/tar 缓冲）与 `temp_root()`
/// （卷容器缓冲 = `temp_dir()/hzip`，Linux 上 `XDG_RUNTIME_DIR/hzip`）。
///
/// 2026-10-17 核对发现：卷容器的 spool 落在 `temp_root()`，而回收只扫 `temp_dir()`
/// ⇒ 卷容器的孤儿从未被回收（40 GB 镜像的缓冲崩一次就永久占盘）。本用例把两处
/// 各放一个超龄孤儿，断言 `sweep_stale_spools`（非注入版）两处都收到。
#[test]
fn sweep_covers_both_spool_roots() {
    let root = hzip_platform::dirs::temp_root();
    let created = !root.exists();
    std::fs::create_dir_all(&root).unwrap();
    let stale_vol = root.join(format!("{SPOOL_PREFIX}vol-stale-{}", std::process::id()));
    let stale_tmp =
        std::env::temp_dir().join(format!("{SPOOL_PREFIX}tmp-stale-{}", std::process::id()));
    for p in [&stale_vol, &stale_tmp] {
        std::fs::write(p, b"x").unwrap();
    }
    let old = std::time::SystemTime::now() - Duration::from_secs(48 * 3600);
    for p in [&stale_vol, &stale_tmp] {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let removed = sweep_stale_spools(SPOOL_STALE_AFTER);
    assert!(
        !stale_vol.exists(),
        "卷容器缓冲（{root:?} 下）必须被回收：removed={removed}"
    );
    assert!(
        !stale_tmp.exists(),
        "zip/cpio/tar 缓冲（temp_dir 下）必须被回收"
    );
    // 自清理：不把开发机上的 `{temp}/hzip` 留在那里——那正是本仓 8 个测试 target
    // 在 CI 上红、本地恒绿的成因（本地目录早被历史运行建好）。
    if created {
        let _ = std::fs::remove_dir(&root);
    }
}
