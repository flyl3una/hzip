//! FAT 写侧：五类变更往返 + **重挂复核**、跨目录改名、ESP 形态、4 GiB 上限。
//!
//! 对应 T-IMG12 计划 §步骤 1 的 `tests/write.rs`。

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use hzip_format::{Change, EntryName};
use hzip_vol::VolError;

/// 名字构造（`Input` 是 `&str`）。
fn name(s: &str) -> EntryName {
    EntryName::new(s)
}

/// 五类变更（建/删/改名/建目录/写内容）往返，且**重挂**后仍成立。
///
/// 重挂用的是设备上真实的字节（`MemDevice::to_bytes`）⇒ 验的是「真的落盘了」，
/// 而不是「内存里那个对象还记得」。
#[test]
fn add_replace_delete_rename_mkdir_round_trip_and_survive_remount() {
    let image = fat32(&[
        ("keep.txt", b"keep"),
        ("old.txt", b"old"),
        ("dir/x.txt", b"x"),
    ]);
    let mut m = mount_image(image);
    let cancel = cancel();

    // ① MakeDir（逐级：newdir/sub）
    m.fs.apply(
        &Change::MakeDir {
            name: name("newdir/sub"),
        },
        None,
        &cancel,
    )
    .expect("MakeDir 应成功");

    // ② Append（CJK 名 + 刚建的目录 + 内容）
    let appended = local_file("fat-append.bin", b"appended");
    m.fs.apply(
        &Change::Append {
            src: appended.clone(),
            name: name("newdir/sub/新文件.txt"),
        },
        Some(&appended),
        &cancel,
    )
    .expect("Append 应成功");

    // ③ Replace（就地改内容）
    let id_keep = id_of(m.fs.as_ref(), "keep.txt");
    let replaced = local_file("fat-replace.bin", b"replaced-contents");
    m.fs.apply(
        &Change::Replace {
            entry: id_keep,
            src: replaced.clone(),
        },
        Some(&replaced),
        &cancel,
    )
    .expect("Replace 应成功");

    // ④ Rename（`to` 不含 `/` ⇒ `from` 的父目录下的新名字，core 契约）
    m.fs.apply(
        &Change::Rename {
            from: name("dir/x.txt"),
            to: name("y.txt"),
        },
        None,
        &cancel,
    )
    .expect("Rename 应成功");

    // ⑤ Delete
    let id_old = id_of(m.fs.as_ref(), "old.txt");
    m.fs.apply(&Change::Delete(id_old), None, &cancel)
        .expect("Delete 应成功");

    m.fs.flush().expect("flush 应成功");
    let bytes = m.dev.to_bytes();
    drop(m);

    // ---- 重挂复核 ----
    let r = mount_image(bytes);
    let list = tree(r.fs.as_ref());
    assert!(
        list.by_name("newdir/sub").is_some(),
        "新目录应在重挂后仍在：{:?}",
        names(&list)
    );
    assert!(list.by_name("newdir/sub/新文件.txt").is_some());
    assert_eq!(
        read_name(r.fs.as_ref(), "newdir/sub/新文件.txt"),
        b"appended"
    );
    assert_eq!(read_name(r.fs.as_ref(), "keep.txt"), b"replaced-contents");
    assert_eq!(read_name(r.fs.as_ref(), "dir/y.txt"), b"x");
    assert!(list.by_name("dir/x.txt").is_none(), "改名后旧名不该在");
    assert!(list.by_name("old.txt").is_none(), "删掉的条目不该在");

    // ---- FAT 无日志 ⇒ 额外断言：写后目录项与 FAT 链自洽（轻量自检）----
    for e in &list.entries {
        if e.is_dir {
            continue;
        }
        let data = read_all(r.fs.as_ref(), e.id);
        assert_eq!(
            data.len() as u64,
            e.size_raw,
            "目录项 size 应与实际可读字节一致：{}",
            e.name
        );
    }
    let audit = r.fs.audit().expect("audit 应可跑");
    assert!(audit.skipped, "FAT 无 fsck：audit 必须如实标注 skipped");
    assert_eq!(
        audit.findings,
        vec!["vol.warn.no-journal".to_string()],
        "自己刚写的盘，轻量自检不该报出问题"
    );
}

