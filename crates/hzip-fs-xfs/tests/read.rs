//! XFS 读侧契约测试：头部复核、全树列出、惰性读、上限与取消、符号链接、自检。
//!
//! 对应任务书 §完成标准 1 的「`supports` 魔数口径、全树列出、惰性读、脏日志/坏校验和
//! 不 panic、上限与取消」中的读侧部分（拒写与坏盘在 `tests/refuse.rs`）。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use common::*;
use hzip_format::NameEncoding;
use hzip_fs_xfs::XfsDriver;
use hzip_progress::CancellationToken;
use hzip_vol::{EntryLimit, FsDriver, FsKind, VolError};

/// 夹具里那份「有目录、有中文名、有空文件、有符号链接、有深目录」的树。
fn sample() -> SparseDevice {
    image(&[
        Node::File("readme.txt", b"hello xfs\n"),
        Node::File("empty.bin", b""),
        Node::File("docs/guide.md", b"# guide\n"),
        Node::File("docs/子目录/中文名.txt", "中文内容\n".as_bytes()),
        Node::File("deep/a/b/c/deep.bin", &vec![0xABu8; 4096 * 3]),
        Node::Dir("just-a-dir"),
        Node::Symlink("link-to-readme", "readme.txt"),
        Node::Symlink("docs/link-up", "../readme.txt"),
    ])
}

/// `supports` 的魔数口径：`XFSB`（**大端**、偏移 0）+ 五条几何证据都要对。
#[test]
fn supports_requires_magic_and_sane_geometry() {
    let img = format_empty();
    let driver = XfsDriver::new();
    let head = img.head();

    assert!(driver.supports(FsKind::Xfs, &head), "真实夹具头部必须点头");
    // 嗅探结论不对 ⇒ 不接（注册表的契约是「两份证据都点头」）。
    for kind in [FsKind::Ext4, FsKind::Ntfs, FsKind::Fat, FsKind::ExFat] {
        assert!(!driver.supports(kind, &head), "{kind:?} 不是我们的卷");
    }

    // 全零（没格式化）/ 太短 / 魔数差点意思：都不接。
    assert!(!driver.supports(FsKind::Xfs, &[0u8; 4096]));
    assert!(!driver.supports(FsKind::Xfs, b"XFSB"));
    let mut wrong = head.clone();
    wrong[0] = b'Y';
    assert!(!driver.supports(FsKind::Xfs, &wrong), "魔数错一位就不接");

    // 几何自相矛盾：块大小不是 2 的幂 / 版本号不是 4 或 5 / 零个分配组。
    let mut bad_bs = head.clone();
    bad_bs[4..8].copy_from_slice(&3000u32.to_be_bytes());
    assert!(!driver.supports(FsKind::Xfs, &bad_bs));

    let mut bad_ver = head.clone();
    bad_ver[100..102].copy_from_slice(&0x0003u16.to_be_bytes());
    assert!(!driver.supports(FsKind::Xfs, &bad_ver));

    let mut bad_ag = head;
    bad_ag[88..92].copy_from_slice(&0u32.to_be_bytes());
    assert!(!driver.supports(FsKind::Xfs, &bad_ag));
}

/// `is_xfs_superblock` 与 `supports` 同一口径（它就是 `supports` 的复核体）。
#[test]
fn supports_is_the_same_check_as_the_public_head_probe() {
    let head = format_empty().head();
    assert!(hzip_fs_xfs::is_xfs_superblock(&head));
    assert!(XfsDriver::new().supports(FsKind::Xfs, &head));
}

