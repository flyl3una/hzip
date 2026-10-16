//! VMDK 容器的读侧测试：探针 + 端到端（**真实 ext4 驱动**）+ 空洞语义 + 只读判定。
//!
//! 「双击磁盘镜像自动挂载、分区当目录显示」这条用户需求，在 VMDK 上就是本文件：
//! 镜像层换成 VMDK（sparse / streamOptimized），分区层与文件系统层一行不改。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use std::sync::Arc;

use common::{
    GRAIN_SECTORS, SECTOR, registry, vmdk_sparse, vmdk_sparse_with_holes, vmdk_stream_optimized,
    write_temp,
};
use hzip_fmt_vmdk::volume::{VmdkVolume, fs_options_for};
use hzip_fmt_vmdk::{PROBE_HEAD_BYTES, VmdkHandler, vmdk_probe};
use hzip_format::{ArchiveReader as _, FormatHandler as _, OpenOptions, PasswordRef, Source};
use hzip_format_testkit::vol::{FixtureFs, MemDevice, raw_with_mbr};
use hzip_progress::CancellationToken;
use hzip_vol::reader::VolumeReader;
use hzip_vol::{BlockDevice, BlockRead as _, EntryLimit, ReadOnlyReason, VolOptions, Volume as _};

/// 只读打开的卷选项（浏览路径的形态）。
fn read_only_opts() -> VolOptions {
    VolOptions {
        writable: false,
        fs: fs_options_for(false),
        fs_registry: registry(),
        ..VolOptions::default()
    }
}

/// 想要写权限的卷选项（用来验「某些布局即使想写也只读」）。
fn writable_opts() -> VolOptions {
    VolOptions {
        writable: true,
        fs: fs_options_for(true),
        fs_registry: registry(),
        ..VolOptions::default()
    }
}

fn mem(img: Vec<u8>) -> Arc<dyn BlockDevice> {
    Arc::new(MemDevice::from_bytes(img))
}

// ------------------------------------------------------------------ 探针

#[test]
fn probe_recognises_sparse_and_stream_optimized_heads() {
    let sparse = vmdk_sparse(&vec![0u8; 1 << 20]);
    assert_eq!(
        vmdk_probe(&sparse[..PROBE_HEAD_BYTES]),
        Some(hzip_format::Confidence::High),
        "monolithicSparse 头应被认领"
    );
    let stream = vmdk_stream_optimized(2048);
    assert_eq!(
        vmdk_probe(&stream[..PROBE_HEAD_BYTES]),
        Some(hzip_format::Confidence::High),
        "streamOptimized 头也是 sparse extent 头"
    );
}

#[test]
fn probe_ignores_junk_flat_and_broken_heads() {
    // 反例：空/太短/纯文本/全零
    assert_eq!(vmdk_probe(&[]), None);
    assert_eq!(vmdk_probe(b"KDMV\x01\0\0\0"), None, "头长不足 ⇒ 不猜");
    assert_eq!(vmdk_probe(&[0u8; 512]), None);
    assert_eq!(
        vmdk_probe(b"this is a text descriptor file, not a sparse extent"),
        None
    );
    // 反例：flat 布局的描述符文件（无魔数）
    let flat = common::vmdk_descriptor_file("monolithicFlat", "FLAT", 2048, None);
    let mut head = flat.clone();
    head.resize(PROBE_HEAD_BYTES, 0);
    assert_eq!(
        vmdk_probe(&head),
        None,
        "flat 没有 sparse 头 ⇒ 探针不该认领"
    );

    // 反例：有魔数但字段不合理（版本 9 / grainSize 0 / numGTEsPerGT 0）
    let good = vmdk_sparse(&vec![0u8; 1 << 20]);
    let mut h = good[..PROBE_HEAD_BYTES].to_vec();
    h[4..8].copy_from_slice(&9u32.to_le_bytes());
    assert_eq!(vmdk_probe(&h), None, "版本不合理 ⇒ 不认领");
    let mut h = good[..PROBE_HEAD_BYTES].to_vec();
    h[20..28].copy_from_slice(&0u64.to_le_bytes());
    assert_eq!(vmdk_probe(&h), None, "grainSize 为 0 ⇒ 不认领");
    let mut h = good[..PROBE_HEAD_BYTES].to_vec();
    h[44..48].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(vmdk_probe(&h), None, "numGTEsPerGT 为 0 ⇒ 不认领");
}

