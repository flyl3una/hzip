//! ext4 只读侧契约测试（T-IMG3 §2.3 映射表 + T-IMG5 的读侧不变式）。
//!
//! 覆盖：全树列出（含中文名/深层目录/空文件）、乱序 + 重复读、符号链接目标、
//! 特殊文件不列出（计数进 `skipped`）、`EntryLimit` 截断、取消语义、`audit` 干净、
//! 头部复核（只认 `s_magic` @1080）。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

use std::path::Path;
use std::sync::Arc;

use fs_ext4::fs_core_bridge::CoreDevice;
use fs_ext4::inode::{S_IFCHR, S_IFIFO};
use fs_ext4::{Filesystem, mkfs};
use hzip_format::Change;
use hzip_format::Source;
use hzip_format_testkit::vol::ext4::{EXT4_BLOCK_SIZE, EXT4_IMAGE_BYTES, VOLUME_UUID};
use hzip_format_testkit::vol::{MemDevice, ext4_image};
use hzip_fs_ext4::Ext4Driver;
use hzip_progress::CancellationToken;
use hzip_vol::device::{Chain, WriteMode};
use hzip_vol::error::{ReadOnlyReason, VolError};
use hzip_vol::{EntryLimit, EntryTree, FsDriver, FsKind, FsOptions, FsVolume, VolOptions};

/// 只读挂载（`FsOptions::default()` = 不请求可写）。
fn mount_ro(dev: Arc<MemDevice>) -> Box<dyn FsVolume> {
    Ext4Driver::new().mount(dev, &FsOptions::default()).unwrap()
}

/// 宽裕的列表上限（本文件里只有专门的截断用例用小值）。
fn limit() -> EntryLimit {
    EntryLimit {
        max_entries: 10_000,
        max_depth: 32,
    }
}

fn cancel() -> CancellationToken {
    CancellationToken::new()
}

fn entries(fs: &dyn FsVolume) -> EntryTree {
    fs.entries(&limit(), &cancel()).unwrap()
}

/// 全量读出某条目（乱序/重复读的对照用）。
fn read_all(fs: &dyn FsVolume, id: hzip_format::EntryId) -> Vec<u8> {
    let size = fs.size_of(id).unwrap();
    let mut out = vec![0u8; usize::try_from(size).unwrap()];
    let n = fs.read_at(id, 0, &mut out).unwrap();
    out.truncate(n);
    out
}

/// 夹具造不出的条目（符号链接/字符设备/管道）：只能用上游 API 现场造。
fn special_image() -> Vec<u8> {
    let mem = MemDevice::new(EXT4_IMAGE_BYTES);
    {
        let dev = CoreDevice::new(mem.clone());
        mkfs::format_filesystem(
            &dev,
            Some("HZTEST"),
            Some(VOLUME_UUID),
            EXT4_IMAGE_BYTES,
            EXT4_BLOCK_SIZE,
        )
        .unwrap();
        let fs = Filesystem::mount(Arc::new(dev)).unwrap();
        fs.apply_mkdir("/etc", 0o755).unwrap();
        fs.apply_create("/etc/fstab", 0o644).unwrap();
        fs.apply_replace_file_content("/etc/fstab", b"LABEL=root / ext4 defaults 0 1\n")
            .unwrap();
        fs.apply_mkdir("/dev", 0o755).unwrap();
        fs.apply_mknod("/dev/null", S_IFCHR | 0o666, 1, 3).unwrap();
        fs.apply_mknod("/pipe", S_IFIFO | 0o644, 0, 0).unwrap();
        fs.apply_symlink("/etc/fstab", "/link").unwrap();
        fs.finish().unwrap();
    }
    mem.to_bytes()
}

