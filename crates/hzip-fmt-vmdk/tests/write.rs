//! VMDK 写侧：`monolithicSparse` 的就地写 / 覆盖写（暂存→提交）往返。
//!
//! # 四条路径，同一套断言口径
//!
//! | 路径 | 走到哪一层 | 关键断言 |
//! |---|---|---|
//! | fs 层 `apply(Replace)` + flush + commit | 卷 → 分区 → **ext4** → grain → 落盘 | 原档变了、新内容读得回、冗余 GD 与主 GD 同步、`uncleanShutdown` 归零、content id 更新 |
//! | `VolumeEditor::write_in_place`（`Replace`） | 契约入口（core 的「编辑保存」用的就是它） | 提交后原档**变了**、重开读回新内容、**其他条目逐字节原样** |
//! | `VolumeEditor::write_in_place`（`Delete`） | 同上 | 条目从树里消失、其他条目原样 |
//! | `VolumeEditor::rewrite`（`Replace` + `Delete` → `Sink::File`） | 契约入口（覆盖写） | **提交前原档 sha256 不变**、产物含改动、其他条目原样 |
//!
//! 另有一条直接驱动设备链的用例，把「覆盖写的暂存语义」钉在**设备层**：写进去的
//! 字节在 `commit` 之前不得落到原档。
//!
//! # 已知限制（不在本 crate 内，供后续接手者）
//!
//! **改动若需要「新分配一个 grain」，只有 `InPlace` 模式能成功。** 上游
//! `VmdkReader` 分配 grain 的方式是把文件**加长**（`BlockDevice::set_len`），
//! 而 `hzip_vol` 的 `OverlayDevice` 按设计**拒绝扩容**（`can_grow() == false`，
//! `set_len` 报 `vol.unsupported.resize`）。实测（vmdk + 真实空洞镜像）：
//!
//! ```text
//! Overlay  write → Err(Io(Custom("vol.unsupported.resize：卷容量不可变")))
//! InPlace  write → Ok（FileDevice::open_rw 可加长）
//! ```
//!
//! 失败是**干净**的（覆盖层丢弃、原档未坏），但默认写模式正是 Overlay ⇒ 编辑一个
//! 「改动落在空洞里」的 VMDK 会报 resize 键。本文件的用例把全部 grain 预分配，
//! 所以走的是**写穿**路径（不需要扩容）。这是 hzip-vol 覆盖层 × 上游分配策略的接缝，
//! 不在本 crate 的可改范围内，已如实上报。
//!
//! # `EntryId` 从哪来
//!
//! `hzip-fs-ext4` 的 `EntryId` 是**一次 `entries()` 建立的表的下标**，故
//! `VolumeEditor::mount_writable` 挂载后必须**先建索引**（`hzip-vol` 已这么做，
//! 缺这一步 `Replace`/`Delete` 全废）。测试取 id 的手法与此一致：挂载 → `entries()`
//! → 取 id（下标是遍历位置的函数，同一棵树两次挂载得到同一组 id）。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hzip_fmt_vmdk::VmdkHandler;
use hzip_fmt_vmdk::volume::{VmdkVolume, fs_options_for};
use hzip_format::{
    Change, ChangeSet, CreateOptions, FormatHandler as _, OpenOptions, PasswordRef, Sink, Source,
    VolumeWriteMode, WriteCtx,
};
use hzip_format_testkit::vol::{FixtureFs, raw_with_mbr};
use hzip_progress::CancellationToken;
use hzip_vol::device::{Chain, WriteMode};
use hzip_vol::{EntryLimit, VolOptions, Volume as _};

use common::{content_id, registry, sha256_file, vmdk_sparse, write_temp};

const NEW_A: &[u8] = b"NEW-CONTENT"; // 比 "old" 长 ⇒ ext4 要动元数据与数据块

// ------------------------------------------------------------ 上下文

/// `WriteCtx` 的持有组件（`WriteCtx` 本身借它们）。
struct Ctx {
    cancel: CancellationToken,
    bus: hzip_format::ProgressBus,
    opts: CreateOptions,
}