/// Append 自行逐级建父目录（调用方不必先 MakeDir）。
#[test]
fn append_creates_missing_parent_dirs() {
    let mut m = mount_image(fat32(&[]));
    let src = local_file("fat-deep.bin", b"deep-content");
    m.fs.apply(
        &Change::Append {
            src: src.clone(),
            name: name("EFI/BOOT/BOOTX64.EFI"),
        },
        Some(&src),
        &cancel(),
    )
    .expect("Append 应逐级建目录");
    m.fs.flush().unwrap();

    let r = mount_image(m.dev.to_bytes());
    let list = tree(r.fs.as_ref());
    assert!(list.by_name("EFI").is_some());
    assert!(list.by_name("EFI/BOOT").is_some());
    assert_eq!(
        read_name(r.fs.as_ref(), "EFI/BOOT/BOOTX64.EFI"),
        b"deep-content"
    );
}

/// `name` 以 `/` 结尾的 Append ＝ 只建目录（`Change` 契约的隐式目录语义，与 ext4 侧同口径）。
#[test]
fn append_with_trailing_slash_only_makes_a_directory() {
    let mut m = mount_image(fat32(&[]));
    let src = local_file("fat-slash.bin", b"ignored");
    m.fs.apply(
        &Change::Append {
            src: src.clone(),
            name: name("implicit/dir/"),
        },
        Some(&src),
        &cancel(),
    )
    .expect("建目录应成功");
    m.fs.flush().unwrap();

    let r = mount_image(m.dev.to_bytes());
    let list = tree(r.fs.as_ref());
    let dir = list
        .by_name("implicit/dir")
        .unwrap_or_else(|| panic!("应建出目录；实际 = {:?}", names(&list)));
    assert!(dir.is_dir);
    assert!(
        list.entries
            .iter()
            .all(|e| !e.name.as_str().starts_with("implicit/dir/")),
        "只建目录时不该写出任何文件：{:?}",
        names(&list)
    );
}

/// 同名 Append ＝ 覆盖（zip `-a` 的更新语义），不是「追加到文件尾」。
#[test]
fn append_overwrites_an_existing_name() {
    let mut m = mount_image(fat32(&[("f.txt", b"0123456789")]));
    let src = local_file("fat-short.bin", b"ab");
    m.fs.apply(
        &Change::Append {
            src: src.clone(),
            name: name("f.txt"),
        },
        Some(&src),
        &cancel(),
    )
    .expect("覆盖应成功");
    assert_eq!(
        read_name(m.fs.as_ref(), "f.txt"),
        b"ab",
        "旧的尾字节必须消失"
    );
}

/// 跨目录改名：`to` 带 `/` ⇒ 卷内完整路径（FAT 侧「拷目录项 + 链搬迁」）。
#[test]
fn rename_across_directories() {
    let mut m = mount_image(fat32(&[("a/x.txt", b"cross"), ("b/keep.txt", b"k")]));
    m.fs.apply(
        &Change::Rename {
            from: name("a/x.txt"),
            to: name("b/y.txt"),
        },
        None,
        &cancel(),
    )
    .expect("跨目录改名应成功");
    m.fs.flush().unwrap();

    let r = mount_image(m.dev.to_bytes());
    let list = tree(r.fs.as_ref());
    assert!(list.by_name("b/y.txt").is_some(), "{:?}", names(&list));
    assert!(list.by_name("a/x.txt").is_none());
    assert!(list.by_name("a").is_some(), "源目录本身不该被删掉");
    assert_eq!(read_name(r.fs.as_ref(), "b/y.txt"), b"cross");
}