#[test]
fn lists_and_reads_the_whole_tree() {
    let img = ext4_image(&[
        ("/etc/fstab", b"LABEL=root / ext4 defaults 0 1\n"),
        ("/boot/BOOTX64.EFI", b"EFI-BYTES"),
        ("/empty.txt", b""),
        ("/文档/说明.txt", "中文内容\n".as_bytes()),
    ]);
    let dev = Arc::new(MemDevice::from_bytes(img));
    let fs = mount_ro(dev);
    let tree = entries(&*fs);

    let names: Vec<&str> = tree.entries.iter().map(|e| e.name.as_str()).collect();
    for want in [
        "etc",
        "etc/fstab",
        "boot",
        "boot/BOOTX64.EFI",
        "empty.txt",
        "文档",
        "文档/说明.txt",
    ] {
        assert!(names.contains(&want), "缺条目 {want}（实得 {names:?}）");
    }
    assert!(
        names
            .iter()
            .all(|n| !n.starts_with("./") && !n.ends_with('/')),
        "名字必须已规范化（无前导 ./、无尾 /）：{names:?}"
    );
    assert!(
        names.iter().all(|n| !n.contains('\\')),
        "名字不含反斜杠：{names:?}"
    );
    assert!(!tree.truncated, "远未到上限，不该截断");
    assert_eq!(tree.skipped, 0, "这个镜像没有可跳过的条目");

    // 目录也作为条目（并且 is_dir）
    let etc = tree.by_name("etc").unwrap();
    assert!(etc.is_dir && !etc.is_symlink);
    assert_eq!(etc.size_raw, 0, "目录没有可提取的字节（仓库约定）");
    assert_eq!(etc.mode.map(|m| m & 0o170000), Some(0o040000), "目录类型位");

    // 只读能力与理由（未请求可写 ⇒ BySetting）
    assert!(!fs.caps().write);
    assert_eq!(fs.caps().read_only_reason, Some(ReadOnlyReason::BySetting));
    assert_eq!(fs.caps().name_max, 255);
    assert_eq!(fs.fs_id(), "ext4");

    // 内容：整条 + 乱序 + 重复
    let fstab = tree.by_name("etc/fstab").unwrap().id;
    assert_eq!(fs.size_of(fstab).unwrap(), 31);
    assert_eq!(
        read_all(&*fs, fstab),
        b"LABEL=root / ext4 defaults 0 1\n".to_vec()
    );
    let mut buf = [0u8; 5];
    assert_eq!(fs.read_at(fstab, 6, &mut buf).unwrap(), 5);
    assert_eq!(&buf, b"root "); // 乱序：先读中段
    let mut head = [0u8; 10];
    assert_eq!(fs.read_at(fstab, 0, &mut head).unwrap(), 10);
    assert_eq!(&head, b"LABEL=root");
    let mut again = [0u8; 5];
    assert_eq!(fs.read_at(fstab, 6, &mut again).unwrap(), 5);
    assert_eq!(&again, b"root "); // 重复：同一偏移第二次结果一致

    // 中文名内容
    let cn = tree.by_name("文档/说明.txt").unwrap().id;
    assert_eq!(read_all(&*fs, cn), "中文内容\n".as_bytes().to_vec());
    assert_eq!(
        tree.by_name("文档/说明.txt").unwrap().name_encoding,
        hzip_format::NameEncoding::Utf8
    );

    // 空文件：大小 0，读 0 字节而不是报错
    let empty = tree.by_name("empty.txt").unwrap().id;
    assert_eq!(fs.size_of(empty).unwrap(), 0);
    let mut buf = [0u8; 16];
    assert_eq!(fs.read_at(empty, 0, &mut buf).unwrap(), 0);

    // `entries()` 重复调用：树的形状与 id 映射一致（读侧无游标状态）
    let again = entries(&*fs);
    assert_eq!(
        again.by_name("etc/fstab").unwrap().id,
        tree.by_name("etc/fstab").unwrap().id
    );
    assert_eq!(again.entries.len(), tree.entries.len());
}

#[test]
fn special_files_are_not_listed_and_symlinks_carry_targets() {
    let dev = Arc::new(MemDevice::from_bytes(special_image()));
    let fs = mount_ro(dev);
    let tree = entries(&*fs);

    assert!(
        tree.by_name("dev/null").is_none(),
        "字符设备不列出（避免误导：它的内容不是可提取的字节）"
    );
    assert!(tree.by_name("pipe").is_none(), "管道不列出");
    assert!(tree.by_name("dev").is_some(), "目录本身要列出");
    assert_eq!(tree.skipped, 2, "被跳过的特殊文件如实计数");

    let link = tree.by_name("link").unwrap();
    assert!(link.is_symlink && !link.is_dir);
    assert_eq!(
        link.symlink_target.as_deref(),
        Some(Path::new("/etc/fstab"))
    );
    assert_eq!(
        fs.size_of(link.id).unwrap(),
        10,
        "符号链接大小 = 目标路径长度"
    );

    // 符号链接不是常规文件：按字节读如实报错（调用方应走 `symlink_target`）
    assert!(
        fs.read_at(link.id, 0, &mut [0u8; 4]).is_err(),
        "不能把符号链接当文件读"
    );
    // 链接的目标确实在树里可读
    assert_eq!(
        read_all(&*fs, tree.by_name("etc/fstab").unwrap().id),
        b"LABEL=root / ext4 defaults 0 1\n".to_vec()
    );
}

#[test]
fn audit_reports_clean_image() {
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[
        ("/etc/fstab", b"x"),
        ("/a/b/c.txt", b"y"),
    ])));
    let fs = mount_ro(dev);
    let audit = fs.audit().unwrap();
    assert!(audit.is_clean(), "刚造的镜像必须自检干净：{audit:?}");
    assert!(audit.findings.is_empty() && !audit.skipped);

    // 特殊文件（含符号链接、设备）同样不制造伪异常
    let dev = Arc::new(MemDevice::from_bytes(special_image()));
    let fs = mount_ro(dev);
    assert!(fs.audit().unwrap().is_clean());
}

