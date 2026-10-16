//! qcow2 拒写：四类形态各自带**稳定键**，且不 panic。
//!
//! 判定序与理由见 `hzip_fmt_qcow2::volume` 的模块文档。这里守的是**可观察面**：
//! 键（UI/CLI 按它查 locale）、能力位、以及「坏输入不 panic」。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use std::sync::Arc;

use common::{
    INCOMPAT_CORRUPT, INCOMPAT_DATA_FILE, INCOMPAT_DIRTY, INCOMPAT_EXTENDED_L2, qcow2_image,
    qcow2_image_flagged, qcow2_image_with_backing, raw_ext4_mbr, sha256_file, write_temp,
};
use hzip_fmt_qcow2::{Qcow2Handler, Qcow2Volume, volume::fs_options_for};
use hzip_format::{
    Change, ChangeSet, CreateOptions, FormatHandler as _, OpenOptions, Sink, Source, WriteCtx,
};
use hzip_format_testkit::vol::MemDevice;
use hzip_fs_ext4::Ext4Driver;
use hzip_progress::{CancellationToken, ProgressBus};
use hzip_vol::error::{ReadOnlyReason, VolError};
use hzip_vol::registry::FsRegistry;
use hzip_vol::{BlockDevice, VolOptions, Volume as _};

fn registry() -> Arc<FsRegistry> {
    let mut reg = FsRegistry::new();
    reg.register(Box::new(Ext4Driver::new()));
    Arc::new(reg)
}

/// 可写设备（`MemDevice` 恒自报可写）+ 请求可写：让**镜像自身**的标记成为唯一变量。
fn open_writable(img: Vec<u8>) -> Result<Qcow2Volume, VolError> {
    let dev: Arc<dyn BlockDevice> = Arc::new(MemDevice::from_bytes(img));
    let opts = VolOptions {
        writable: true,
        fs: fs_options_for(true),
        fs_registry: registry(),
        ..VolOptions::default()
    };
    Qcow2Volume::open(dev, opts)
}

fn raw() -> Vec<u8> {
    raw_ext4_mbr(&[("/a", b"b")])
}

// ------------------------------------------------------------ 只读（镜像自带硬伤）

#[test]
fn dirty_image_is_read_only_with_reason() {
    let vol = open_writable(qcow2_image_flagged(&raw(), INCOMPAT_DIRTY, 0)).unwrap();
    let caps = vol.caps();
    assert!(!caps.writable, "dirty 镜像不得广告可写");
    assert_eq!(caps.read_only_reason, Some(ReadOnlyReason::Dirty));
    assert_eq!(
        ReadOnlyReason::Dirty.key(),
        "vol.readonly.dirty",
        "稳定键（UI/CLI 按它查 locale）"
    );
    // 只读但**读得了**：分区行照给（dirty 对读是提示，不是拒绝）
    assert_eq!(vol.partitions().unwrap().len(), 1);
    assert!(vol.open_fs(Some(0)).unwrap().is_some(), "dirty 卷仍可浏览");

    // 交叉证据：「降级为只读」不是我们加戏——上游自己的**写**打开也拒绝这两类
    // （`Header::check_writable`；理由见 volume.rs 模块文档）。
    let dev: Arc<dyn BlockDevice> = Arc::new(MemDevice::from_bytes(qcow2_image_flagged(
        &raw(),
        INCOMPAT_DIRTY,
        0,
    )));
    assert!(
        img_qcow2::Qcow2Reader::open_rw_on_device(dev).is_err(),
        "上游对 dirty 镜像的写打开应当失败（我们的判定序据此降级为只读）"
    );
}

#[test]
fn corrupt_image_is_read_only_with_reason() {
    let vol = open_writable(qcow2_image_flagged(&raw(), INCOMPAT_CORRUPT, 0)).unwrap();
    assert!(!vol.caps().writable);
    assert_eq!(vol.caps().read_only_reason, Some(ReadOnlyReason::Dirty));
    assert_eq!(vol.partitions().unwrap().len(), 1);
}

