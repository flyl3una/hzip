//! 写侧用例（计划 `T-IMG11` 步骤 1 的 `tests/write.rs`）。
//!
//! 三件事在这里被钉死：
//!
//! 1. **五类变更往返 + 重挂**：`apply` 的改动先落在临时镜像上，`flush()` 才回放到设备；
//!    重挂用的是**同一台设备**，故“重挂后还在”等价于“回放真的发生了”；
//! 2. **回放前的设备不变**：取消即丢的语义（覆盖写不放宽）；
//! 3. **空间不足 = 可读理由**，不是 panic，且新建的空文件会被回滚。

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use hzip_format::Change;
use hzip_vol::VolError;

/// 五类变更一条不落，写完 `audit()`，再把设备重挂一次核对。
#[test]
fn five_changes_round_trip_and_survive_remount() {
    let dir = tempfile::tempdir().expect("临时目录");
    let replaced = local_file(dir.path(), "replaced.bin", b"REPLACED-CONTENT");
    let appended = local_file(dir.path(), "appended.bin", b"APPENDED-CONTENT");

    let image = ntfs_image(&[
        ("keep.txt", b"keep"),
        ("edit.txt", b"old"),
        ("gone.txt", b"bye"),
        ("dir/inner.txt", b"in"),
    ]);
    let dev = device(image.clone());

    {
        let mut fs = mount(dev.clone(), &writable()).expect("挂载");
        assert!(fs.caps().write, "干净卷必须可写");

        // ① Replace（已存在）
        fs.apply(
            &Change::Replace {
                entry: id_of(&fs, "edit.txt"),
                src: replaced.clone(),
            },
            Some(&replaced),
            &cancel(),
        )
        .expect("Replace");

        // ② Append（新文件，父目录已存在）
        fs.apply(
            &Change::Append {
                src: appended.clone(),
                name: name("dir/new.txt"),
            },
            Some(&appended),
            &cancel(),
        )
        .expect("Append");

        // ③ MakeDir（中间目录不存在 ⇒ 逐级建）
        fs.apply(
            &Change::MakeDir {
                name: name("made/sub/deep"),
            },
            None,
            &cancel(),
        )
        .expect("MakeDir");

        // ④ Rename（同目录只换 basename）
        fs.apply(
            &Change::Rename {
                from: name("gone.txt"),
                to: name("renamed.txt"),
            },
            None,
            &cancel(),
        )
        .expect("Rename");

        // ⑤ Delete（文件）
        fs.apply(&Change::Delete(id_of(&fs, "keep.txt")), None, &cancel())
            .expect("Delete");

        // 改动只在临时镜像里：设备此刻必须与挂载时**逐字节相同**（取消即丢的语义）。
        assert_eq!(
            bytes_of(&dev),
            image,
            "flush 之前设备一个字节都不该动（覆盖写语义）"
        );

        // 另一个挂载看到的是**设备上**的老卷：未提交的改动不泄漏。
        {
            let other = mount(dev.clone(), &writable()).expect("第二个挂载");
            assert_eq!(read_all(&other, id_of(&other, "edit.txt"), 3), b"old");
            assert!(
                entries(&other).by_name("renamed.txt").is_none(),
                "未 flush ⇒ 别的挂载看不到改名"
            );
            assert!(
                entries(&other).by_name("made").is_none(),
                "未 flush ⇒ 别的挂载看不到新目录"
            );
        }

        fs.flush().expect("flush");
    }

    // 重挂（第二次）：改动现在必须在设备上。
    let mut fs = mount(dev, &writable()).expect("二次重挂");
    let tree = entries(&fs);
    assert_eq!(
        names(&tree),
        vec![
            "dir",
            "dir/inner.txt",
            "dir/new.txt",
            "edit.txt",
            "made",
            "made/sub",
            "made/sub/deep",
            "renamed.txt",
        ],
        "五类变更后的树"
    );
    assert_eq!(
        tree.by_name("edit.txt").unwrap().size_raw,
        16,
        "Replace 后的大小"
    );
    assert_eq!(
        read_all(&fs, id_of(&fs, "edit.txt"), 16),
        b"REPLACED-CONTENT"
    );
    assert_eq!(
        read_all(&fs, id_of(&fs, "dir/new.txt"), 16),
        b"APPENDED-CONTENT"
    );
    assert_eq!(
        read_all(&fs, id_of(&fs, "renamed.txt"), 3),
        b"bye",
        "改名保内容"
    );
    let summary = fs.audit().expect("audit");
    assert!(!summary.skipped, "上游 fsck 只读子集应当跑完了");
    assert!(
        summary.findings.is_empty(),
        "写入后的卷不该有 fsck findings：{:?}",
        summary.findings
    );
    // 收尾：再验证一次 flush 之后的设备是稳定可写的。
    fs.apply(
        &Change::MakeDir {
            name: name("after"),
        },
        None,
        &cancel(),
    )
    .expect("flush 后再写");
    fs.flush().expect("二次 flush");
}