/// 全树列出：名字用 `/` 分隔；目录**也**列出；目录大小按仓库口径为 0。
#[test]
fn lists_the_whole_tree_with_slash_separated_names() {
    let fs = mount_ro(&sample());
    let t = tree(&*fs);
    assert!(!t.truncated);
    assert_eq!(t.skipped, 0, "夹具里没有该跳过的条目");

    let mut names: Vec<&str> = t.entries.iter().map(|e| e.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "deep",
            "deep/a",
            "deep/a/b",
            "deep/a/b/c",
            "deep/a/b/c/deep.bin",
            "docs",
            "docs/guide.md",
            "docs/link-up",
            "docs/子目录",
            "docs/子目录/中文名.txt",
            "empty.bin",
            "just-a-dir",
            "link-to-readme",
            "readme.txt",
        ],
        "根不列在树里；每条用 `/` 分隔且无前导 `./`"
    );

    let readme = t.by_name("readme.txt").unwrap();
    assert!(!readme.is_dir && !readme.is_symlink);
    assert_eq!(readme.size_raw, 10);
    assert_eq!(readme.mode, Some(0o100644), "权限位 + 文件类型位原样给出");
    assert_eq!(readme.name_encoding, NameEncoding::Utf8);
    assert!(readme.mtime.is_some(), "XFS 有 mtime");

    let empty = t.by_name("empty.bin").unwrap();
    assert_eq!(empty.size_raw, 0);
    assert_eq!(empty.size_packed, 0, "0 字节文件不占块");

    let dir = t.by_name("just-a-dir").unwrap();
    assert!(dir.is_dir);
    assert_eq!(dir.size_raw, 0, "目录没有可提取的字节（tar/zip 同口径）");

    let deep = t.by_name("deep/a/b/c/deep.bin").unwrap();
    assert_eq!(deep.size_raw, 4096 * 3);
    assert_eq!(
        deep.size_packed,
        4096 * 3,
        "盘上占用 = di_nblocks（文件系统块）× 块大小（12288 B 的文件占 3 块）"
    );
}

/// 符号链接：给 target（**原样存储**，可能是相对路径），大小 = 目标长度。
#[test]
fn symlinks_expose_their_target() {
    let fs = mount_ro(&sample());
    let t = tree(&*fs);

    let link = t.by_name("link-to-readme").unwrap();
    assert!(link.is_symlink);
    assert_eq!(link.symlink_target.as_deref().unwrap(), "readme.txt");
    assert_eq!(link.size_raw, "readme.txt".len() as u64);

    let up = t.by_name("docs/link-up").unwrap();
    assert!(up.is_symlink);
    assert_eq!(
        up.symlink_target.as_deref().unwrap(),
        "../readme.txt",
        "相对目标原样给，不解析"
    );
}

/// 惰性读：乱序、重复、跨块、EOF 之外，全都要对（契约红线 2）。
#[test]
fn read_at_is_lazy_random_and_repeatable() {
    let fs = mount_ro(&sample());
    let big_id = id_of(&*fs, "deep/a/b/c/deep.bin");
    let readme_id = id_of(&*fs, "readme.txt");
    assert_eq!(fs.size_of(big_id).unwrap(), 4096 * 3);
    assert_eq!(fs.size_of(readme_id).unwrap(), 10);

    // 乱序 + 重复：先读尾、再读头、再读中、再读一遍尾。
    let mut tail = [0u8; 16];
    assert_eq!(fs.read_at(big_id, 4096 * 3 - 16, &mut tail).unwrap(), 16);
    assert!(tail.iter().all(|b| *b == 0xAB));

    let mut head = [0u8; 8];
    assert_eq!(fs.read_at(big_id, 0, &mut head).unwrap(), 8);
    assert!(head.iter().all(|b| *b == 0xAB));

    let mut mid = [0u8; 4096];
    assert_eq!(fs.read_at(big_id, 4096, &mut mid).unwrap(), 4096);
    assert!(mid.iter().all(|b| *b == 0xAB));

    let mut tail2 = [0u8; 16];
    assert_eq!(fs.read_at(big_id, 4096 * 3 - 16, &mut tail2).unwrap(), 16);
    assert_eq!(tail, tail2, "同一区间两次读必须一致");

    // 短读只发生在文件尾（EOF 之后返回 0，不报错、不夹取）。
    let mut over = [0u8; 64];
    assert_eq!(fs.read_at(readme_id, 8, &mut over).unwrap(), 2);
    assert_eq!(&over[..2], b"s\n");
    assert_eq!(fs.read_at(readme_id, 10, &mut over).unwrap(), 0);
    assert_eq!(fs.read_at(readme_id, 1_000_000, &mut over).unwrap(), 0);
    // 空缓冲：不碰设备，直接 0。
    assert_eq!(fs.read_at(readme_id, 0, &mut []).unwrap(), 0);
    // 目录没有可提取的字节。
    assert_eq!(
        fs.read_at(id_of(&*fs, "docs"), 0, &mut over).unwrap(),
        0,
        "目录读返回 0"
    );
}

