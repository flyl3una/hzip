//! VMDK 的**按名字拒绝**：布局分类纯函数的穷举单测 + 打开路径的拒写口径。
//!
//! 两条硬要求在这里被钉死：
//!
//! 1. **纯函数穷举**：每个 `createType` 字符串、魔数分支、大小写、CRLF、
//!    NUL 填充、空描述符，各自有断言（`classify_layout` 不看数据区）；
//! 2. **不读数据区就拒绝**：拒写用例用 `CountingDevice` 量读量——「拒绝」不能靠
//!    「先读进来再说」，那正是「误读」的入口。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use hzip_fmt_vmdk::volume::{
    KEY_DESCRIPTOR, KEY_LAYOUT_FLAT, KEY_LAYOUT_VMFS, KEY_MULTI_EXTENT, KEY_SNAPSHOT_DELTA,
    KEY_UNKNOWN_LAYOUT, Layout, VmdkVolume, classify_layout, fs_options_for,
};
use hzip_fmt_vmdk::{PROBE_HEAD_BYTES, VmdkHandler, vmdk_probe};
use hzip_format::{FormatHandler as _, OpenOptions, Source};
use hzip_vol::VolOptions;
use hzip_vol::error::VolError;
use hzip_vol::{BlockDevice, Volume as _};

use common::{
    SECTOR, counting_read_only, registry, vmdk_descriptor_file, vmdk_sparse, vmdk_vmfs_cowd,
    write_temp,
};

fn kdmv_head() -> Vec<u8> {
    let img = vmdk_sparse(&vec![0u8; 1 << 20]);
    img[..PROBE_HEAD_BYTES].to_vec()
}

fn cowd_head() -> Vec<u8> {
    vmdk_vmfs_cowd()[..PROBE_HEAD_BYTES].to_vec()
}

fn writable_opts() -> VolOptions {
    VolOptions {
        writable: true,
        fs: fs_options_for(true),
        fs_registry: registry(),
        ..VolOptions::default()
    }
}

/// 拒绝键（`VolError::Unsupported` 的稳定文案键）。
fn refuse_key(dev: std::sync::Arc<dyn BlockDevice>) -> &'static str {
    match VmdkVolume::new(dev, writable_opts()) {
        Ok(_) => panic!("该布局应被拒绝"),
        Err(VolError::Unsupported(key)) => key,
        Err(other) => panic!("应报 Unsupported，得到 {other:?}"),
    }
}

// ------------------------------------------- 布局分类：纯函数穷举

#[test]
fn classify_covers_every_create_type() {
    let h = kdmv_head();
    let cases: &[(&str, Layout)] = &[
        ("monolithicSparse", Layout::MonolithicSparse),
        ("streamOptimized", Layout::StreamOptimized),
        ("monolithicFlat", Layout::MonolithicFlat),
        ("fullDevice", Layout::MonolithicFlat),
        ("partitionedDevice", Layout::MonolithicFlat),
        ("twoGbMaxExtentSparse", Layout::TwoGbMaxExtent),
        ("twoGbMaxExtentFlat", Layout::TwoGbMaxExtent),
        ("vmfsSparse", Layout::VmfsSparse),
        ("vmfs", Layout::VmfsSparse),
    ];
    for (ty, want) in cases {
        let text = format!("createType=\"{ty}\"\n");
        assert_eq!(
            classify_layout(&h, Some(&text)).unwrap(),
            *want,
            "createType={ty}"
        );
        // 无引号写法（真实产物两种都有）
        let bare = format!("createType={ty}\n");
        assert_eq!(
            classify_layout(&h, Some(&bare)).unwrap(),
            *want,
            "{ty} 无引号"
        );
    }
}

#[test]
fn classify_is_case_insensitive_on_key_and_value() {
    let h = kdmv_head();
    for text in [
        "createType=\"MONOLITHICSPARSE\"\n",
        " createType = \"MonolithicSparse\" \n",
        "CREATETYPE=\"monolithicsparse\"\n",
    ] {
        assert_eq!(
            classify_layout(&h, Some(text)).unwrap(),
            Layout::MonolithicSparse,
            "{text:?}"
        );
    }
}

#[test]
fn classify_tolerates_crlf_nul_padding_and_comments() {
    let h = kdmv_head();
    let text = "# Disk DescriptorFile\r\nversion=1\r\ncreateType=\"monolithicSparse\"\r\n\r\n\
                RW 2048 SPARSE \"x.vmdk\"\r\n\0\0\0\0";
    assert_eq!(
        classify_layout(&h, Some(text)).unwrap(),
        Layout::MonolithicSparse
    );
}

