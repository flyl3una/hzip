//! FAT 读侧：LFN/8.3/CJK、多层目录与空文件、惰性读、时间戳、三种 FAT 宽度。
//!
//! 对应 T-IMG12 计划 §步骤 1 的 `tests/read.rs`。

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use time::OffsetDateTime;

/// LFN 与 8.3 **并存**：长名按长名列出，短名按短名列出，两者都在树里。
#[test]
fn long_and_short_names_both_visible() {
    let image = fat32(&[("BOOTX64.EFI", b"efi"), ("Some Long Name.txt", b"long")]);
    let mounted = mount_image(image);
    let list = tree(mounted.fs.as_ref());

    assert!(list.by_name("BOOTX64.EFI").is_some(), "8.3 名应可见");
    assert!(
        list.by_name("Some Long Name.txt").is_some(),
        "LFN 名应可见（而不是 8.3 的缩略名）；实际 = {:?}",
        names(&list)
    );
    assert!(
        !list.truncated && list.skipped == 0,
        "小树不该截断或跳过：{list:?}"
    );
    assert_eq!(read_name(mounted.fs.as_ref(), "BOOTX64.EFI"), b"efi");
    assert_eq!(
        read_name(mounted.fs.as_ref(), "Some Long Name.txt"),
        b"long"
    );
}

/// **只有短名**的条目（真实世界的老工具/嵌入式会写这种盘）：LFN 项被删掉后，
/// 长名列表仍要能按短名列出它。
#[test]
fn short_name_only_entry_is_listed() {
    let mut image = fat32(&[("BOOTX64.EFI", b"efi-bytes")]);
    strip_lfn(&mut image, b"BOOTX64 EFI");
    let mounted = mount_image(image);

    let list = tree(mounted.fs.as_ref());
    let found = list
        .by_name("BOOTX64.EFI")
        .expect("删掉 LFN 项后仍应按短名列出来");
    assert_eq!(found.size_raw, 9);
    assert_eq!(read_name(mounted.fs.as_ref(), "BOOTX64.EFI"), b"efi-bytes");
}

/// CJK 名经 LFN（UTF-16 → UTF-8）往返：不丢条目、字节不丢。
#[test]
fn cjk_names_round_trip() {
    let image = fat32(&[
        ("文档/说明.txt", "中文内容".as_bytes()),
        ("日本語のファイル.txt", "こんにちは".as_bytes()),
        ("한국어.txt", "안녕하세요".as_bytes()),
    ]);
    let mounted = mount_image(image);
    let list = tree(mounted.fs.as_ref());

    assert!(
        list.by_name("文档").is_some(),
        "目录名应可读：{:?}",
        names(&list)
    );
    assert!(list.by_name("文档/说明.txt").is_some());
    assert!(list.by_name("日本語のファイル.txt").is_some());
    assert!(list.by_name("한국어.txt").is_some());
    assert_eq!(
        read_name(mounted.fs.as_ref(), "文档/说明.txt"),
        "中文内容".as_bytes()
    );
    assert_eq!(
        read_name(mounted.fs.as_ref(), "日本語のファイル.txt"),
        "こんにちは".as_bytes()
    );
    assert_eq!(
        read_name(mounted.fs.as_ref(), "한국어.txt"),
        "안녕하세요".as_bytes()
    );
}

/// 多层目录 + 空文件 + 目录条目本身也在树里（`is_dir` 为真、`size_raw == 0`）。
#[test]
fn nested_dirs_and_empty_files() {
    let image = fat32(&[
        ("empty.txt", b""),
        ("a/b/c/deep.bin", b"deep"),
        ("a/b/sibling.txt", b"sib"),
    ]);
    let mounted = mount_image(image);
    let list = tree(mounted.fs.as_ref());

    let dir_a = entry(&list, "a");
    assert!(dir_a.is_dir, "a 应是目录");
    assert_eq!(dir_a.size_raw, 0, "目录条目没有大小");
    assert!(entry(&list, "a/b").is_dir);
    assert!(entry(&list, "a/b/c").is_dir);

    let deep = entry(&list, "a/b/c/deep.bin");
    assert!(!deep.is_dir);
    assert_eq!(deep.size_raw, 4);
    assert_eq!(read_name(mounted.fs.as_ref(), "a/b/c/deep.bin"), b"deep");
    assert_eq!(read_name(mounted.fs.as_ref(), "a/b/sibling.txt"), b"sib");

    // 空文件：size 0 ⇒ size_of 为 0，read_at 立刻 EOF。
    let empty = entry(&list, "empty.txt");
    assert_eq!(empty.size_raw, 0);
    assert_eq!(mounted.fs.size_of(empty.id).unwrap(), 0);
    let mut buf = [0u8; 16];
    assert_eq!(mounted.fs.read_at(empty.id, 0, &mut buf).unwrap(), 0);

    // 目录条目本身没有内容可读（投影层只对文件调用，但接口要稳）。
    assert_eq!(mounted.fs.read_at(dir_a.id, 0, &mut buf).unwrap(), 0);
}