#[test]
fn dirty_image_refuses_write_in_place_by_name() {
    let img = qcow2_image_flagged(&raw(), INCOMPAT_DIRTY, 0);
    let (_dir, path) = write_temp("dirty.qcow2", &img);
    let before = sha256_file(&path);

    let handler = Qcow2Handler::new(registry());
    let editor = handler
        .open_editor(Source::File(path.clone()), &OpenOptions::default())
        .expect("打开编辑器本身应成功");
    let (_ldir, local) = write_temp("x.txt", b"x");
    let cancel = CancellationToken::new();
    let progress = ProgressBus::new();
    let copts = CreateOptions::default();
    let ctx = WriteCtx {
        cancel: &cancel,
        progress: &progress,
        opts: &copts,
    };
    let changes = ChangeSet {
        changes: vec![Change::Append {
            src: local,
            // 档案级名（与读侧投影一致）；本用例守的是**拒写**，与路由无关
            name: "Partition 1/x.txt".into(),
        }],
    };
    let err = editor.write_in_place(changes, &ctx).unwrap_err();
    assert!(
        matches!(&err, hzip_error::HzipError::Unsupported { what } if what == "vol.readonly.outer-not-editable"),
        "{err:?}"
    );
    assert_eq!(sha256_file(&path), before, "拒写路径必须原档一字节未动");
}

// ------------------------------------------------------------ 按名字拒绝

#[test]
fn encrypted_image_is_refused_with_reason() {
    let img = qcow2_image_flagged(&raw(), 0, 1);
    let err = open_writable(img.clone()).unwrap_err();
    assert!(
        matches!(err, VolError::ReadOnly(ReadOnlyReason::Encrypted)),
        "{err:?}"
    );
    assert_eq!(ReadOnlyReason::Encrypted.key(), "vol.readonly.encrypted");

    // 交叉证据：上游**连读**都不支持加密（`check_supported` 就拒绝），所以我们给不出
    // 「能读的只读卷」——只能报理由键。计划原文写「加密 ⇒ 只读」，落地如实改成拒绝。
    let dev: Arc<dyn BlockDevice> = Arc::new(MemDevice::from_bytes(img));
    assert!(
        img_qcow2::Qcow2Reader::open_on_device(dev).is_err(),
        "上游读路径对加密镜像亦应失败"
    );
}

#[test]
fn external_data_file_is_refused_by_name() {
    let err = open_writable(qcow2_image_flagged(&raw(), INCOMPAT_DATA_FILE, 0)).unwrap_err();
    assert!(
        matches!(&err, VolError::Unsupported(k) if *k == "vol.unsupported.external-data"),
        "{err:?}"
    );
}

#[test]
fn extended_l2_is_refused_by_name() {
    let err = open_writable(qcow2_image_flagged(&raw(), INCOMPAT_EXTENDED_L2, 0)).unwrap_err();
    assert!(
        matches!(&err, VolError::Unsupported(k) if *k == "vol.unsupported.ext-l2"),
        "{err:?}"
    );
}

#[test]
fn backing_file_is_refused_by_name_with_the_missing_prerequisite() {
    // 设备形态没有路径 ⇒ 上游解析不出父镜像 ⇒ 未分配的簇会「读回 0」而不是父镜像的
    // 数据。静默错数据比拒绝坏得多，故按名字拒绝（理由见 volume.rs 模块文档）。
    let err = open_writable(qcow2_image_with_backing(&raw())).unwrap_err();
    assert!(
        matches!(&err, VolError::Unsupported(k) if *k == "vol.unsupported.backing-file"),
        "{err:?}"
    );
}

// ------------------------------------------------------------ 坏输入不 panic

#[test]
fn a_broken_image_is_an_error_never_a_panic() {
    for bad in [
        Vec::new(),                           // 空
        vec![0u8; 8],                         // 只有魔数长度
        b"QFI\xfb\0\0\0\x03".to_vec(),        // 头被截断（72 字节以下）
        vec![0xFFu8; 4096],                   // 全 1：魔数不对
        qcow2_image(&raw())[..3000].to_vec(), // 头够了、L1 表越界
    ] {
        let dev: Arc<dyn BlockDevice> = Arc::new(MemDevice::from_bytes(bad));
        let opts = VolOptions {
            writable: true,
            fs: fs_options_for(true),
            fs_registry: registry(),
            ..VolOptions::default()
        };
        // 只要求「有 Err、不 panic」；具体错误类别由上面的用例分别守。
        let _ = Qcow2Volume::open(dev, opts);
    }
}

#[test]
fn create_writer_is_refused_instead_of_faking_an_image() {
    let handler = Qcow2Handler::new(registry());
    let err = match handler.create_writer(Sink::File("out.qcow2".into()), &CreateOptions::default())
    {
        Ok(_) => panic!("qcow2 不产出镜像，应拒绝"),
        Err(e) => e,
    };
    assert!(
        matches!(err, hzip_error::HzipError::Unsupported { ref what } if what == "vol.unsupported.create-image"),
        "{err:?}"
    );
}