#[test]
fn classify_magic_branch_wins_over_descriptor() {
    // COWD 头 ⇒ vmfsSparse，**无论描述符说什么**
    assert_eq!(
        classify_layout(&cowd_head(), None).unwrap(),
        Layout::VmfsSparse
    );
    assert_eq!(
        classify_layout(&cowd_head(), Some("createType=\"monolithicSparse\"\n")).unwrap(),
        Layout::VmfsSparse
    );
}

#[test]
fn classify_missing_or_broken_descriptor_is_named_not_guessed() {
    let h = kdmv_head();
    assert_eq!(refused(&h, None), KEY_DESCRIPTOR, "空描述符 ⇒ 不猜");
    assert_eq!(
        refused(&h, Some("   \0\0\n")),
        KEY_DESCRIPTOR,
        "全空白/NUL 描述符 ⇒ 不猜"
    );
    assert_eq!(
        refused(&h, Some("createType=\"\"\n")),
        KEY_DESCRIPTOR,
        "空 createType 值 ⇒ 不猜"
    );
    assert_eq!(
        refused(&h, Some("createType=\"someOtherLayout\"\n")),
        KEY_UNKNOWN_LAYOUT,
        "未知 createType ⇒ 稳定键"
    );
}

/// 纯函数拒绝键（`classify_layout` 的 `Unsupported`）。
fn refused(head: &[u8], desc: Option<&str>) -> &'static str {
    match classify_layout(head, desc) {
        Ok(layout) => panic!("应被拒绝，得到 {layout:?}"),
        Err(VolError::Unsupported(key)) => key,
        Err(other) => panic!("应报 Unsupported，得到 {other:?}"),
    }
}

#[test]
fn classify_detects_snapshot_delta_by_parent_linkage() {
    let h = kdmv_head();
    // 父文件名提示
    let text = "createType=\"monolithicSparse\"\nparentFileNameHint=\"base.vmdk\"\n\
                RW 2048 SPARSE \"x.vmdk\"\n";
    assert_eq!(
        classify_layout(&h, Some(text)).unwrap(),
        Layout::SnapshotDelta
    );
    // parentCID 非哨兵
    let text = "createType=\"monolithicSparse\"\nparentCID=1234abcd\nRW 2048 SPARSE \"x.vmdk\"\n";
    assert_eq!(
        classify_layout(&h, Some(text)).unwrap(),
        Layout::SnapshotDelta
    );
    // 哨兵/空值 ⇒ 不是 delta（假阳性会误拒一个完全正常的盘）
    for text in [
        "createType=\"monolithicSparse\"\nparentCID=ffffffff\nRW 2048 SPARSE \"x.vmdk\"\n",
        "createType=\"monolithicSparse\"\nparentCID=0xffffffff\nRW 2048 SPARSE \"x.vmdk\"\n",
        "createType=\"monolithicSparse\"\nparentCID=0XFFFFFFFF\nRW 2048 SPARSE \"x.vmdk\"\n",
        "createType=\"monolithicSparse\"\nparentFileNameHint=\"\"\nRW 2048 SPARSE \"x.vmdk\"\n",
    ] {
        assert_eq!(
            classify_layout(&h, Some(text)).unwrap(),
            Layout::MonolithicSparse,
            "{text:?}"
        );
    }
}

// ------------------------------------------- 打开路径：按名字拒绝 + 读量

#[test]
fn monolithic_flat_is_refused_by_name_without_touching_data() {
    let img = vmdk_descriptor_file("monolithicFlat", "FLAT", 2048, None);
    let (counter, dev) = counting_read_only(img);
    assert_eq!(refuse_key(dev), KEY_LAYOUT_FLAT);
    assert!(counter.reads() <= 4, "读次数 {}", counter.reads());
    assert!(counter.bytes() <= 4096, "读字节 {}", counter.bytes());
}

#[test]
fn two_gb_max_extent_is_refused_by_name() {
    for ty in ["twoGbMaxExtentSparse", "twoGbMaxExtentFlat"] {
        let img = vmdk_descriptor_file(ty, "SPARSE", 2048, None);
        let (counter, dev) = counting_read_only(img);
        assert_eq!(refuse_key(dev), KEY_MULTI_EXTENT, "{ty}");
        assert!(counter.bytes() <= 4096, "{ty} 读字节 {}", counter.bytes());
    }
}

