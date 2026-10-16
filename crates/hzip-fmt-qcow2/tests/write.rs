//! qcow2 写侧：覆盖写暂存 / 就地提交 / 重写副本三条路径的最小往返。
//!
//! 覆盖写的语义（T-IMG6）在 qcow2 上的落地：改动先落 `OverlayDevice`，
//! **提交前原档一个字节不动**；提交时只把宿主脏块回放回原文件。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use std::sync::Arc;

use common::{CLUSTER_SIZE, qcow2_image, qcow2_image_with, raw_ext4_mbr, sha256_file, write_temp};
use hzip_fmt_qcow2::{Qcow2Handler, Qcow2Volume, volume::fs_options_for};
use hzip_format::{
    Change, ChangeSet, CreateOptions, FormatHandler as _, OpenOptions, PasswordRef, Sink, Source,
    WriteCtx,
};
use hzip_fs_ext4::Ext4Driver;
use hzip_progress::{CancellationToken, ProgressBus};
use hzip_vol::device::{Chain, WriteMode};
use hzip_vol::registry::FsRegistry;
use hzip_vol::{BlockDevice, BlockRead as _, VolOptions, Volume as _};

fn registry() -> Arc<FsRegistry> {
    let mut reg = FsRegistry::new();
    reg.register(Box::new(Ext4Driver::new()));
    Arc::new(reg)
}

fn vol_opts(writable: bool) -> VolOptions {
    VolOptions {
        writable,
        fs: fs_options_for(writable),
        fs_registry: registry(),
        ..VolOptions::default()
    }
}

/// 读回某个分区内文件的内容（走完整链路：qcow2 → 分区 → ext4）。
fn read_from_image(path: &std::path::Path, suffix: &str) -> Vec<u8> {
    let handler = Qcow2Handler::new(registry());
    let reader = handler
        .open_reader(Source::File(path.to_path_buf()), &OpenOptions::default())
        .unwrap();
    let entries = reader.entries().unwrap();
    let id = entries
        .iter()
        .position(|e| e.name.as_str().ends_with(suffix))
        .unwrap_or_else(|| {
            panic!(
                "镜像里应有 {suffix}；实际 = {:?}",
                entries
                    .iter()
                    .map(|e| e.name.as_str().to_string())
                    .collect::<Vec<_>>()
            )
        });
    let mut buf = Vec::new();
    std::io::Read::read_to_end(
        &mut reader.read_entry(id, PasswordRef::none()).unwrap(),
        &mut buf,
    )
    .unwrap();
    buf
}

// ------------------------------------------------------------ 覆盖写暂存语义

/// 直接驱动设备链：写进**虚拟盘**（走 qcow2 的簇分配 + refcount），
/// 断言提交前原档一字节未动、提交后可读回。
#[test]
fn overlay_staging_keeps_source_untouched_until_commit() {
    let raw = raw_ext4_mbr(&[("/a.txt", b"old")]);
    // 末尾 8 个未分配虚拟簇（写进去会触发簇分配）+ 4 个文件内空闲宿主簇
    // （分配器有处可落，不必走 `set_len`——覆盖层不支持扩容）。
    let img = qcow2_image_with(&raw, &[], 8, 4);
    let (_dir, path) = write_temp("stage.qcow2", &img);
    let before = sha256_file(&path);
    let header_before = std::fs::read(&path).unwrap()[..512].to_vec();

    let opts = vol_opts(true);
    let chain = Chain::open(Source::File(path.clone()), WriteMode::Overlay, &opts, None).unwrap();
    let vol = Qcow2Volume::open(chain.device(), opts).unwrap();
    assert!(vol.caps().writable, "覆盖链上的 qcow2 应可写");
    assert!(vol.caps().in_place, "源是常规文件 ⇒ 提交只回放脏块");
    assert_eq!(vol.caps().read_only_reason, None);

    // 写入第一个未分配虚拟簇：qcow2 必须分配新簇并更新 refcount 表/L2
    let tail = raw.len() as u64;
    let payload = vec![0xABu8; 4096];
    let vdev = vol.device();
    vdev.write_at(tail, &payload).unwrap();
    vdev.flush().unwrap();

    assert_eq!(sha256_file(&path), before, "提交前原档必须一个字节都不动");
    assert_eq!(
        std::fs::read(&path).unwrap()[..512],
        header_before[..],
        "暂存期间连头部都不该被碰"
    );

    chain.commit(None, None).unwrap();
    assert_ne!(sha256_file(&path), before, "提交后原档应变化");
    assert_eq!(
        std::fs::read(&path).unwrap()[..512],
        header_before[..],
        "回放的宿主脏块只该是数据簇与 L2 表——头部块不在其中（覆盖层挂在 qcow2 下面）"
    );

    // 重开复核：新写入的字节读得回来，且原有内容仍在
    let vol2 = Qcow2Volume::open(
        Arc::new(hzip_format_testkit::vol::MemDevice::from_bytes(
            std::fs::read(&path).unwrap(),
        )),
        vol_opts(false),
    )
    .unwrap();
    let vdev2 = vol2.device();
    let mut back = vec![0u8; payload.len()];
    vdev2.read_at(tail, &mut back).unwrap();
    assert_eq!(back, payload, "提交后新簇的内容必须读得回来");
    assert_eq!(
        vol2.geometry().virtual_size,
        raw.len() as u64 + 8 * CLUSTER_SIZE
    );
    assert_eq!(read_from_image(&path, "a.txt"), b"old", "原有内容不受影响");
}

// ------------------------------------------------------------ 就地写 / 重写