// ------------------------------------------------------- 端到端（sparse + ext4）

#[test]
fn sparse_image_lists_partitions_and_tree() {
    let img = vmdk_sparse(&raw_with_mbr(
        FixtureFs::Ext4,
        &[
            ("/etc/fstab", b"LABEL=root /\n"),
            ("/dir/keep.txt", b"keep"),
        ],
    ));
    let (_dir, path) = write_temp("disk.vmdk", &img);
    let handler = VmdkHandler::new(registry());
    let reader = handler
        .open_reader(Source::File(path), &OpenOptions::default())
        .expect("打开 monolithicSparse 镜像");

    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        names.contains(&"Partition 1".to_string()),
        "分区行应在：{names:?}"
    );
    assert!(
        names.contains(&"Partition 1/etc/fstab".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"Partition 1/dir/keep.txt".to_string()),
        "{names:?}"
    );

    // 分区行的元数据（label/类型/文件系统）与 raw 容器同口径
    let rows = reader.volume_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "Partition 1");
    assert_eq!(
        rows[0].label, None,
        "MBR 分区无标签 ⇒ 界面按语言显示「分区 N」"
    );
    assert_eq!(rows[0].fs.as_deref(), Some("ext4"));

    // 读文件（随机 + 重复）
    let entries = reader.entries().unwrap();
    let id = entries
        .iter()
        .position(|e| e.name.as_str().ends_with("etc/fstab"))
        .unwrap();
    let mut buf = Vec::new();
    std::io::Read::read_to_end(
        &mut reader.read_entry(id, PasswordRef::none()).unwrap(),
        &mut buf,
    )
    .unwrap();
    assert_eq!(buf, b"LABEL=root /\n");
}

#[test]
fn sparse_geometry_is_sparse_and_reports_the_grain_size() {
    let img = vmdk_sparse(&vec![0u8; 4 << 20]);
    let vol = VmdkVolume::new(mem(img), read_only_opts()).unwrap();
    let geom = vol.geometry();
    assert_eq!(geom.virtual_size, 4 << 20, "虚拟容量 = 头里的 capacity");
    assert_eq!(geom.sector_size, 512);
    assert!(geom.is_sparse, "VMDK 有 grain 表 ⇒ 稀疏");
    assert!(!geom.has_backing, "能打开的镜像没有父链");
}

// --------------------------------------------------- streamOptimized：只读

