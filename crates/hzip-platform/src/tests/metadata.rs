use super::*;
use std::time::Duration;

#[test]
fn attrs_roundtrip() {
    let dir = crate::testutil::temp_dir("meta");
    let f = dir.join("a.txt");
    std::fs::write(&f, b"x").unwrap();
    let now = OffsetDateTime::from_unix_timestamp_nanos(
        (OffsetDateTime::now_utc().unix_timestamp() - 3600) as i128 * 1_000_000_000,
    )
    .unwrap();
    let meta = FileMeta {
        read_only: true,
        hidden: true,
        system: true,
        executable: false,
        mtime: Some(now),
    };
    apply_meta(&f, &meta).unwrap();
    let got = read_meta(&f).unwrap();
    assert!(got.read_only);
    // 隐藏位：Windows 是真实属性位（可写可读回）；unix 由文件名 `.` 前缀决定，
    // `apply_meta` 对隐藏位是**文档化无操作**（02 §5.3.2 映射表），故 `a.txt`
    // 读回仍为非隐藏。unix 侧的文件名约定语义由 `hidden_unix_convention` 覆盖。
    #[cfg(windows)]
    assert!(got.hidden);
    #[cfg(not(windows))]
    assert!(!got.hidden, "unix 隐藏由文件名决定，apply_meta 不应改变它");
    #[cfg(windows)]
    assert!(got.system);
    #[cfg(not(windows))]
    assert!(!got.system); // unix 无系统位
    let got_m = got.mtime.unwrap();
    let want_m = now;
    let diff = (got_m - want_m).abs();
    assert!(
        diff < Duration::from_secs(2),
        "mtime 差 {diff:?} 超过 2 s（FAT 精度）"
    );
    // 清理属性，避免残留只读文件阻碍清理
    let clear = FileMeta {
        read_only: false,
        ..Default::default()
    };
    apply_meta(&f, &clear).ok();
}

#[test]
fn executable_semantics() {
    let dir = crate::testutil::temp_dir("meta-exe");
    let f = dir.join("tool.bat");
    std::fs::write(&f, b"@echo off").unwrap();
    #[cfg(windows)]
    assert!(read_meta(&f).unwrap().executable); // 扩展名推断
    let g = dir.join("tool.txt");
    std::fs::write(&g, b"x").unwrap();
    #[cfg(windows)]
    assert!(!read_meta(&g).unwrap().executable);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&g, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read_meta(&g).unwrap().executable);
        std::fs::set_permissions(&g, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!read_meta(&g).unwrap().executable);
    }
}

#[test]
fn hidden_unix_convention() {
    let dir = crate::testutil::temp_dir("meta-hidden");
    let f = dir.join(".hidden");
    std::fs::write(&f, b"x").unwrap();
    let g = dir.join("visible");
    std::fs::write(&g, b"x").unwrap();
    // 02 §5.3.2 映射表：unix 隐藏 = 文件名以 `.` 开头（约定，读取时判定）；
    // Windows 隐藏 = FILE_ATTRIBUTE_HIDDEN，点前缀不自动置位。
    #[cfg(not(windows))]
    {
        assert!(read_meta(&f).unwrap().hidden, "unix 点前缀应判隐藏");
        assert!(!read_meta(&g).unwrap().hidden);
    }
    #[cfg(windows)]
    {
        assert!(!read_meta(&f).unwrap().hidden, "win 点前缀不自动隐藏");
        assert!(!read_meta(&g).unwrap().hidden);
        // 显式置隐藏位 → 读回 true（win 属性位语义）
        let meta = FileMeta {
            hidden: true,
            ..Default::default()
        };
        apply_meta(&g, &meta).unwrap();
        assert!(read_meta(&g).unwrap().hidden);
        let clear = FileMeta {
            hidden: false,
            ..Default::default()
        };
        apply_meta(&g, &clear).ok();
    }
}

/// set_mtime：文件 + 目录（win BACKUP_SEMANTICS 句柄；T32 例外新增）。
#[test]
fn set_mtime_file_and_dir() {
    let dir = crate::testutil::temp_dir("meta-mtime");
    let f = dir.path().join("a.txt");
    std::fs::write(&f, b"x").unwrap();
    let sub = dir.path().join("subdir");
    std::fs::create_dir_all(&sub).unwrap();
    let target = OffsetDateTime::from_unix_timestamp_nanos(1_767_225_600_000_000_000).unwrap();
    // 文件
    set_mtime(&f, target).unwrap();
    let got_f = read_meta(&f).unwrap().mtime.unwrap();
    assert!(
        (got_f - target).abs() < std::time::Duration::from_secs(2),
        "文件 mtime 差过大: {got_f} vs {target}"
    );
    // 目录（win 需 BACKUP_SEMANTICS——本测试即回归）
    set_mtime(&sub, target).unwrap();
    let got_d = read_meta(&sub).unwrap().mtime.unwrap();
    assert!(
        (got_d - target).abs() < std::time::Duration::from_secs(2),
        "目录 mtime 差过大: {got_d} vs {target}"
    );
    // unix：权限不被默认调整（目录保持可遍历）
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&sub).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o111,
            0o111,
            "目录执行位应保持（set_mtime 不动权限）"
        );
    }
}