/// 修改时间如实上报。FAT 的时间字段精度是 **2 s**（DOS 时间只存秒/2），
/// 且夹具用固定时钟 ⇒ 允许 ±2 s 的偏差（断言写成区间而不是等值）。
#[test]
fn modification_time_is_reported() {
    let image = fat32(&[("time.txt", b"t")]);
    let mounted = mount_image(image);
    let id = id_of(mounted.fs.as_ref(), "time.txt");

    let info = entry(&tree(mounted.fs.as_ref()), "time.txt").clone();
    let mtime = info.mtime.expect("FAT 有 mtime，应非 None");
    let want = OffsetDateTime::from_unix_timestamp(FIXED_UNIX_SECONDS).unwrap();
    let delta = (mtime.unix_timestamp() - want.unix_timestamp()).abs();
    assert!(
        delta <= 2,
        "mtime 应接近夹具固定时刻（FAT 精度 2 s）：得到 {mtime}，期望 {want}"
    );
    // `size_of` 与树里的 size 一致（同一份目录项）。
    assert_eq!(mounted.fs.size_of(id).unwrap(), info.size_raw);
}

/// 三种 FAT 宽度（12/16/32）都能列、都能读——12 位表项是最易错的一档。
#[test]
fn all_three_fat_widths_list_and_read() {
    let cases: [(&str, Vec<u8>); 3] = [
        (
            "FAT12",
            fat12(&[("A.TXT", b"fat12"), ("dir/B.TXT", b"nested12")]),
        ),
        (
            "FAT16",
            fat16(&[("A.TXT", b"fat16"), ("dir/B.TXT", b"nested16")]),
        ),
        (
            "FAT32",
            fat32(&[("A.TXT", b"fat32"), ("dir/B.TXT", b"nested32")]),
        ),
    ];
    for (width, image) in cases {
        let mounted = mount_image(image);
        let list = tree(mounted.fs.as_ref());
        assert!(
            list.by_name("A.TXT").is_some() && list.by_name("dir/B.TXT").is_some(),
            "{width} 应列出两个条目；实际 = {:?}",
            names(&list)
        );
        let want = width.to_ascii_lowercase();
        assert_eq!(
            read_name(mounted.fs.as_ref(), "A.TXT"),
            want.as_bytes(),
            "{width} 根目录文件内容"
        );
        assert_eq!(
            read_name(mounted.fs.as_ref(), "dir/B.TXT"),
            format!("nested{}", &want[3..]).as_bytes(),
            "{width} 子目录文件内容"
        );
    }
}

/// 惰性读：任意偏移、乱序、重复都可用（契约红线 2）。
#[test]
fn lazy_reads_are_order_independent() {
    let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let image = fat32(&[("blob.bin", &data)]);
    let mounted = mount_image(image);
    let id = id_of(mounted.fs.as_ref(), "blob.bin");

    // 乱序 + 重复 + 跨界 + 越过 EOF（Clamp 到 EOF ⇒ 0 字节，不是错误）
    let mut buf = [0u8; 100];
    let n = mounted.fs.read_at(id, 4000, &mut buf).unwrap();
    assert_eq!(n, 96, "文件尾只能给到 96 字节（短读是合法结局）");
    assert_eq!(&buf[..n], &data[4000..]);
    assert_eq!(mounted.fs.read_at(id, 4096, &mut buf).unwrap(), 0);
    assert_eq!(mounted.fs.read_at(id, 1 << 40, &mut buf).unwrap(), 0);
    let n = mounted.fs.read_at(id, 100, &mut buf).unwrap();
    assert_eq!(n, 100);
    let n2 = mounted.fs.read_at(id, 100, &mut buf).unwrap();
    assert_eq!(n2, 100);
    assert_eq!(&buf[..], &data[100..200]);

    // 短读：只给 3 字节的缓冲 ⇒ 恰好 3 字节，不是整块。
    let mut small = [0u8; 3];
    assert_eq!(mounted.fs.read_at(id, 7, &mut small).unwrap(), 3);
    assert_eq!(small, [data[7], data[8], data[9]]);
}

/// 8.3 短名与 LFN 混在同一个目录里，两种都不丢（真盘上最常见的形态）。
#[test]
fn mixed_83_and_lfn_in_one_directory() {
    let image = fat32(&[
        ("README.TXT", b"short"),
        ("A Long File Name With Spaces.bin", b"long"),
        ("SUB", b"dirish"),
    ]);
    let mounted = mount_image(image);
    let list = tree(mounted.fs.as_ref());
    assert_eq!(
        list.entries.len(),
        3,
        "不应多出/少出条目：{:?}",
        names(&list)
    );
    assert_eq!(read_name(mounted.fs.as_ref(), "README.TXT"), b"short");
    assert_eq!(
        read_name(mounted.fs.as_ref(), "A Long File Name With Spaces.bin"),
        b"long"
    );

    // 大小与内容无关地对得上：`size_of` 走目录项，`read_all` 走数据。
    let id = id_of(mounted.fs.as_ref(), "SUB");
    assert_eq!(read_all(mounted.fs.as_ref(), id), b"dirish");
}