#[test]
fn vmfs_sparse_cowd_is_refused_by_the_magic_alone() {
    let (counter, dev) = counting_read_only(vmdk_vmfs_cowd());
    assert_eq!(refuse_key(dev), KEY_LAYOUT_VMFS);
    assert_eq!(counter.reads(), 1, "COWD 只需读一次头");
    assert_eq!(counter.bytes(), PROBE_HEAD_BYTES as u64);
}

#[test]
fn vmfs_sparse_create_type_is_refused_by_name() {
    let img = vmdk_descriptor_file("vmfsSparse", "SPARSE", 2048, None);
    let (_counter, dev) = counting_read_only(img);
    assert_eq!(refuse_key(dev), KEY_LAYOUT_VMFS);
}

#[test]
fn snapshot_delta_is_refused_by_name() {
    let img = vmdk_descriptor_file("monolithicSparse", "SPARSE", 2048, Some("base.vmdk"));
    let (_counter, dev) = counting_read_only(img);
    assert_eq!(refuse_key(dev), KEY_SNAPSHOT_DELTA);
}

#[test]
fn sparse_extent_without_a_descriptor_is_refused_without_touching_grains() {
    // KDMV 头但 descriptorOffset/descriptorSize 为 0（分卷 extent 的常态）
    let mut img = vmdk_sparse(&vec![0u8; 4 << 20]);
    img[28..44].fill(0); // descriptor_offset + descriptor_size
    let (counter, dev) = counting_read_only(img);
    assert_eq!(refuse_key(dev), KEY_DESCRIPTOR);
    assert_eq!(counter.reads(), 1, "只读一次头就够下结论");
    assert_eq!(counter.bytes(), PROBE_HEAD_BYTES as u64);

    // 描述符扇区数离谱（超出任何真实产物）⇒ 不按它去读
    let mut img = vmdk_sparse(&vec![0u8; 4 << 20]);
    img[36..44].copy_from_slice(&(1u64 << 40).to_le_bytes()); // descriptor_size
    let (counter, dev) = counting_read_only(img);
    assert_eq!(refuse_key(dev), KEY_DESCRIPTOR);
    assert_eq!(counter.reads(), 1);
}

// ------------------------------------------------- 只读设备末端（浏览路径）

#[test]
fn read_only_device_end_yields_device_reason() {
    // 布局 OK、也想要写，但设备末端只读（浏览链的形态）⇒ DeviceReadOnly，
    // 且**不是**错误：卷照常打开、只读。
    let img = vmdk_sparse(&raw_with_mbr_for_refuse());
    let (counter, dev) = counting_read_only(img);
    let vol = VmdkVolume::new(dev, writable_opts()).expect("只读末端不该让卷打不开");
    let caps = vol.caps();
    assert!(!caps.writable);
    assert_eq!(
        caps.read_only_reason,
        Some(hzip_vol::ReadOnlyReason::DeviceReadOnly)
    );
    assert!(counter.bytes() > 0, "确实读了（只是只读）");
    // 分区表探测也要能在虚拟盘上跑通
    assert_eq!(vol.partitions().unwrap().len(), 1);
}

/// 一小块 MBR 盘（拒写用例只关心「能打开、只读」，内容不重要）。
fn raw_with_mbr_for_refuse() -> Vec<u8> {
    use hzip_format_testkit::vol::{FixtureFs, raw_with_mbr};
    raw_with_mbr(FixtureFs::Ext4, &[("/a", b"b")])
}

#[test]
fn fixture_sector_constant_matches_the_format() {
    assert_eq!(SECTOR, 512, "VMDK 逻辑扇区固定 512（格式所致，不是可配项）");
}

// ------------------------------------- 扩展名回退：flat 描述符文件进到 handler

#[test]
fn flat_descriptor_reaches_the_handler_by_extension_fallback() {
    let img = vmdk_descriptor_file("monolithicFlat", "FLAT", 2048, None);
    // 探针不认领（flat 没有 sparse 头）⇒ core 只能靠扩展名 `.vmdk` 回退进本 handler
    let mut head = img.clone();
    head.resize(PROBE_HEAD_BYTES, 0);
    assert_eq!(vmdk_probe(&head), None, "flat 不该被探针认领");

    let (_dir, path) = write_temp("flat.vmdk", &img);
    let handler = VmdkHandler::new(registry());
    let err = match handler.open_reader(Source::File(path), &OpenOptions::default()) {
        Ok(_) => panic!("flat 描述符文件必须被拒绝"),
        Err(e) => e,
    };
    assert!(
        matches!(err, hzip_error::HzipError::Unsupported { ref what } if what == KEY_LAYOUT_FLAT),
        "回退进 handler 后必须按名字拒绝：{err:?}"
    );
}