impl Ctx {
    fn new(mode: VolumeWriteMode) -> Self {
        Self {
            cancel: CancellationToken::new(),
            bus: hzip_format::ProgressBus::default(),
            opts: CreateOptions {
                volume_write: mode,
                ..CreateOptions::default()
            },
        }
    }

    fn wctx(&self) -> WriteCtx<'_> {
        WriteCtx {
            cancel: &self.cancel,
            progress: &self.bus,
            opts: &self.opts,
        }
    }
}

fn vol_opts(writable: bool) -> VolOptions {
    VolOptions {
        writable,
        fs: fs_options_for(writable),
        fs_registry: registry(),
        ..VolOptions::default()
    }
}

// ------------------------------------------------------------ 工具

/// 夹具：`a.txt = "old"`、`b.txt = "keep-b"`、`c.txt = "keep-c"` 的 sparse 镜像。
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let img = vmdk_sparse(&raw_with_mbr(
        FixtureFs::Ext4,
        &[
            ("/a.txt", b"old"),
            ("/b.txt", b"keep-b"),
            ("/c.txt", b"keep-c"),
        ],
    ));
    let (dir, path) = write_temp("disk.vmdk", &img);
    let src = dir.path().join("new-a.txt");
    std::fs::write(&src, NEW_A).unwrap();
    (dir, path, src)
}

/// **档案级**条目 id：读侧投影会把「卷行」排在最前（`Partition 1`），其后才是各卷内
/// 条目 ⇒ `档案 id = 卷内 id + 1`。
///
/// 编辑器（`VolumeEditor`）收到的是 core 传下来的**档案级**变更，路由时再减 1 变回
/// 卷内 id（2026-10-16）。直接调 `fs.apply` 的用例（不走编辑器）仍用卷内 id
/// —— 两侧混用会让断言悄悄错位，故这里显式区分两个助手。
fn archive_id(path: &Path, name: &str) -> usize {
    local_entries(path)[name] + 1
}

/// 只读挂载一次并列出条目名（**保序**：下标即 `EntryId`）。
fn local_entries(path: &Path) -> HashMap<String, usize> {
    let opts = vol_opts(false);
    let chain = Chain::open(
        Source::File(path.to_path_buf()),
        WriteMode::ReadOnly,
        &opts,
        None,
    )
    .unwrap();
    let vol = VmdkVolume::new(chain.device(), opts).unwrap();
    let fs = vol.open_fs(Some(0)).unwrap().expect("分区 0 应挂上 ext4");
    let tree = fs
        .entries(&EntryLimit::default(), &CancellationToken::new())
        .unwrap();
    tree.entries
        .iter()
        .map(|e| (e.name.as_str().to_string(), e.id))
        .collect()
}

/// 通过 handler 取条目 id（投影名 `Partition 1/<name>` 或后缀匹配）。
fn read_file(path: &Path, name: &str) -> Vec<u8> {
    let handler = VmdkHandler::new(registry());
    let reader = handler
        .open_reader(Source::File(path.to_path_buf()), &OpenOptions::default())
        .unwrap();
    let want = format!("Partition 1/{name}");
    let entries = reader.entries().unwrap();
    let id = entries
        .iter()
        .position(|e| e.name.as_str() == want)
        .unwrap_or_else(|| panic!("{want} 不在投影里：{:?}", entry_names(&entries)));
    let mut buf = Vec::new();
    std::io::Read::read_to_end(
        &mut reader.read_entry(id, PasswordRef::none()).unwrap(),
        &mut buf,
    )
    .unwrap();
    buf
}

/// 投影里的条目名（判「删没删掉」）。
fn projected_names(path: &Path) -> Vec<String> {
    let handler = VmdkHandler::new(registry());
    let reader = handler
        .open_reader(Source::File(path.to_path_buf()), &OpenOptions::default())
        .unwrap();
    entry_names(&reader.entries().unwrap())
}

fn entry_names(entries: &[hzip_format::EntryInfo]) -> Vec<String> {
    entries
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect()
}

