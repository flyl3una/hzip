//! XFS 写侧契约测试：五类 `Change` + 重挂复核 + 能力与理由 + 按名拒绝。
//!
//! 「重挂复核」的做法：写完之后 `flush()`（干净卸载）→ 在**同一份字节的副本**上重新
//! 挂载读一遍——这正是「改动真的落到卷上，而不只是这次挂载的内存印象」的判据。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use std::path::Path;

use common::*;
use hzip_format::Change;
use hzip_vol::{FsDriver, ReadOnlyReason, VolError};

/// 写回之后重新挂载（只读）复核。
fn remount(image: &SparseDevice) -> Box<dyn hzip_vol::FsVolume> {
    mount_ro(&image.snapshot())
}

/// `Replace`：内容替换（变长、变短、变长），重挂可见，且**保本地 mtime**。
#[test]
fn replace_rewrites_content_and_remounts() {
    let img = image(&[
        Node::File("a.txt", b"0123456789"),
        Node::File("dir/b.txt", b"xx"),
    ]);
    let mut fs = mount_rw(&img);
    let (_tmp, src) = local_file("new.txt", b"short");

    fs.apply(
        &Change::Replace {
            entry: id_of(&*fs, "a.txt"),
            src: src.clone(),
        },
        None,
        &cancel(),
    )
    .unwrap();
    // 变短：`replace` 必须先截断（上游 `write` 只覆盖既有字节，不会缩短文件）。
    assert_eq!(read_name(&*fs, "a.txt"), b"short");
    assert_eq!(fs.size_of(id_of(&*fs, "a.txt")).unwrap(), 5);

    // 变长：再来一次。
    let (_tmp2, src2) = local_file("longer.txt", &vec![b'z'; 100 * 1024]);
    fs.apply(
        &Change::Replace {
            entry: id_of(&*fs, "a.txt"),
            src: src2,
        },
        None,
        &cancel(),
    )
    .unwrap();
    assert_eq!(fs.size_of(id_of(&*fs, "a.txt")).unwrap(), 100 * 1024);
    fs.flush().unwrap();
    drop(fs);

    let ro = remount(&img);
    assert_eq!(
        read_name(&*ro, "a.txt"),
        vec![b'z'; 100 * 1024],
        "重挂必须看得到替换后的内容"
    );
    let e = tree(&*ro)
        .entries
        .into_iter()
        .find(|e| e.name.as_str() == "a.txt")
        .unwrap();
    assert_eq!(
        e.mtime.unwrap().unix_timestamp(),
        FIXED_MTIME_SECS as i64,
        "本地文件 mtime 要保真（`change_attributes`）"
    );
    assert!(ro.audit().unwrap().is_clean(), "写完之后卷必须自洽");
}