#[test]
fn supports_only_accepts_the_real_magic() {
    let driver = Ext4Driver::new();
    assert_eq!(driver.id(), "ext4");

    let mut head = [0u8; 1082];
    head[1080..1082].copy_from_slice(&[0x53, 0xEF]);
    assert!(driver.supports(FsKind::Ext4, &head), "s_magic @1080 应认");

    // ⚠ `53 EF` @1024 是常见误记（1024 处是 s_inodes_count）
    let mut wrong = [0u8; 1082];
    wrong[1024..1026].copy_from_slice(&[0x53, 0xEF]);
    assert!(
        !driver.supports(FsKind::Ext4, &wrong),
        "魔数认错位置就不得认"
    );

    assert!(!driver.supports(FsKind::Fat, &head), "嗅探说是 FAT 就不接");
    assert!(
        !driver.supports(FsKind::Ext4, &[0x53, 0xEF]),
        "头太短不得越界"
    );
    assert!(!driver.supports(FsKind::Ext4, &[]));
}

#[test]
fn cancel_stops_the_listing() {
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/a/b.txt", b"x")])));
    let fs = mount_ro(dev);
    let token = CancellationToken::new();
    token.cancel();
    let err = fs.entries(&limit(), &token).unwrap_err();
    assert!(matches!(err, VolError::Cancelled), "实得 {err:?}");
}

#[test]
fn entry_limit_truncates_by_count_and_by_depth() {
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[
        ("/a.txt", b"a"),
        ("/b.txt", b"b"),
        ("/c/d.txt", b"c"),
        ("/c/e/f.txt", b"e"),
    ])));

    // 条数上限
    let fs = mount_ro(dev.clone());
    let tree = fs
        .entries(
            &EntryLimit {
                max_entries: 2,
                max_depth: 32,
            },
            &cancel(),
        )
        .unwrap();
    assert_eq!(tree.entries.len(), 2);
    assert!(tree.truncated, "到条数上限必须标截断");

    // 深度上限：只列顶层（根的孩子），不再进目录
    let fs = mount_ro(dev.clone());
    let tree = fs
        .entries(
            &EntryLimit {
                max_entries: 10_000,
                max_depth: 1,
            },
            &cancel(),
        )
        .unwrap();
    let names: Vec<&str> = tree.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"a.txt") && names.contains(&"c"));
    assert!(!names.contains(&"c/d.txt"), "深度到顶就不再深入");
    assert!(tree.truncated, "深度截断必须标注");

    // 深度 0：什么都不列
    let tree = fs
        .entries(
            &EntryLimit {
                max_entries: 10_000,
                max_depth: 0,
            },
            &cancel(),
        )
        .unwrap();
    assert!(tree.entries.is_empty() && tree.truncated);
}

#[test]
fn read_only_device_is_refused_for_writing_with_device_reason() {
    // 生产路径里的只读形态：设备链按「只读」模式打开（`ReadOnlyDevice`，
    // `is_writable() == false`）。
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("disk.img");
    std::fs::write(&img, ext4_image(&[("/a.txt", b"x")])).unwrap();
    let chain = Chain::open(
        Source::File(img),
        WriteMode::ReadOnly,
        &VolOptions::default(),
        None,
    )
    .unwrap();

    let mut fs = Ext4Driver::new()
        .mount(
            chain.device(),
            &FsOptions {
                writable: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!fs.caps().write, "设备写不了就不能声称可写");
    assert_eq!(
        fs.caps().read_only_reason,
        Some(ReadOnlyReason::DeviceReadOnly)
    );
    // 读照常（只读设备上上游把重放落进缓存，不改设备）
    assert_eq!(
        read_all(&*fs, entries(&*fs).by_name("a.txt").unwrap().id),
        b"x".to_vec()
    );
    let err = fs
        .apply(
            &Change::MakeDir {
                name: "nope".into(),
            },
            None,
            &cancel(),
        )
        .unwrap_err();
    assert!(
        matches!(err, VolError::ReadOnly(ReadOnlyReason::DeviceReadOnly)),
        "实得 {err:?}"
    );
}

#[test]
fn unknown_entry_id_is_an_error_not_a_panic() {
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/a.txt", b"a")])));
    let fs = mount_ro(dev);
    // 还没列树就按 id 读 ⇒ 如实报错
    assert!(matches!(fs.size_of(7).unwrap_err(), VolError::Image(_)));
    let tree = entries(&*fs);
    let mut names: Vec<String> = tree
        .entries
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    names.sort();
    // 夹具卷的顶层 = mkfs 造的 `lost+found` + 我们放的那一个文件
    assert_eq!(names, vec!["a.txt".to_string(), "lost+found".to_string()]);
    assert!(matches!(
        fs.read_at(tree.entries[0].id + 5, 0, &mut [0u8; 4])
            .unwrap_err(),
        VolError::Image(_)
    ));
    assert!(tree.by_name("不存在").is_none());
}