fn le_u32(img: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(img[off..off + 4].try_into().unwrap())
}

fn le_u64(img: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(img[off..off + 8].try_into().unwrap())
}

/// 稀疏元数据（头 + GD + RGD）逐字节未动。
///
/// 夹具把**全部 grain 预分配**，所以改动落在既有 grain 内（写穿）⇒ 目录/表都不该被碰，
/// 头里的 `uncleanShutdown` 也在正常 flush 后回到 0 ⇒ 头逐字节不变。
fn assert_sparse_metadata_untouched(before: &[u8], after: &[u8]) {
    assert_eq!(&before[..512], &after[..512], "稀疏头必须逐字节不变");
    let capacity = le_u64(before, 12);
    let gt_count = capacity.div_ceil(128).div_ceil(512) as usize;
    let gd_len = gt_count * 4;
    for (label, off) in [
        ("主 GD", le_u64(before, 56) as usize),
        ("冗余 GD", le_u64(before, 48) as usize),
    ] {
        assert_eq!(
            &before[off * 512..off * 512 + gd_len],
            &after[off * 512..off * 512 + gd_len],
            "{label} 不该被这次改动碰（grain 已预分配）"
        );
    }
}

/// 冗余 grain 目录与主目录**同步**。
///
/// 「同步」不是「两份目录的字节相同」——那两处本来就指向**不同的**表位置
/// （主目录 → 主表，冗余目录 → 冗余表）。同步的含义是：**同一个槽位要么都为空、
/// 要么都非空，且两张表的内容逐字节一致**（描述同一份 grain 映射）。上游负责维护
/// 同步，这里只断言我们自己写出来的镜像确实同步。
fn assert_redundant_gd_matches_primary(img: &[u8]) {
    assert_eq!(le_u32(img, 0) & 0x2, 0x2, "镜像应带冗余 GT 标志");
    let capacity = le_u64(img, 12);
    let rgd_off = le_u64(img, 48) as usize;
    let gd_off = le_u64(img, 56) as usize;
    assert_ne!(rgd_off, 0, "带标志就必须有 rgdOffset");
    let gt_count = capacity.div_ceil(128).div_ceil(512) as usize;
    let gd_len = gt_count * 4;
    let gd = &img[gd_off * 512..gd_off * 512 + gd_len];
    let rgd = &img[rgd_off * 512..rgd_off * 512 + gd_len];
    for (t, (p, r)) in gd.chunks_exact(4).zip(rgd.chunks_exact(4)).enumerate() {
        let ps = u32::from_le_bytes(p.try_into().unwrap()) as usize;
        let rs = u32::from_le_bytes(r.try_into().unwrap()) as usize;
        assert_eq!(
            ps == 0,
            rs == 0,
            "第 {t} 张表：主/冗余目录的「有/无」必须一致（{ps} vs {rs}）"
        );
        if ps == 0 {
            continue;
        }
        assert_eq!(
            &img[ps * 512..ps * 512 + 2048],
            &img[rs * 512..rs * 512 + 2048],
            "第 {t} 张 grain 表的主/冗余副本必须描述同一份映射"
        );
    }
}

// ------------------------------------------------- 一、fs 层 Replace（完整断言）