/// 变更集：名字必须是**档案级**（读侧投影名 = 分区行名 + `/` + 卷内路径）。
///
/// 编辑器收的是 core 传下来的档案级变更，路由（剥前缀 → 卷内相对路径）由
/// `VolumeEditor` 负责 —— 这里若写卷内的裸名（`new.txt`），路由会按键拒绝
/// （`vol.unsupported.change-route`），因为那条变更不指向镜像树里的任何条目。
fn change_set(local: std::path::PathBuf) -> ChangeSet {
    ChangeSet {
        changes: vec![Change::Append {
            src: local,
            name: "Partition 1/new.txt".into(),
        }],
    }
}

#[test]
fn write_in_place_round_trip_through_ext4() {
    let raw = raw_ext4_mbr(&[("/a.txt", b"old")]);
    // 预留空闲宿主簇：ext4 若需要新块，qcow2 有处可落（不必扩容）。
    let (_dir, path) = write_temp("rw.qcow2", &qcow2_image_with(&raw, &[], 0, 8));
    let (_ldir, local) = write_temp("new.txt", b"NEW-CONTENT");

    let handler = Qcow2Handler::new(registry());
    let editor = handler
        .open_editor(Source::File(path.clone()), &OpenOptions::default())
        .unwrap();

    let cancel = CancellationToken::new();
    let progress = ProgressBus::new();
    let copts = CreateOptions::default(); // volume_write = Overlay（默认）
    let ctx = WriteCtx {
        cancel: &cancel,
        progress: &progress,
        opts: &copts,
    };
    let report = editor.write_in_place(change_set(local), &ctx).unwrap();
    assert_eq!(report.entries_written, 1);

    // 重开复核：新条目在、内容对、旧条目还在
    assert_eq!(read_from_image(&path, "new.txt"), b"NEW-CONTENT");
    assert_eq!(read_from_image(&path, "a.txt"), b"old");
}

#[test]
fn rewrite_produces_a_readable_copy_without_touching_the_source() {
    let raw = raw_ext4_mbr(&[("/a.txt", b"old")]);
    let (_dir, path) = write_temp("src.qcow2", &qcow2_image_with(&raw, &[], 0, 8));
    let before = sha256_file(&path);
    let dst = path.with_file_name("copy.qcow2");
    let (_ldir, local) = write_temp("new.txt", b"REWRITTEN");

    let handler = Qcow2Handler::new(registry());
    let editor = handler
        .open_editor(Source::File(path.clone()), &OpenOptions::default())
        .unwrap();

    let cancel = CancellationToken::new();
    let progress = ProgressBus::new();
    let copts = CreateOptions::default();
    let ctx = WriteCtx {
        cancel: &cancel,
        progress: &progress,
        opts: &copts,
    };
    let report = editor
        .rewrite(change_set(local), Sink::File(dst.clone()), &ctx)
        .unwrap();
    assert_eq!(report.entries_written, 1);
    assert!(report.bytes_out > 0, "应写出脏块字节数");

    assert_eq!(sha256_file(&path), before, "rewrite 不得动源档");
    assert_eq!(read_from_image(&dst, "new.txt"), b"REWRITTEN");
    assert_eq!(read_from_image(&dst, "a.txt"), b"old");
}

#[test]
fn in_place_mode_writes_straight_to_the_source_file() {
    let raw = raw_ext4_mbr(&[("/a.txt", b"old")]);
    let (_dir, path) = write_temp("direct.qcow2", &qcow2_image_with(&raw, &[], 0, 8));
    let before = sha256_file(&path);
    let (_ldir, local) = write_temp("new.txt", b"DIRECT");

    let handler = Qcow2Handler::new(registry());
    let editor = handler
        .open_editor(Source::File(path.clone()), &OpenOptions::default())
        .unwrap();

    let cancel = CancellationToken::new();
    let progress = ProgressBus::new();
    let copts = CreateOptions {
        volume_write: hzip_format::VolumeWriteMode::InPlace,
        ..CreateOptions::default()
    };
    let ctx = WriteCtx {
        cancel: &cancel,
        progress: &progress,
        opts: &copts,
    };
    editor.write_in_place(change_set(local), &ctx).unwrap();

    assert_ne!(sha256_file(&path), before, "就地写直接改原档");
    assert_eq!(read_from_image(&path, "new.txt"), b"DIRECT");
}

#[test]
fn a_round_trip_leaves_the_image_a_valid_sparse_container() {
    // 「写后还能被重新打开」这条要在**容器层**也成立，不只看文件系统。
    let raw = raw_ext4_mbr(&[("/a.txt", b"old")]);
    let (_dir, path) = write_temp("valid.qcow2", &qcow2_image(&raw));
    let (_ldir, local) = write_temp("new.txt", b"x");

    let handler = Qcow2Handler::new(registry());
    let editor = handler
        .open_editor(Source::File(path.clone()), &OpenOptions::default())
        .unwrap();
    let cancel = CancellationToken::new();
    let progress = ProgressBus::new();
    let copts = CreateOptions::default();
    let ctx = WriteCtx {
        cancel: &cancel,
        progress: &progress,
        opts: &copts,
    };
    editor.write_in_place(change_set(local), &ctx).unwrap();

    let vol = Qcow2Volume::open(
        Arc::new(hzip_format_testkit::vol::MemDevice::from_bytes(
            std::fs::read(&path).unwrap(),
        )),
        vol_opts(false),
    )
    .unwrap();
    let geom = vol.geometry();
    assert!(geom.is_sparse);
    assert!(!geom.has_backing);
    assert_eq!(geom.virtual_size, raw.len() as u64);
}