/// 目录改名连带整棵子树的路径（id 表与 FAT 目录项两边都要跟上）。
#[test]
fn renaming_a_directory_moves_its_whole_subtree() {
    let mut m = mount_image(fat32(&[
        ("old/inner/deep.txt", b"deep"),
        ("old/side.txt", b"side"),
        ("other.txt", b"other"),
    ]));
    let deep_id = id_of(m.fs.as_ref(), "old/inner/deep.txt");
    m.fs.apply(
        &Change::Rename {
            from: name("old"),
            to: name("new"),
        },
        None,
        &cancel(),
    )
    .expect("目录改名应成功");
    // id 稳定：改名前拿到的 id，改名后仍指向同一个文件。
    assert_eq!(read_all(m.fs.as_ref(), deep_id), b"deep");
    m.fs.flush().unwrap();

    let r = mount_image(m.dev.to_bytes());
    let list = tree(r.fs.as_ref());
    assert!(
        list.by_name("new/inner/deep.txt").is_some(),
        "{:?}",
        names(&list)
    );
    assert!(list.by_name("new/side.txt").is_some());
    assert!(list.by_name("old").is_none());
    assert_eq!(read_name(r.fs.as_ref(), "new/inner/deep.txt"), b"deep");
}

/// 非空目录不删（**不递归**）：`vol.err.dir-not-empty`；空了以后可删。
#[test]
fn deleting_a_non_empty_directory_is_refused() {
    let mut m = mount_image(fat32(&[("d/f.txt", b"f")]));
    let id_dir = id_of(m.fs.as_ref(), "d");
    match m.fs.apply(&Change::Delete(id_dir), None, &cancel()) {
        Err(VolError::Unsupported(key)) => {
            assert_eq!(key, hzip_fs_fat::ERR_DIR_NOT_EMPTY, "应是目录非空这个键");
        }
        other => panic!("非空目录删除应被拒绝，得到 {other:?}"),
    }
    assert!(
        tree(m.fs.as_ref()).by_name("d/f.txt").is_some(),
        "被拒绝的删除不能动到内容"
    );

    // 先删文件，再删目录 ⇒ 成立。
    let id_file = id_of(m.fs.as_ref(), "d/f.txt");
    m.fs.apply(&Change::Delete(id_file), None, &cancel())
        .unwrap();
    let id_dir = id_of(m.fs.as_ref(), "d");
    m.fs.apply(&Change::Delete(id_dir), None, &cancel())
        .unwrap();
    m.fs.flush().unwrap();

    let r = mount_image(m.dev.to_bytes());
    assert!(tree(r.fs.as_ref()).by_name("d").is_none());
}

/// ESP 形态（最常见的真实用法）：`/EFI/BOOT/BOOTX64.EFI` 一族**增删改**都成立。
#[test]
fn works_on_an_efi_system_partition_shape() {
    let image = fat32(&[
        ("EFI/BOOT/BOOTX64.EFI", b"original-bootx64"),
        ("EFI/BOOT/BOOTIA32.EFI", b"original-bootia32"),
        ("EFI/Microsoft/Boot/bootmgfw.efi", b"windows-boot"),
    ]);
    let mut m = mount_image(image);
    let list = tree(m.fs.as_ref());
    for want in [
        "EFI",
        "EFI/BOOT",
        "EFI/BOOT/BOOTX64.EFI",
        "EFI/BOOT/BOOTIA32.EFI",
        "EFI/Microsoft",
        "EFI/Microsoft/Boot",
        "EFI/Microsoft/Boot/bootmgfw.efi",
    ] {
        assert!(
            list.by_name(want).is_some(),
            "ESP 形态缺条目 {want}：{:?}",
            names(&list)
        );
    }

    let cancel = cancel();
    // 改：替换引导器内容
    let id_x64 = id_of(m.fs.as_ref(), "EFI/BOOT/BOOTX64.EFI");
    let new_boot = local_file("fat-bootx64.bin", b"patched-bootx64-image");
    m.fs.apply(
        &Change::Replace {
            entry: id_x64,
            src: new_boot.clone(),
        },
        Some(&new_boot),
        &cancel,
    )
    .unwrap();
    // 删：清掉 32 位引导器
    let id_ia32 = id_of(m.fs.as_ref(), "EFI/BOOT/BOOTIA32.EFI");
    m.fs.apply(&Change::Delete(id_ia32), None, &cancel).unwrap();
    // 增：新增一个配置文件
    let added = local_file("fat-grub.cfg", b"set timeout=0\n");
    m.fs.apply(
        &Change::Append {
            src: added.clone(),
            name: name("EFI/BOOT/grub.cfg"),
        },
        Some(&added),
        &cancel,
    )
    .unwrap();
    // 改名：换个引导器名（同目录，bare name）
    m.fs.apply(
        &Change::Rename {
            from: name("EFI/BOOT/BOOTX64.EFI"),
            to: name("BOOTX64.BAK"),
        },
        None,
        &cancel,
    )
    .unwrap();
    m.fs.flush().unwrap();

    let r = mount_image(m.dev.to_bytes());
    let list = tree(r.fs.as_ref());
    assert_eq!(
        read_name(r.fs.as_ref(), "EFI/BOOT/BOOTX64.BAK"),
        b"patched-bootx64-image"
    );
    assert!(list.by_name("EFI/BOOT/BOOTX64.EFI").is_none());
    assert!(list.by_name("EFI/BOOT/BOOTIA32.EFI").is_none());
    assert_eq!(
        read_name(r.fs.as_ref(), "EFI/BOOT/grub.cfg"),
        b"set timeout=0\n"
    );
    assert_eq!(
        read_name(r.fs.as_ref(), "EFI/Microsoft/Boot/bootmgfw.efi"),
        b"windows-boot",
        "没碰过的条目必须原样保留"
    );
}