#[test]
fn stream_optimized_reads_but_is_read_only() {
    let img = vmdk_stream_optimized(2048); // 1 MiB 虚拟盘，全空洞
    let vol = VmdkVolume::new(mem(img.clone()), writable_opts())
        .expect("streamOptimized 也应能打开（只读）");
    let caps = vol.caps();
    assert!(!caps.writable, "streamOptimized 不得广告可写");
    assert!(!caps.in_place, "只读 ⇒ 谈不上就地提交");
    assert_eq!(
        caps.read_only_reason,
        Some(ReadOnlyReason::Layout),
        "理由必须是 Layout（布局只读）"
    );

    // 「读可用」：整卷读取返回全零，且能建出索引
    let reader = VolumeReader::build(
        Box::new(vol),
        &EntryLimit::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert_eq!(
        names,
        vec!["Volume".to_string()],
        "全零盘 ⇒ 无分区表：{names:?}"
    );
    assert!(!reader.caps().edit, "只读布局不得广告可编辑");
}

// ------------------------------------------------------------- 空洞（sparse）

#[test]
fn unallocated_grain_reads_as_zeros() {
    // 虚拟内容：第 1 个 grain 全 0xAB。把它标成**未分配** ⇒ 读回应是全零。
    let mut raw = vec![0u8; 1 << 20];
    let grain = GRAIN_SECTORS as usize * SECTOR;
    raw[grain..2 * grain].fill(0xAB);

    let holed = VmdkVolume::new(mem(vmdk_sparse_with_holes(&raw, &[1])), read_only_opts()).unwrap();
    let mut buf = vec![0u8; 4096];
    holed
        .device()
        .read_at(grain as u64 + 100, &mut buf)
        .expect("空洞也要能读（不报错）");
    assert!(
        buf.iter().all(|&b| b == 0),
        "未分配 grain 必须读回 0，而不是源内容"
    );

    // 对照：无空洞版本读出的就是 0xAB（证明确实是「空洞语义」而非内容本来就零）
    let full = VmdkVolume::new(mem(vmdk_sparse(&raw)), read_only_opts()).unwrap();
    let mut buf = vec![0u8; 4096];
    full.device().read_at(grain as u64 + 100, &mut buf).unwrap();
    assert!(buf.iter().all(|&b| b == 0xAB), "已分配 grain 应读出源内容");
}

#[test]
fn hole_on_the_first_grain_hides_the_partition_table() {
    let disk = raw_with_mbr(FixtureFs::Ext4, &[("/a", b"b")]);
    let handler = VmdkHandler::new(registry());

    // 对照：grain 0 已分配 ⇒ MBR 可读 ⇒ 有分区行
    let (_d1, p1) = write_temp("plain.vmdk", &vmdk_sparse(&disk));
    let names: Vec<String> = handler
        .open_reader(Source::File(p1), &OpenOptions::default())
        .unwrap()
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(names.contains(&"Partition 1".to_string()), "{names:?}");

    // grain 0 未分配 ⇒ MBR 读成全零 ⇒ 无分区表，整卷当一行「未知卷」
    let (_d2, p2) = write_temp("holed.vmdk", &vmdk_sparse_with_holes(&disk, &[0]));
    let names: Vec<String> = handler
        .open_reader(Source::File(p2), &OpenOptions::default())
        .unwrap()
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert_eq!(
        names,
        vec!["Volume".to_string()],
        "首个 grain 是空洞 ⇒ 分区表读成零：{names:?}"
    );
}

// -------------------------------------------------------- 未干净关闭（脏标记）

#[test]
fn unclean_shutdown_reads_but_refuses_writes() {
    let mut img = vmdk_sparse(&raw_with_mbr(FixtureFs::Ext4, &[("/a", b"b")]));
    common::mark_unclean_shutdown(&mut img);

    // 即使调用方**想要写**，也一律只读 + 理由 Dirty
    let vol = VmdkVolume::new(mem(img), writable_opts()).unwrap();
    let caps = vol.caps();
    assert!(!caps.writable, "未干净关闭的镜像不得广告可写");
    assert_eq!(caps.read_only_reason, Some(ReadOnlyReason::Dirty));

    // 读照常：分区行与树都在
    let reader = VolumeReader::build(
        Box::new(vol),
        &EntryLimit::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(names.contains(&"Partition 1/a".to_string()), "{names:?}");
}

#[test]
fn create_writer_is_refused_by_name() {
    let handler = VmdkHandler::new(registry());
    let err = match handler.create_writer(
        hzip_format::Sink::File(std::path::PathBuf::from("x.vmdk")),
        &Default::default(),
    ) {
        Ok(_) => panic!("vmdk 不产出镜像，应拒绝"),
        Err(e) => e,
    };
    assert!(
        matches!(err, hzip_error::HzipError::Unsupported { ref what }
                 if what == "vol.unsupported.create-image"),
        "{err:?}"
    );
}