#[test]
fn rename_preserves_content_and_times() {
    let dir = tempfile::tempdir().expect("临时目录");
    let src = local_file(dir.path(), "src.txt", b"1234567890");
    let image = ntfs_image(&[("old.txt", b"")]);
    let dev = device(image);
    let mut fs = mount(dev, &writable()).expect("挂载");

    // 先写内容并钉上本地 mtime，改名后内容与时间都该还在。
    fs.apply(
        &Change::Replace {
            entry: id_of(&fs, "old.txt"),
            src: src.clone(),
        },
        Some(&src),
        &cancel(),
    )
    .expect("Replace");
    let before = entry_of(&fs, "old.txt");
    assert_eq!(before.size_raw, 10);
    assert_eq!(read_all(&fs, id_of(&fs, "old.txt"), 10), b"1234567890");
    let mtime_before = before.mtime.expect("mtime 应当有值");

    fs.apply(
        &Change::Rename {
            from: name("old.txt"),
            to: name("new.txt"),
        },
        None,
        &cancel(),
    )
    .expect("Rename");

    let after = entry_of(&fs, "new.txt");
    assert_eq!(after.size_raw, 10, "改名保内容");
    assert_eq!(read_all(&fs, id_of(&fs, "new.txt"), 10), b"1234567890");
    let mtime_after = after.mtime.expect("mtime 应当有值");
    // 上游口径：改名会刷新索引项里的时间戳（与 Windows 一致），故只要求“不倒退”。
    assert!(
        mtime_after >= mtime_before,
        "改名后时间戳不该倒退（上游会刷新为当下）：{mtime_before:?} → {mtime_after:?}"
    );
    assert!(entries(&fs).by_name("old.txt").is_none(), "旧名应当消失");
}

/// 跨目录改名：文件走 `link` + `unlink`（同一条记录改挂名字），目录如实拒。
#[test]
fn rename_across_directories_uses_link_and_unlink() {
    let image = ntfs_image(&[("a/f.txt", b"cross-dir content"), ("b/other.txt", b"other")]);
    let dev = device(image);
    let mut fs = mount(dev.clone(), &writable()).expect("挂载");

    fs.apply(
        &Change::Rename {
            from: name("a/f.txt"),
            to: name("b/f2.txt"),
        },
        None,
        &cancel(),
    )
    .expect("跨目录改名");

    let tree = entries(&fs);
    assert!(tree.by_name("a/f.txt").is_none(), "旧路径消失");
    let moved = tree.by_name("b/f2.txt").expect("新路径存在");
    assert_eq!(moved.size_raw, 17);
    assert_eq!(read_all(&fs, moved.id, 17), b"cross-dir content");

    // 目录不能跨目录搬（NTFS 不能硬链目录）⇒ 如实拒。
    let err = fs
        .apply(
            &Change::Rename {
                from: name("a"),
                to: name("b/a"),
            },
            None,
            &cancel(),
        )
        .expect_err("搬目录必须被拒");
    assert!(
        matches!(err, VolError::Unsupported("vol.err.dir-move")),
        "得到 {err:?}"
    );

    // 重挂后仍然成立（link+unlink 落盘了）。
    fs.flush().expect("flush");
    let fs2 = mount(dev, &writable()).expect("重挂");
    assert_eq!(
        read_all(&fs2, id_of(&fs2, "b/f2.txt"), 17),
        b"cross-dir content"
    );
}