/// FAT32 的单文件 4 GiB 上限：**写前**就拒绝（`vol.err.too-large`），不截断、不半写。
#[test]
fn fat32_four_gib_limit_is_reported_not_truncated() {
    // ① 门本身：边界精确（4 GiB - 1 放行，+1 拒绝）
    assert!(hzip_fs_fat::check_file_size(hzip_fs_fat::FAT_MAX_FILE_BYTES).is_ok());
    match hzip_fs_fat::check_file_size(hzip_fs_fat::FAT_MAX_FILE_BYTES + 1) {
        Err(VolError::Unsupported(key)) => assert_eq!(key, hzip_fs_fat::ERR_TOO_LARGE),
        other => panic!("超上限应报 vol.err.too-large，得到 {other:?}"),
    }

    // ② 端到端：源文件是 4 GiB + 1（只读它的长度 —— 门在**打开/读取内容之前**）
    let big = over_limit_source();
    let big_path = big.path().to_path_buf();
    let mut m = mount_image(fat32(&[("boot.efi", b"ORIGINAL")]));
    let cancel = cancel();

    let id = id_of(m.fs.as_ref(), "boot.efi");
    match m.fs.apply(
        &Change::Replace {
            entry: id,
            src: big_path.clone(),
        },
        Some(big.path()),
        &cancel,
    ) {
        Err(VolError::Unsupported(key)) => assert_eq!(key, hzip_fs_fat::ERR_TOO_LARGE),
        other => panic!("Replace 超上限应报 vol.err.too-large，得到 {other:?}"),
    }
    match m.fs.apply(
        &Change::Append {
            src: big_path.clone(),
            name: name("big.bin"),
        },
        Some(big.path()),
        &cancel,
    ) {
        Err(VolError::Unsupported(key)) => assert_eq!(key, hzip_fs_fat::ERR_TOO_LARGE),
        other => panic!("Append 超上限应报 vol.err.too-large，得到 {other:?}"),
    }

    // ③ **不截断、不半写**：目标文件一个字节没动，被拒的 Append 也没留下条目
    assert_eq!(read_name(m.fs.as_ref(), "boot.efi"), b"ORIGINAL");
    m.fs.flush().unwrap();
    let r = mount_image(m.dev.to_bytes());
    assert_eq!(read_name(r.fs.as_ref(), "boot.efi"), b"ORIGINAL");
    assert!(
        tree(r.fs.as_ref()).by_name("big.bin").is_none(),
        "被拒绝的 Append 不该留下空条目"
    );
}
