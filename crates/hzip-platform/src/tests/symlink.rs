use super::*;

#[test]
fn hardlink_roundtrip() {
    let dir = crate::testutil::temp_dir("symlink");
    let src = dir.join("src.txt");
    std::fs::write(&src, b"hello").unwrap();
    let dst = dir.join("dst.txt");
    create_hardlink(&src, &dst).unwrap();
    assert_eq!(std::fs::read(&dst).unwrap(), b"hello");
    assert_eq!(read_link(&dst).unwrap_or_default(), PathBuf::new()); // 硬链接非符号链接
    assert_eq!(symlink_kind(&dst).unwrap(), LinkKind::NotLink);
}

#[test]
fn degrade_chain() {
    let dir = crate::testutil::temp_dir("symlink-degrade");
    let target = dir.join("t.txt");
    std::fs::write(&target, b"payload").unwrap();
    let link = dir.join("l.txt");

    // 先探测特权可用性：Reject 策略成功 → 真符号链接；Unsupported → 无特权
    let probe = create_symlink(
        &target,
        &link,
        &SymlinkPolicy {
            fallback: Fallback::Reject,
        },
    );
    match probe {
        Ok(SymlinkOutcome::Created) => {
            // 特权可用：验证 symlink_kind
            assert_eq!(symlink_kind(&link).unwrap(), LinkKind::File);
            std::fs::remove_file(&link).unwrap();
        }
        Err(HzipError::Unsupported { .. }) => {
            // 无特权：Copy 降级 → 普通文件内容一致 + DegradedToCopy
            let out = create_symlink(
                &target,
                &link,
                &SymlinkPolicy {
                    fallback: Fallback::Copy,
                },
            );
            assert_eq!(out.unwrap(), SymlinkOutcome::DegradedToCopy);
            let md = std::fs::symlink_metadata(&link).unwrap();
            assert!(!md.file_type().is_symlink());
            assert_eq!(std::fs::read(&link).unwrap(), b"payload");
            // Reject → Err(Unsupported)
            let link2 = dir.join("l2.txt");
            let out2 = create_symlink(
                &target,
                &link2,
                &SymlinkPolicy {
                    fallback: Fallback::Reject,
                },
            );
            assert!(matches!(out2, Err(HzipError::Unsupported { .. })));
        }
        other => panic!("unexpected probe result: {other:?}"),
    }
}

#[test]
fn symlink_kind_not_link() {
    let dir = crate::testutil::temp_dir("symlink-kind");
    let f = dir.join("plain.txt");
    std::fs::write(&f, b"x").unwrap();
    assert_eq!(symlink_kind(&f).unwrap(), LinkKind::NotLink);
}

#[test]
#[cfg(unix)]
fn symlink_kind_unix() {
    let dir = crate::testutil::temp_dir("symlink-kind-u");
    let target = dir.join("t.txt");
    std::fs::write(&target, b"x").unwrap();
    let link = dir.join("l.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(symlink_kind(&link).unwrap(), LinkKind::File);
    let broken = dir.join("b.txt");
    std::os::unix::fs::symlink(dir.join("missing"), &broken).unwrap();
    assert_eq!(symlink_kind(&broken).unwrap(), LinkKind::Broken);
}