/// `EntryLimit::max_entries` ⇒ `truncated`；没到上限 ⇒ 不截断。
#[test]
fn entry_limit_truncates_instead_of_blowing_up() {
    let fs = mount_ro(&sample());
    let all = tree(&*fs).entries.len();

    let tight = EntryLimit {
        max_entries: 3,
        max_depth: 32,
    };
    let t = fs.entries(&tight, &cancel()).unwrap();
    assert_eq!(t.entries.len(), 3);
    assert!(t.truncated, "到上限必须如实标注截断");

    let exact = EntryLimit {
        max_entries: all,
        max_depth: 32,
    };
    let t = fs.entries(&exact, &cancel()).unwrap();
    assert_eq!(t.entries.len(), all);
    assert!(!t.truncated, "正好装下不算截断");
}

/// `EntryLimit::max_depth` ⇒ 到顶不再深入，并标注截断。
#[test]
fn depth_limit_stops_descending() {
    let fs = mount_ro(&sample());
    let shallow = EntryLimit {
        max_entries: 10_000,
        max_depth: 1,
    };
    let t = fs.entries(&shallow, &cancel()).unwrap();
    assert!(t.truncated);
    for e in &t.entries {
        assert!(
            !e.name.as_str().contains('/'),
            "深度 1 只该列出顶层：{}",
            e.name
        );
    }
    // 顶层条目一个不少（截断只发生在「再往下」）。
    assert_eq!(t.entries.len(), 6);
}

/// 取消：进 `entries()` 之前就取消 ⇒ `Cancelled`；且**不动**上一次成功的条目表。
#[test]
fn cancelling_entries_reports_cancelled_and_keeps_the_old_table() {
    let fs = mount_ro(&sample());
    let id = id_of(&*fs, "readme.txt");
    assert_eq!(read_name(&*fs, "readme.txt"), b"hello xfs\n");

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        fs.entries(&limit(), &cancelled),
        Err(VolError::Cancelled)
    ));
    // 旧表仍然可用（最后一次成功的列表没被半列出来的结果换掉）。
    assert_eq!(read_all(&*fs, id), b"hello xfs\n".to_vec());
    assert_eq!(fs.size_of(id).unwrap(), 10);
}

/// 名字里有控制字符的条目**不列出**（计数进 `skipped`）：它表示不成 `EntryName`，
/// 列出来只会让上层拿着一个它无法回指的名字。
#[test]
fn control_character_names_are_counted_as_skipped() {
    // 夹具造不出控制字符名（`Node` 走的是 `&str`），故直接用上游建一个。
    let img = format_empty();
    let dev: std::sync::Arc<dyn hzip_vol::BlockDevice> = std::sync::Arc::new(img.clone());
    {
        let xfs = fs_xfs::Filesystem::mount_rw(dev).unwrap();
        let root = xfs.superblock().rootino;
        let (ino, _) = xfs.create_file(root, b"bad\x01name", 0o644).unwrap();
        xfs.write(ino, 0, "藏在坏名字后面".as_bytes()).unwrap();
        xfs.unmount().unwrap();
    }
    let fs = mount_ro(&img);
    let t = tree(&*fs);
    assert_eq!(t.skipped, 1, "表示不成 EntryName 的名字只计数、不列出");
    assert!(t.entries.is_empty(), "坏名字不进树");
}

/// 非 UTF-8 名字：有损解码后**列出**，并如实标 `NameEncoding::Unknown`。
#[test]
fn non_utf8_names_are_listed_lossily_and_labelled() {
    let img = format_empty();
    let dev: std::sync::Arc<dyn hzip_vol::BlockDevice> = std::sync::Arc::new(img.clone());
    {
        let xfs = fs_xfs::Filesystem::mount_rw(dev).unwrap();
        let root = xfs.superblock().rootino;
        let (ino, _) = xfs.create_file(root, b"raw-\xff\xfe.bin", 0o644).unwrap();
        xfs.write(ino, 0, b"bytes").unwrap();
        xfs.unmount().unwrap();
    }
    let fs = mount_ro(&img);
    let t = tree(&*fs);
    let e = t
        .entries
        .iter()
        .find(|e| e.name.as_str().contains('\u{FFFD}'))
        .expect("有损解码后应能列出（U+FFFD 不是控制字符）");
    assert_eq!(e.name_encoding, NameEncoding::Unknown);
    assert_eq!(e.size_raw, 5);
    assert_eq!(read_all(&*fs, e.id), b"bytes", "按 id 读得到原始字节");
}