#[test]
fn write_allocates_and_keeps_the_redundant_gd_in_step() {
    let (_dir, path, src) = fixture();
    let before = std::fs::read(&path).unwrap();
    let cid_before = content_id(&before);

    // 就地链（直接写原文件）——与 `VolumeEditor` 的就地模式同一条链
    let opts = vol_opts(true);
    let chain = Chain::open(Source::File(path.clone()), WriteMode::InPlace, &opts, None).unwrap();
    assert_eq!(chain.mode(), WriteMode::InPlace);
    let vol = VmdkVolume::new(chain.device(), opts).unwrap();
    assert!(vol.caps().writable, "就地链上的 sparse 应可写");
    let mut fs = vol.open_fs(Some(0)).unwrap().expect("分区 0 应挂上 ext4");

    // ext4 的 `EntryId` 由 `entries()` 建立 ⇒ 先列一次（这也是它的文档口径）
    let cancel = CancellationToken::new();
    let tree = fs.entries(&EntryLimit::default(), &cancel).unwrap();
    let id = tree.by_name("a.txt").expect("a.txt 在树里").id;

    fs.apply(
        &Change::Replace {
            entry: id,
            src: src.clone(),
        },
        Some(&src),
        &cancel,
    )
    .expect("替换应成功");
    fs.flush().expect("fs 落盘应成功");
    drop(fs);
    chain.commit(None, Some(&cancel)).expect("提交应成功");

    // ① 原档字节确实变了 + 新内容读得回
    let after = std::fs::read(&path).unwrap();
    assert_ne!(before, after, "提交后原档字节必须改变");
    assert_eq!(read_file(&path, "a.txt"), NEW_A);
    assert_eq!(read_file(&path, "b.txt"), b"keep-b", "未改动的条目必须原样");
    // ② 冗余 GD 与主 GD 同步
    assert_redundant_gd_matches_primary(&after);
    // ③ 正常关闭后 `uncleanShutdown` 必须归零
    assert_eq!(
        after[common::O_UNCLEAN],
        0,
        "flush 之后 uncleanShutdown 必须为 0"
    );
    // ④ content id 已更新（上游负责；只断言「变了」且是十六进制串）
    let cid_after = content_id(&after);
    assert_ne!(cid_after, cid_before, "content id 应随内容更新");
    assert!(!cid_after.is_empty());
    // ⑤ 未改动的块原样：稀疏头与 GD/RGD 都没被碰（grain 已预分配 ⇒ 全程写穿）
    assert_sparse_metadata_untouched(&before, &after);
}

// --------------------------------------- 二、editor 就地写：Replace

#[test]
fn write_in_place_replaces_and_commits() {
    let (_dir, path, src) = fixture();
    let id = archive_id(&path, "a.txt");
    let before = sha256_file(&path);
    let raw_before = std::fs::read(&path).unwrap();
    let b_before = read_file(&path, "b.txt");
    let c_before = read_file(&path, "c.txt");

    let handler = VmdkHandler::new(registry());
    let editor = handler
        .open_editor(Source::File(path.clone()), &OpenOptions::default())
        .unwrap();
    let ctx = Ctx::new(VolumeWriteMode::InPlace);
    let cs = ChangeSet {
        changes: vec![Change::Replace {
            entry: id,
            src: src.clone(),
        }],
    };
    let rep = editor
        .write_in_place(cs, &ctx.wctx())
        .expect("就地写应成功");
    assert_eq!(rep.entries_written, 1);

    // 提交后原档变了，且**重开**读回新内容
    assert_ne!(sha256_file(&path), before, "提交后原档应改变");
    assert_eq!(read_file(&path, "a.txt"), NEW_A);
    // 未改动的条目逐字节原样
    assert_eq!(read_file(&path, "b.txt"), b_before);
    assert_eq!(read_file(&path, "c.txt"), c_before);
    // 稀疏元数据原样 + 冗余 GD 同步 + 无脏标记
    let raw_after = std::fs::read(&path).unwrap();
    assert_sparse_metadata_untouched(&raw_before, &raw_after);
    assert_redundant_gd_matches_primary(&raw_after);
    assert_eq!(raw_after[common::O_UNCLEAN], 0);
}

// --------------------------------------- 三、editor 就地写：Delete