/// 跨块（> 8 MiB = 逐块写的块大小）的 `Replace`：每个字节都要对，重挂也一样。
#[test]
fn replace_of_a_multi_chunk_file_keeps_every_byte() {
    let img = image(&[Node::File("big.bin", b"x")]);
    let mut fs = mount_rw(&img);
    // 9 MiB ⇒ 必过 COPY_CHUNK 的边界（两次 `write`，两条 extent）。
    let data: Vec<u8> = (0..9 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let (_tmp, src) = local_file("big.bin", &data);
    fs.apply(
        &Change::Replace {
            entry: id_of(&*fs, "big.bin"),
            src,
        },
        None,
        &cancel(),
    )
    .unwrap();
    assert_eq!(
        fs.size_of(id_of(&*fs, "big.bin")).unwrap(),
        data.len() as u64
    );
    assert_eq!(read_name(&*fs, "big.bin"), data);
    fs.flush().unwrap();
    drop(fs);

    let ro = remount(&img);
    assert_eq!(read_name(&*ro, "big.bin"), data, "重挂后每个字节都要对");
    assert!(ro.audit().unwrap().is_clean());
}

/// `Replace` 的目标不在卷里（id 快照过期）⇒ 建出来再写，而不是整个编辑失败。
#[test]
fn replace_of_a_missing_entry_creates_it() {
    let img = image(&[Node::File("a.txt", b"a")]);
    let mut fs = mount_rw(&img);
    let id = id_of(&*fs, "a.txt");
    // 先把目标删掉（此时 id 已经悬空），再 Replace 它。
    fs.apply(&Change::Delete(id), None, &cancel()).unwrap();
    let (_tmp, src) = local_file("new.txt", b"reborn");
    fs.apply(&Change::Replace { entry: id, src }, None, &cancel())
        .unwrap();
    fs.flush().unwrap();
    drop(fs);

    let ro = remount(&img);
    assert_eq!(read_name(&*ro, "a.txt"), b"reborn");
}

/// `Append`：新条目（含逐级隐式目录）、已存在 ⇒ 覆盖其内容。
#[test]
fn append_creates_nested_paths_and_overwrites_existing() {
    let img = image(&[Node::File("keep.txt", b"keep")]);
    let mut fs = mount_rw(&img);

    let (_tmp, src) = local_file("n.txt", b"nested");
    fs.apply(
        &Change::Append {
            src,
            name: "new/deep/leaf.txt".into(),
        },
        None,
        &cancel(),
    )
    .unwrap();

    let (_tmp2, src2) = local_file("o.txt", b"over");
    fs.apply(
        &Change::Append {
            src: src2,
            name: "keep.txt".into(),
        },
        None,
        &cancel(),
    )
    .unwrap();
    assert_eq!(
        read_name(&*fs, "keep.txt"),
        b"over",
        "同名 ⇒ 覆盖（zip -a 语义）"
    );

    // 名字以 `/` 结尾 ⇒ 只建目录。
    fs.apply(
        &Change::Append {
            src: Path::new("").to_path_buf(),
            name: "made/by/append/".into(),
        },
        None,
        &cancel(),
    )
    .unwrap();
    fs.flush().unwrap();
    drop(fs);

    let ro = remount(&img);
    assert_eq!(read_name(&*ro, "new/deep/leaf.txt"), b"nested");
    assert_eq!(read_name(&*ro, "keep.txt"), b"over");
    assert!(tree(&*ro).by_name("made/by/append").unwrap().is_dir);
    assert!(ro.audit().unwrap().is_clean());
    drop((_tmp, _tmp2));
}

/// `MakeDir`：逐级建（父目录不存在也建）；已存在 ⇒ 幂等成功。
#[test]
fn make_dir_builds_parents_and_is_idempotent() {
    let img = image(&[Node::File("a.txt", b"a")]);
    let mut fs = mount_rw(&img);
    for _ in 0..2 {
        fs.apply(
            &Change::MakeDir {
                name: "x/y/z".into(),
            },
            None,
            &cancel(),
        )
        .unwrap();
    }
    assert!(
        fs.entries(&limit(), &cancel())
            .unwrap()
            .by_name("x/y/z")
            .unwrap()
            .is_dir
    );
    fs.flush().unwrap();
    drop(fs);

    let ro = remount(&img);
    let t = tree(&*ro);
    assert!(t.by_name("x").unwrap().is_dir);
    assert!(t.by_name("x/y").unwrap().is_dir);
    assert!(t.by_name("x/y/z").unwrap().is_dir);
    assert!(ro.audit().unwrap().is_clean());
}

/// `Delete`：文件删掉；空目录删掉；**非空目录按名拒绝**（不递归删）。
#[test]
fn delete_removes_files_and_empty_dirs_but_refuses_non_empty() {
    let img = image(&[
        Node::File("a.txt", b"a"),
        Node::File("full/inner.txt", b"i"),
        Node::Dir("empty-dir"),
    ]);
    let mut fs = mount_rw(&img);

    let err = fs
        .apply(&Change::Delete(id_of(&*fs, "full")), None, &cancel())
        .unwrap_err();
    assert!(
        matches!(err, VolError::Unsupported("vol.err.dir-not-empty")),
        "非空目录必须按名拒绝：{err:?}"
    );

    fs.apply(&Change::Delete(id_of(&*fs, "a.txt")), None, &cancel())
        .unwrap();
    fs.apply(&Change::Delete(id_of(&*fs, "empty-dir")), None, &cancel())
        .unwrap();
    fs.flush().unwrap();
    drop(fs);

    let ro = remount(&img);
    let t = tree(&*ro);
    assert!(t.by_name("a.txt").is_none(), "删掉的文件不该回来");
    assert!(t.by_name("empty-dir").is_none());
    assert!(t.by_name("full").unwrap().is_dir, "非空目录要原样留着");
    assert_eq!(read_name(&*ro, "full/inner.txt"), b"i");
    assert!(ro.audit().unwrap().is_clean());
}

/// `Rename`：同目录改名（含 `to` 带父目录段的写法）；跨目录按名拒绝。
#[test]
fn rename_within_a_directory_and_refusal_across_directories() {
    let img = image(&[
        Node::File("dir/a.txt", b"content"),
        Node::File("other/b.txt", b"b"),
    ]);
    let mut fs = mount_rw(&img);

    fs.apply(
        &Change::Rename {
            from: "dir/a.txt".into(),
            to: "renamed.txt".into(),
        },
        None,
        &cancel(),
    )
    .unwrap();
    assert_eq!(read_name(&*fs, "dir/renamed.txt"), b"content");

    // `to` 含 `/` 但父目录与 `from` 相同 ⇒ 仍算「同目录改名」，允许。
    fs.apply(
        &Change::Rename {
            from: "dir/renamed.txt".into(),
            to: "dir/final.txt".into(),
        },
        None,
        &cancel(),
    )
    .unwrap();
    assert_eq!(read_name(&*fs, "dir/final.txt"), b"content");

    // 跨目录搬移 ⇒ 上游只有同目录改名，本驱动按名拒绝。
    let err = fs
        .apply(
            &Change::Rename {
                from: "dir/final.txt".into(),
                to: "other/final.txt".into(),
            },
            None,
            &cancel(),
        )
        .unwrap_err();
    assert!(
        matches!(err, VolError::Unsupported("vol.err.dir-move")),
        "跨目录改名必须按名拒绝：{err:?}"
    );

    // 按名引用一个不存在的条目 ⇒ 稳定键拒绝（不猜路径）。
    let err = fs
        .apply(
            &Change::Rename {
                from: "nope.txt".into(),
                to: "x.txt".into(),
            },
            None,
            &cancel(),
        )
        .unwrap_err();
    assert!(
        matches!(err, VolError::Unsupported("vol.err.entry-gone")),
        "查不到源必须如实拒绝：{err:?}"
    );

    fs.flush().unwrap();
    drop(fs);

    let ro = remount(&img);
    assert_eq!(read_name(&*ro, "dir/final.txt"), b"content");
    assert!(tree(&*ro).by_name("dir/a.txt").is_none());
    assert!(ro.audit().unwrap().is_clean());
}

/// 目录已超出**短格式**（条目多到装不进 inode）⇒ 上游的改名不覆盖它，本驱动按名拒绝。
///
/// 短格式 vs 块格式是上游的口径（`Format::Local` = 目录内容就在 inode 里）；本驱动
/// **自己先判形态**，好过按上游的错误文本猜键。
#[test]
fn rename_in_a_block_format_directory_is_refused_by_key() {
    // 足够多的顶层条目 ⇒ 创建过程会把 root 目录从短格式转成块格式。
    let names: Vec<String> = (0..40).map(|i| format!("file-{i:03}-pad.txt")).collect();
    let nodes: Vec<Node<'_>> = names.iter().map(|n| Node::File(n.as_str(), b"x")).collect();
    let img = image(&nodes);
    let mut fs = mount_rw(&img);
    assert_eq!(tree(&*fs).entries.len(), 40);

    let err = fs
        .apply(
            &Change::Rename {
                from: names[0].as_str().into(),
                to: "renamed.txt".into(),
            },
            None,
            &cancel(),
        )
        .err()
        .unwrap();
    assert!(
        matches!(err, VolError::Unsupported("vol.unsupported.dir-too-large")),
        "块格式目录里的改名必须按名拒绝：{err:?}"
    );
    // 拒绝之后卷还是干净的（没有半改）。
    assert!(fs.audit().unwrap().is_clean());
}

/// 能力集：可写卷写侧全开；`flush` 之后卷是**干净**的（可写重挂不再被脏日志挡住）。
#[test]
fn writable_caps_and_flush_leaves_a_clean_volume() {
    let img = image(&[Node::File("a.txt", b"a")]);
    let mut fs = mount_rw(&img);
    let caps = fs.caps();
    assert!(caps.write && caps.delete && caps.rename && caps.mkdir && caps.symlink);
    assert!(caps.times && caps.mode);
    assert_eq!(caps.name_max, hzip_fs_xfs::NAME_MAX);
    assert_eq!(caps.read_only_reason, None);

    let (_tmp, src) = local_file("b.txt", b"b");
    fs.apply(
        &Change::Append {
            src,
            name: "b.txt".into(),
        },
        None,
        &cancel(),
    )
    .unwrap();
    // 还没 flush：日志里有未落盘的记录 ⇒ 以**可写**方式重挂会被拒（这正是 flush 存在的理由）。
    let dirty_dev: std::sync::Arc<dyn hzip_vol::BlockDevice> = std::sync::Arc::new(img.snapshot());
    let dirty = hzip_fs_xfs::XfsDriver::new().mount(
        dirty_dev,
        &hzip_vol::FsOptions {
            writable: true,
            ..Default::default()
        },
    );
    // 本挂载自己的记录还在内存里（push 过一版），设备上看到的是「记录未重放」——如实承认：
    // 此时要可写重挂，先得 flush。
    if let Ok(other) = dirty {
        assert!(
            !other.caps().write || other.caps().read_only_reason.is_some(),
            "未 flush 的卷不该被当成干净卷"
        );
    }

    fs.flush().unwrap();
    drop(fs);

    // flush 之后：可写重挂成功（日志已用 unmount 记录收尾）。
    let dev: std::sync::Arc<dyn hzip_vol::BlockDevice> = std::sync::Arc::new(img.snapshot());
    let mut again = hzip_fs_xfs::XfsDriver::new()
        .mount(
            dev,
            &hzip_vol::FsOptions {
                writable: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(again.caps().write, "干净卷必须能可写重挂");
    assert_eq!(again.caps().read_only_reason, None);
    assert_eq!(read_name(&*again, "b.txt"), b"b");
    assert!(again.audit().unwrap().is_clean());
    // 干净卷上再写一笔，仍能走通。
    let (_tmp2, src2) = local_file("c.txt", b"c");
    again
        .apply(
            &Change::Append {
                src: src2,
                name: "c.txt".into(),
            },
            None,
            &cancel(),
        )
        .unwrap();
}

/// 只读卷上的五类变更全部按理由拒绝（理由来自 caps，不是写起来才知道的错）。
#[test]
fn read_only_mount_refuses_every_change() {
    let img = image(&[Node::File("a.txt", b"a")]);
    let (_tmp, src) = local_file("n.txt", b"n");
    let mut fs = mount_ro(&img);
    assert!(!fs.caps().write);
    assert_eq!(fs.caps().read_only_reason, Some(ReadOnlyReason::BySetting));

    let id = id_of(&*fs, "a.txt");
    let changes = [
        Change::Replace {
            entry: id,
            src: src.clone(),
        },
        Change::Delete(id),
        Change::Append {
            src: src.clone(),
            name: "b.txt".into(),
        },
        Change::Rename {
            from: "a.txt".into(),
            to: "c.txt".into(),
        },
        Change::MakeDir { name: "d".into() },
    ];
    for change in changes {
        let err = fs.apply(&change, None, &cancel()).unwrap_err();
        assert!(
            matches!(err, VolError::ReadOnly(ReadOnlyReason::BySetting)),
            "{change:?} 必须以同一个理由拒绝：{err:?}"
        );
    }
    assert_eq!(img.writes(), 0, "只读卷上的拒绝不许碰设备");
    fs.flush().unwrap(); // 只读 flush 是无操作（不写 unmount 记录）
    assert_eq!(img.writes(), 0);
}

/// 取消：进 `apply` 之前就取消 ⇒ `Cancelled`，且设备一个字节都不动。
#[test]
fn cancelling_apply_reports_cancelled_before_touching_the_device() {
    let img = image(&[Node::File("a.txt", b"a")]);
    let mut fs = mount_rw(&img);
    img.reset_writes();
    let cancelled = hzip_progress::CancellationToken::new();
    cancelled.cancel();
    let (_tmp, src) = local_file("n.txt", b"n");
    let err = fs
        .apply(
            &Change::Append {
                src,
                name: "b.txt".into(),
            },
            None,
            &cancelled,
        )
        .unwrap_err();
    assert!(matches!(err, VolError::Cancelled));
    assert_eq!(img.writes(), 0, "取消必须在动卷之前生效");
}

/// 未知 id：`read_at`/`size_of`/`apply` 都给稳定键，不 panic。
#[test]
fn unknown_ids_are_refused_by_key() {
    let img = image(&[Node::File("a.txt", b"a")]);
    let mut fs = mount_rw(&img);
    let bogus = 999;
    let mut buf = [0u8; 4];
    assert!(matches!(
        fs.read_at(bogus, 0, &mut buf),
        Err(VolError::Unsupported("vol.err.unknown-entry"))
    ));
    assert!(matches!(
        fs.size_of(bogus),
        Err(VolError::Unsupported("vol.err.unknown-entry"))
    ));
    assert!(matches!(
        fs.apply(&Change::Delete(bogus), None, &cancel()),
        Err(VolError::Unsupported("vol.err.unknown-entry"))
    ));
}

/// 源不是常规文件 / 末段名超长 / 拿目录当常规文件写：写前按名拒绝。
#[test]
fn bad_sources_and_names_are_refused_before_writing() {
    let img = image(&[Node::File("a.txt", b"a")]);
    let mut fs = mount_rw(&img);
    let (tmp, src) = local_file("n.txt", b"n");

    // 源是目录。
    let err = fs
        .apply(
            &Change::Append {
                src: tmp.path().to_path_buf(),
                name: "dir-source.txt".into(),
            },
            None,
            &cancel(),
        )
        .unwrap_err();
    assert!(
        matches!(err, VolError::Unsupported("vol.err.source-not-file")),
        "{err:?}"
    );

    // 末段名超过 255 字节。
    let long = "x".repeat(300);
    let err = fs
        .apply(
            &Change::Append {
                src: src.clone(),
                name: long.into(),
            },
            None,
            &cancel(),
        )
        .unwrap_err();
    assert!(
        matches!(err, VolError::Unsupported("vol.err.name-too-long")),
        "{err:?}"
    );

    // 把常规文件当目录用 ⇒ 类型不符。
    let err = fs
        .apply(
            &Change::Append {
                src: src.clone(),
                name: "a.txt/inner".into(),
            },
            None,
            &cancel(),
        )
        .unwrap_err();
    assert!(
        matches!(err, VolError::Unsupported("vol.err.bad-type")),
        "拿常规文件当目录用必须按名拒绝：{err:?}"
    );
}

/// 非 UTF-8 名字的条目：**按 id** 仍能替换/删除（写侧按字节路径定位）。
#[test]
fn changes_by_id_work_for_non_utf8_names() {
    let img = format_empty();
    let dev: std::sync::Arc<dyn hzip_vol::BlockDevice> = std::sync::Arc::new(img.clone());
    {
        let xfs = fs_xfs::Filesystem::mount_rw(dev).unwrap();
        let root = xfs.superblock().rootino;
        let (ino, _) = xfs.create_file(root, b"raw-\xff.bin", 0o644).unwrap();
        xfs.write(ino, 0, b"old").unwrap();
        xfs.unmount().unwrap();
    }
    let mut fs = mount_rw(&img);
    let id = tree(&*fs)
        .entries
        .iter()
        .find(|e| e.name.as_str().contains('\u{FFFD}'))
        .map(|e| e.id)
        .expect("有损名应能列出");

    let (_tmp, src) = local_file("new.bin", b"fresh bytes");
    fs.apply(&Change::Replace { entry: id, src }, None, &cancel())
        .unwrap();
    assert_eq!(read_all(&*fs, id), b"fresh bytes");
    // 卷里仍然只有**一个**条目（没有被「有损名」另建出一个副本）。
    assert_eq!(tree(&*fs).entries.len(), 1, "不许按有损名另建副本");

    fs.apply(&Change::Delete(id), None, &cancel()).unwrap();
    fs.flush().unwrap();
    drop(fs);
    let ro = remount(&img);
    assert!(tree(&*ro).entries.is_empty());
    assert!(ro.audit().unwrap().is_clean());
}