/// `audit()`：干净卷干净；`skipped` 为假（确实检查过了，不是「没实现」）。
#[test]
fn audit_of_a_clean_image_is_clean_and_not_skipped() {
    let fs = mount_ro(&sample());
    let report = fs.audit().unwrap();
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    assert!(!report.skipped, "上游 check 是真自检，不许标成「未实现」");
    assert!(report.is_clean());
}

/// 读侧**不改档案**：只读挂载上列树、乱序读、自检，设备一个字节都不许写。
#[test]
fn read_path_never_writes_the_device() {
    let img = sample();
    let before = img.snapshot();
    img.reset_writes();
    {
        let fs = mount_ro(&img);
        let t = tree(&*fs);
        for e in &t.entries {
            let _ = read_all(&*fs, e.id);
        }
        fs.audit().unwrap();
    }
    assert_eq!(img.writes(), 0, "只读路径不许碰设备");
    assert_eq!(img.bytes_written(), 0);
    assert!(
        img.differing_pages(&before).is_empty(),
        "只读路径不允许改动任何字节"
    );
}

/// 夹具的结构确定性：两次构建的差异**恰好是这 4 页**——inode chunk 的首个 inode 页
/// 与内部日志的头 3 页。
///
/// # 差异从哪来（以及为什么只能是这 4 页）
///
/// 上游 `mkfs` 与写路径都用**系统时钟**给 inode 打时间戳（mkfs 给 3 个初始 inode、
/// `create_*`/`write` 给新 inode），且 0.14.1 **没有时钟注入点**（ext4 夹具的 `Runtime`
/// 注入是 ext4 上游给的）。于是：
///
/// | 页 | 是什么 | 为什么不一样 |
/// |---|---|---|
/// | 16 | inode chunk 的第一页（inode 128..143：root 目录、两个 realtime inode、夹具建的文件） | 这些 inode 的 atime/mtime/ctime/crtime 来自时钟 |
/// | 40966..40968 | 内部日志的头几页 | 夹具写的日志记录里带着 inode 镜像（时间戳）与其 CRC32C |
///
/// 其余一律逐字节相同（超级块扇区、各 AG 头、btree 根、文件内容、目录块）。
/// 页号由夹具的几何算出来（320 MiB / 4 KiB / 4 个 AG）；几何一改这条就会炸，
/// 逼着人重新看一眼而不是让它悄悄漂。
#[test]
fn fixture_is_structurally_deterministic() {
    // inode 大小 512 ⇒ 每页 8 个 inode；`rootino` 是 128 ⇒ chunk 首页 = 128 / 8 = 16。
    const INODE_CHUNK_PAGE: u64 = 128 / (4096 / 512);
    // 内部日志在「中间那个 AG」的第 6 个文件系统块（mkfs 的布局：块 0 是超级块副本 +
    // AGF/AGI/AGFL，块 1..=5 是五棵 btree 根，块 6 起是日志）。
    const LOG_PAGE: u64 = (IMAGE_BYTES / 2 + 6 * BLOCK_SIZE as u64) / 4096;

    let nodes = [Node::File("a.txt", b"aaa"), Node::File("dir/b.txt", b"bbb")];
    let first = image(&nodes);
    let second = image(&nodes);
    let diff = first.differing_pages(&second);

    assert_eq!(
        diff,
        [INODE_CHUNK_PAGE, LOG_PAGE, LOG_PAGE + 1, LOG_PAGE + 2],
        "除了 inode 时间戳（及其在日志记录里的镜像/校验和），镜像必须逐字节相同"
    );

    // 换个说法把「结构相同」也钉一下：超级块扇区逐字节相同、树相同。
    assert_eq!(first.read_bytes(0, 512), second.read_bytes(0, 512));
    let fs = mount_ro(&first);
    let other = mount_ro(&second);
    assert_eq!(tree(&*fs).entries.len(), tree(&*other).entries.len());
    assert_eq!(read_name(&*fs, "a.txt"), read_name(&*other, "a.txt"));
}