/// 空间不足 ⇒ `vol.err.no-space`（不是 panic），且**新建的空文件被回滚**。
#[test]
fn full_volume_gives_readable_reason() {
    let dir = tempfile::tempdir().expect("临时目录");
    // 32 MiB 的卷，要塞 40 MiB 的内容。
    let big = local_file_sized(dir.path(), "big.bin", 40 * 1024 * 1024);
    let dev = device(ntfs_image(&[("seed.txt", b"seed")]));
    let mut fs = mount(dev, &writable()).expect("挂载");

    let err = fs
        .apply(
            &Change::Append {
                src: big,
                name: name("too-big.bin"),
            },
            None,
            &cancel(),
        )
        .expect_err("盘满必须报错");
    assert!(
        matches!(err, VolError::Quota("vol.err.no-space")),
        "盘满要给可读理由键，得到 {err:?}"
    );

    // 回滚：失败后不该留下半个空文件。
    let tree = entries(&fs);
    assert!(
        tree.by_name("too-big.bin").is_none(),
        "写失败必须回滚刚建的文件；实际 {:?}",
        names(&tree)
    );
    assert!(tree.by_name("seed.txt").is_some(), "卷本身仍然可列");
}

#[test]
fn append_refuses_a_name_that_already_exists() {
    let dir = tempfile::tempdir().expect("临时目录");
    let src = local_file(dir.path(), "x.txt", b"x");
    let dev = device(ntfs_image(&[("dup.txt", b"dup")]));
    let mut fs = mount(dev, &writable()).expect("挂载");
    let err = fs
        .apply(
            &Change::Append {
                src,
                name: name("dup.txt"),
            },
            None,
            &cancel(),
        )
        .expect_err("重名必须被拒");
    assert!(
        matches!(err, VolError::Unsupported("vol.err.exists")),
        "得到 {err:?}"
    );
}

#[test]
fn delete_of_a_non_empty_directory_is_refused() {
    let dev = device(ntfs_image(&[("dir/a.txt", b"a")]));
    let mut fs = mount(dev, &writable()).expect("挂载");
    let err = fs
        .apply(&Change::Delete(id_of(&fs, "dir")), None, &cancel())
        .expect_err("非空目录不能删");
    assert!(
        matches!(err, VolError::Unsupported("vol.err.dir-not-empty")),
        "得到 {err:?}"
    );
    // 空目录可以删。
    fs.apply(
        &Change::MakeDir {
            name: name("empty"),
        },
        None,
        &cancel(),
    )
    .expect("MakeDir");
    fs.apply(&Change::Delete(id_of(&fs, "empty")), None, &cancel())
        .expect("删空目录");
    assert!(entries(&fs).by_name("empty").is_none());
}

/// 只读挂载下的每一次 `apply` 都要带理由拒绝（不是静默无效）。
#[test]
fn applying_on_a_read_only_mount_reports_the_reason() {
    let dir = tempfile::tempdir().expect("临时目录");
    let src = local_file(dir.path(), "x.txt", b"x");
    let dev = device(ntfs_image(&[]));
    let mut fs = mount(dev, &read_only()).expect("只读挂载");
    let err = fs
        .apply(
            &Change::Append {
                src,
                name: name("nope.txt"),
            },
            None,
            &cancel(),
        )
        .expect_err("只读卷不许写");
    assert!(matches!(err, VolError::ReadOnly(_)), "得到 {err:?}");
    fs.flush().expect("只读卷的 flush 是空操作");
}

#[test]
fn change_set_runs_in_order() {
    // `ChangeSet` 的批语义：同一批里先建目录再往里写文件。
    let dir = tempfile::tempdir().expect("临时目录");
    let src = local_file(dir.path(), "f.txt", b"batch");
    let dev = device(ntfs_image(&[]));
    let mut fs = mount(dev, &writable()).expect("挂载");
    let changes = hzip_format::ChangeSet {
        changes: vec![
            Change::MakeDir {
                name: name("batch-dir"),
            },
            Change::Append {
                src,
                name: name("batch-dir/inner.txt"),
            },
        ],
    };
    for change in &changes.changes {
        fs.apply(change, None, &cancel()).expect("批次内逐条执行");
    }
    assert_eq!(
        read_all(&fs, id_of(&fs, "batch-dir/inner.txt"), 5),
        b"batch"
    );
}