#[test]
fn write_in_place_deletes_and_commits() {
    let (_dir, path, _src) = fixture();
    let id = archive_id(&path, "b.txt");
    let before = sha256_file(&path);
    let a_before = read_file(&path, "a.txt");
    let c_before = read_file(&path, "c.txt");

    let handler = VmdkHandler::new(registry());
    let editor = handler
        .open_editor(Source::File(path.clone()), &OpenOptions::default())
        .unwrap();
    let ctx = Ctx::new(VolumeWriteMode::InPlace);
    let rep = editor
        .write_in_place(
            ChangeSet {
                changes: vec![Change::Delete(id)],
            },
            &ctx.wctx(),
        )
        .expect("删除应成功");
    assert_eq!(rep.entries_written, 1);

    assert_ne!(sha256_file(&path), before, "提交后原档应改变");
    let names = projected_names(&path);
    assert!(
        !names.contains(&"Partition 1/b.txt".to_string()),
        "b.txt 应已删除：{names:?}"
    );
    assert!(
        names.contains(&"Partition 1/a.txt".to_string()),
        "{names:?}"
    );
    // 未改动的条目逐字节原样
    assert_eq!(read_file(&path, "a.txt"), a_before);
    assert_eq!(read_file(&path, "c.txt"), c_before);
    assert_eq!(std::fs::read(&path).unwrap()[common::O_UNCLEAN], 0);
}

// ------------------------------- 四、editor 覆盖写：Replace + Delete → 新档

#[test]
fn rewrite_stages_replace_and_delete_without_touching_the_source() {
    let (dir, path, src) = fixture();
    let (id_a, id_b) = (archive_id(&path, "a.txt"), archive_id(&path, "b.txt"));
    let before = sha256_file(&path);
    let c_before = read_file(&path, "c.txt");

    let handler = VmdkHandler::new(registry());
    let editor = handler
        .open_editor(Source::File(path.clone()), &OpenOptions::default())
        .unwrap();
    let out = dir.path().join("out.vmdk");
    let ctx = Ctx::new(VolumeWriteMode::Overlay);
    let rep = editor
        .rewrite(
            ChangeSet {
                changes: vec![
                    Change::Replace {
                        entry: id_a,
                        src: src.clone(),
                    },
                    Change::Delete(id_b),
                ],
            },
            Sink::File(out.clone()),
            &ctx.wctx(),
        )
        .expect("覆盖写应成功");
    assert_eq!(rep.entries_written, 2);

    // **提交前（含提交）原档一个字节不动**
    assert_eq!(sha256_file(&path), before, "覆盖写不得改动源档案");
    // 产物新档含改动，且重开能读回
    assert_ne!(sha256_file(&out), before, "产物必须包含改动");
    assert_eq!(read_file(&out, "a.txt"), NEW_A);
    assert_eq!(read_file(&out, "c.txt"), c_before, "未改动的条目必须原样");
    let names = projected_names(&out);
    assert!(
        !names.contains(&"Partition 1/b.txt".to_string()),
        "b.txt 应已删除：{names:?}"
    );
    assert_redundant_gd_matches_primary(&std::fs::read(&out).unwrap());
}

// ----------------------- 五、设备层：覆盖链的写入在 commit 前不落原档

#[test]
fn overlay_chain_writes_stay_in_the_staging_layer_until_commit() {
    let (_dir, path, src) = fixture();
    let before = sha256_file(&path);

    let opts = vol_opts(true);
    let chain = Chain::open(Source::File(path.clone()), WriteMode::Overlay, &opts, None).unwrap();
    assert_eq!(chain.mode(), WriteMode::Overlay);
    let vol = VmdkVolume::new(chain.device(), opts).unwrap();
    assert!(vol.caps().writable, "覆盖链上的 sparse 应可写");
    let mut fs = vol.open_fs(Some(0)).unwrap().expect("分区 0 应挂上 ext4");
    let cancel = CancellationToken::new();
    let tree = fs.entries(&EntryLimit::default(), &cancel).unwrap();
    let id = tree.by_name("a.txt").expect("a.txt 在树里").id;
    fs.apply(
        &Change::Replace {
            entry: id,
            src: src.clone(),
        },
        Some(&src),
        &cancel,
    )
    .expect("暂存层里的替换应成功");
    fs.flush().unwrap();
    drop(fs);

    // 提交前原档 sha256 不变（改动还在溢出文件里）
    assert_eq!(sha256_file(&path), before, "提交前原档必须一个字节不动");

    chain.commit(None, Some(&cancel)).expect("回放应成功");
    assert_ne!(sha256_file(&path), before, "提交后原档应变化");
    assert_eq!(read_file(&path, "a.txt"), NEW_A);
}
