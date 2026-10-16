//! 分区层契约测试：有表/无表/坏表三态 + 子设备切片语义。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

use std::sync::Arc;

use fs_core::{BlockDevice as _, BlockRead as _};
use hzip_format_testkit::vol::{
    FixtureFs, GptPartition, MemDevice, PartitionType, gpt_only, raw_bare, raw_with_gpt,
    raw_with_mbr,
};
use hzip_vol::partition::{open_region, probe_partitions, region, volume_views, whole};
use hzip_vol::{FsKind, TableKind};

#[test]
fn mbr_disk_yields_one_partition_with_fs_and_offset() {
    let mem = Arc::new(MemDevice::from_bytes(raw_with_mbr(
        FixtureFs::Ext4,
        &[("/etc/fstab", b"x")],
    )));
    let disk: Arc<dyn fs_core::BlockDevice> = mem.clone();
    let probe = probe_partitions(&disk).unwrap();
    assert_eq!(probe.table, Some(TableKind::Mbr));
    assert_eq!(probe.len(), 1);
    let p = &probe.partitions[0];
    assert_eq!(p.id, 0);
    assert_eq!(p.start, 1 << 20, "分区自 1 MiB 起（与夹具一致）");
    assert_eq!(p.type_byte, Some(0x83), "ext4 ⇒ 类型 0x83");
    assert_eq!(p.fs, Some(FsKind::Ext4), "应嗅探到 ext4");
    assert!(
        p.display().starts_with("1. "),
        "显示名带序号：{}",
        p.display()
    );
    assert!(
        p.display().contains("(ext4)"),
        "显示名带文件系统：{}",
        p.display()
    );

    // 子设备切片：偏移重定位 + 可写（写落到父设备对应位置）
    let sub = open_region(&disk, &probe, Some(0)).unwrap();
    assert_eq!(sub.size_bytes(), p.len);
    sub.write_at(0, b"SLICE").unwrap();
    let base = mem.to_bytes();
    assert_eq!(
        &base[(1 << 20)..(1 << 20) + 5],
        b"SLICE",
        "写应落在分区起点处"
    );
}

#[test]
fn fat32_partition_reports_fat_and_type_byte_lba() {
    let disk: Arc<dyn fs_core::BlockDevice> = Arc::new(MemDevice::from_bytes(raw_with_mbr(
        FixtureFs::Fat32,
        &[("A.TXT", b"a")],
    )));
    let probe = probe_partitions(&disk).unwrap();
    let p = &probe.partitions[0];
    assert_eq!(p.type_byte, Some(0x0C), "FAT32 LBA ⇒ 0x0C");
    assert_eq!(p.fs, Some(FsKind::Fat));
}

#[test]
fn gpt_disk_uses_the_partition_name_and_sniffs_the_fs() {
    let disk: Arc<dyn fs_core::BlockDevice> = Arc::new(MemDevice::from_bytes(raw_with_gpt(
        FixtureFs::Ext4,
        &[("/a", b"b")],
    )));
    let probe = probe_partitions(&disk).unwrap();
    assert_eq!(probe.table, Some(TableKind::Gpt));
    assert_eq!(probe.len(), 1);
    let p = &probe.partitions[0];
    assert_eq!(p.type_byte, None, "GPT 分区没有 MBR 类型字节");
    assert_eq!(
        p.label.as_deref(),
        Some("hzip-fixture"),
        "GPT 分区名就是真实标签（界面据此作分区名）"
    );
    assert_eq!(p.fs, Some(FsKind::Ext4));
}

#[test]
fn gpt_without_name_falls_back_to_the_type_guid() {
    let parts = [GptPartition {
        ty: PartitionType::EfiSystem,
        start_lba: 2048,
        len_lba: 1024,
        name: "",
    }];
    let disk: Arc<dyn fs_core::BlockDevice> =
        Arc::new(MemDevice::from_bytes(gpt_only(&parts, 8 << 20)));
    let probe = probe_partitions(&disk).unwrap();
    // 名字为空 ⇒ **没有标签**（label=None），类型描述进 `type_name`（界面类型列用）
    assert_eq!(probe.partitions[0].label, None, "无名 ⇒ 无标签");
    assert_eq!(
        probe.partitions[0].type_name, "EFI System",
        "类型描述取自类型 GUID"
    );
    assert_eq!(probe.partitions[0].fs, None, "分区内是零 ⇒ 未识别");
}

#[test]
fn bare_filesystem_has_no_table_and_gets_a_whole_view() {
    let bare = raw_bare(FixtureFs::Ext4, &[("/a", b"b")]);
    let disk: Arc<dyn fs_core::BlockDevice> = Arc::new(MemDevice::from_bytes(bare.clone()));
    let probe = probe_partitions(&disk).unwrap();
    assert!(!probe.has_table(), "裸文件系统镜像不应被认成有分区表");
    assert!(probe.is_empty());
    let views = volume_views(&probe, disk.size_bytes());
    assert_eq!(views.len(), 1, "无表 ⇒ 一个整卷视图");
    assert_eq!(views[0].len, bare.len() as u64);
    assert!(open_region(&disk, &probe, None).is_ok(), "整卷设备应可开");
}

#[test]
fn zero_disk_and_bad_table_do_not_fail_the_open() {
    let zero: Arc<dyn fs_core::BlockDevice> = Arc::new(MemDevice::new(1 << 20));
    let probe = probe_partitions(&zero).unwrap();
    assert!(!probe.has_table() && probe.is_empty(), "全零盘 ⇒ 无表");

    // 坏 GPT（主头 CRC 被改坏）：按无表处理 + 提示，仍不报错
    let parts = [GptPartition {
        ty: PartitionType::LinuxFilesystem,
        start_lba: 2048,
        len_lba: 1024,
        name: "root",
    }];
    let mut bytes = gpt_only(&parts, 8 << 20);
    bytes[512 + 16] ^= 0xFF; // 破坏主头 CRC
    let disk: Arc<dyn fs_core::BlockDevice> = Arc::new(MemDevice::from_bytes(bytes));
    let probe = probe_partitions(&disk).unwrap();
    assert!(
        probe.warnings.contains(&"vol.warn.table-corrupt") || !probe.has_table(),
        "坏表应降级为提示或无表，而不是失败：{probe:?}"
    );
}

#[test]
fn region_and_whole_slice_sizes() {
    let disk: Arc<dyn fs_core::BlockDevice> = Arc::new(MemDevice::new(1 << 20));
    assert_eq!(whole(&disk).size_bytes(), 1 << 20);
    let part = region(&disk, 4096, 8192);
    assert_eq!(part.size_bytes(), 8192);
    // 切片越界写由切片自己报错（不落到父设备）
    assert!(part.write_at(8190, &[0; 8]).is_err());
}
