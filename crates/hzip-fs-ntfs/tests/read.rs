//! 读侧用例（计划 `T-IMG11` 步骤 1 的 `tests/read.rs`）。
//!
//! 夹具是上游 `mkfs` + 上游写路径现场造的小卷（`hzip_format_testkit::vol::ntfs`），
//! 设备是内存盘；每个用例都走**真**驱动（物化 → 挂载 → 列/读）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use hzip_format::Change;
use hzip_vol::VolError;

#[test]
fn lists_and_reads_a_fresh_volume() {
    let image = ntfs_image(&[
        ("top.txt", b"top level"),
        ("dir/inner.txt", b"nested content"),
        ("dir/empty.txt", b""),
    ]);
    let fs = mount(device(image), &writable()).expect("挂载");

    let tree = entries(&fs);
    assert!(!tree.truncated);
    assert_eq!(
        names(&tree),
        vec!["dir", "dir/empty.txt", "dir/inner.txt", "top.txt"],
        "树形与名字"
    );
    // id 必须是 0 起连续的（契约：EntryId 0 起连续）。
    for (index, entry) in tree.entries.iter().enumerate() {
        assert_eq!(entry.id, index, "id 必须按列表顺序 0 起连续");
    }
    assert!(tree.by_name("dir").unwrap().is_dir, "目录条目 is_dir");
    assert!(!tree.by_name("top.txt").unwrap().is_dir);

    let id = id_of(&fs, "dir/inner.txt");
    assert_eq!(fs.size_of(id).unwrap(), "nested content".len() as u64);
    assert_eq!(
        read_all(&fs, id, fs.size_of(id).unwrap()),
        b"nested content"
    );
    assert_eq!(fs.size_of(id_of(&fs, "dir/empty.txt")).unwrap(), 0);

    // 惰性读：乱序、重复、贴尾、越界（对应 `ArchiveReader` 红线 2）。
    let mut buf = vec![0u8; 7];
    let n = fs.read_at(id, 7, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"content");
    let head = {
        let mut b = vec![0u8; 6];
        let n = fs.read_at(id, 0, &mut b).unwrap();
        assert_eq!(&b[..n], b"nested");
        b
    };
    assert_eq!(&head[..6], b"nested", "重复/乱序读必须一致");
    let n = fs.read_at(id, 14, &mut buf).unwrap();
    assert_eq!(n, 0, "贴尾/越界读返回 0，不报错");
    assert_eq!(fs.read_at(id, 0, &mut []).unwrap(), 0, "空缓冲返回 0");

    // 目录条目不该被当成文件读。
    let dir_id = id_of(&fs, "dir");
    assert!(fs.read_at(dir_id, 0, &mut buf).is_err(), "读目录必须报错");
}

#[test]
fn unicode_names_round_trip() {
    let image = ntfs_image(&[
        ("中文目录/名字.txt", "内容：中文".as_bytes()),
        ("表情🙂.bin", b"emoji"),
        ("日本語/ファイル.bin", b"jp"),
    ]);
    let fs = mount(device(image), &writable()).expect("挂载");
    let tree = entries(&fs);

    // UTF-16 → UTF-8 不丢条目，且逐字符一致。
    for want in [
        "中文目录",
        "中文目录/名字.txt",
        "表情🙂.bin",
        "日本語",
        "日本語/ファイル.bin",
    ] {
        assert!(
            tree.by_name(want).is_some(),
            "条目 {want} 应当在树里；实际 {:?}",
            names(&tree)
        );
    }
    assert_eq!(fs.lossy_name_count(), 0, "合法名字不该出现替换字符");

    let id = id_of(&fs, "中文目录/名字.txt");
    assert_eq!(
        read_all(&fs, id, fs.size_of(id).unwrap()),
        "内容：中文".as_bytes()
    );
    let entry = entry_of(&fs, "表情🙂.bin");
    assert_eq!(entry.name.as_str(), "表情🙂.bin", "名字逐字符一致");
}

#[test]
fn resident_and_nonresident_files() {
    // 常驻：几百字节（塞得进 1 KiB 的 MFT 记录）。
    let small = b"resident payload".to_vec();
    // 非驻留：200 KiB，跨 50 个 4 KiB 簇。
    let big: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
    let image = ntfs_image(&[("small.txt", &small), ("big.bin", &big)]);

    let fs = mount(device(image), &writable()).expect("挂载");
    let small_id = id_of(&fs, "small.txt");
    let big_id = id_of(&fs, "big.bin");
    assert_eq!(fs.size_of(small_id).unwrap(), small.len() as u64);
    assert_eq!(fs.size_of(big_id).unwrap(), big.len() as u64);
    assert_eq!(read_all(&fs, small_id, small.len() as u64), small);
    assert_eq!(read_all(&fs, big_id, big.len() as u64), big);

    // 跨簇窗口读：起点不在块边界上，长度跨块。
    let mut buf = vec![0u8; 7000];
    let n = fs.read_at(big_id, 4090, &mut buf).unwrap();
    assert_eq!(n, 7000);
    assert_eq!(&buf[..n], &big[4090..4090 + 7000]);
}

#[test]
fn deep_directory_tree() {
    let deep = "l1/l2/l3/l4/l5/l6/l7/l8/deep.txt";
    let image = ntfs_image(&[(deep, b"deep content"), ("l1/side.txt", b"side")]);
    let fs = mount(device(image), &writable()).expect("挂载");

    let tree = entries(&fs);
    for level in 1..=8 {
        let dir = (1..=level)
            .map(|n| format!("l{n}"))
            .collect::<Vec<_>>()
            .join("/");
        assert!(
            tree.by_name(&dir).is_some(),
            "第 {level} 层目录 {dir} 应当在树里"
        );
    }
    assert!(tree.by_name(deep).is_some());
    let id = id_of(&fs, deep);
    assert_eq!(read_all(&fs, id, 12), b"deep content");

    // 深度上限：只列到第 3 层。
    let shallow = entries_with(
        &fs,
        &EntryLimit {
            max_depth: 3,
            ..EntryLimit::default()
        },
    );
    assert!(shallow.by_name("l1/l2/l3").is_some());
    assert!(shallow.by_name("l1/l2/l3/l4").is_none(), "深度上限必须生效");
    assert!(shallow.by_name(deep).is_none());

    // 条目上限：截断如实标注。
    let cut = entries_with(
        &fs,
        &EntryLimit {
            max_entries: 3,
            ..EntryLimit::default()
        },
    );
    assert_eq!(cut.entries.len(), 3);
    assert!(cut.truncated, "被条目上限截断必须标注");
    assert_eq!(cut.skipped, 0, "NTFS 没有要跳过的条目类型");
}

#[test]
fn entries_can_be_cancelled() {
    let image = ntfs_image(&[("a.txt", b"a"), ("dir/b.txt", b"b")]);
    let fs = mount(device(image), &writable()).expect("挂载");
    let err = fs
        .entries(&EntryLimit::default(), &cancelled())
        .expect_err("已取消的 token 必须中断列目录");
    assert!(matches!(err, VolError::Cancelled), "得到 {err:?}");
}

#[test]
fn change_to_a_deleted_entry_is_reported_not_guessed() {
    // 条目 id 只在最近一次 entries() 内有效：越界 id 要报错而不是乱猜路径。
    let image = ntfs_image(&[("a.txt", b"a")]);
    let mut fs = mount(device(image), &writable()).expect("挂载");
    let err = fs
        .apply(&Change::Delete(99), None, &cancel())
        .expect_err("越界 id 必须报错");
    assert!(
        format!("{err}").contains("99"),
        "错误里要带上越界 id：{err}"
    );
}
