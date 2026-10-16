//! 卷镜像夹具**自证**：此处失败 ⇒ 后面所有卷相关测试的结论都不可信。
//!
//! 每条断言都在回答「这个夹具造出来的东西，真的是它声称的那种格式吗」：
//!
//! - `MemDevice` 的越界/短读语义（契约里写死的那几条）；
//! - MBR / GPT 的字段与 **CRC 自校验**（不是抄来的常量）；
//! - ext4 / FAT32 用**上游自己的驱动**读回夹具写进去的文件（最强的有效性证明）；
//! - `raw_with_mbr` 的分区项与实际写入位置一致。
//!
//! 确定性另有一段：同一组入参两次调用逐字节相同（若某格式不满足，断言会指出，
//! 我们据此修正文档而不是放宽断言）。

#![cfg(feature = "vol")]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::Arc;

use fs_core::{BlockDevice as _, BlockRead as _};
use hzip_format_testkit::vol::{
    FixtureFs, GptPartition, MemDevice, PartitionType, ext4_image, ext4_image_dirty, fat32_image,
    gpt_only, mbr, mbr_only, raw_bare, raw_with_mbr, write_fs_into,
};

const SECTOR: usize = 512;

// ---------------------------------------------------------------- MemDevice

#[test]
fn mem_device_round_trips_and_bounds_checks() {
    let dev = MemDevice::new(4096);
    dev.write_at(100, b"hello").unwrap();
    let mut buf = [0u8; 5];
    dev.read_at(100, &mut buf).unwrap();
    assert_eq!(&buf, b"hello");
    assert_eq!(dev.to_bytes()[100..105], *b"hello");
    assert_eq!(dev.size_bytes(), 4096);
    assert!(dev.is_writable());

    // 越界：读与写都必须 Err（不 panic、不静默截断）
    assert!(dev.read_at(4090, &mut [0u8; 16]).is_err(), "跨界读应报错");
    assert!(dev.write_at(4096, b"x").is_err(), "越界写应报错");
    // 空区间贴尾合法
    assert!(dev.read_at(4096, &mut []).is_ok(), "零长度读在 len 处合法");
    // 容量不可变
    assert!(dev.set_len(8192).is_err(), "卷容量构造时固定");
    assert!(!dev.can_grow());
    // 共享底层：clone 是同一块盘的第二个句柄
    let view = dev.clone();
    dev.write_at(0, b"Z").unwrap();
    assert_eq!(view.to_bytes()[0], b'Z');
}

// ------------------------------------------------------------------- MBR/GPT

#[test]
fn mbr_fields_and_signature() {
    let m = mbr(&[(0x83, 2048, 4096), (0x0C, 6144, 8192)]);
    assert_eq!(&m[510..512], &[0x55, 0xAA], "引导签名");
    assert_eq!(m[446 + 4], 0x83);
    assert_eq!(
        u32::from_le_bytes(m[446 + 8..446 + 12].try_into().unwrap()),
        2048
    );
    assert_eq!(
        u32::from_le_bytes(m[446 + 12..446 + 16].try_into().unwrap()),
        4096
    );
    assert_eq!(m[446 + 16 + 4], 0x0C, "第二项类型");
    assert_eq!(
        u32::from_le_bytes(m[446 + 16 + 8..446 + 16 + 12].try_into().unwrap()),
        6144
    );
}

#[test]
#[should_panic(expected = "只有四个主分区项")]
fn mbr_refuses_more_than_four_entries() {
    let _ = mbr(&[(0x83, 1, 1); 5]);
}

#[test]
fn gpt_headers_validate_their_own_crc() {
    let parts = [GptPartition {
        ty: PartitionType::LinuxFilesystem,
        start_lba: 2048,
        len_lba: 4096,
        name: "root",
    }];
    let total = 4 << 20;
    let disk = gpt_only(&parts, total);
    assert_eq!(disk.len() as u64, total);

    // 主头
    assert_eq!(&disk[512..520], b"EFI PART");
    let primary_crc = u32::from_le_bytes(disk[512 + 16..512 + 20].try_into().unwrap());
    let mut header = disk[512..512 + 92].to_vec();
    header[16..20].copy_from_slice(&[0, 0, 0, 0]);
    assert_eq!(
        hzip_format_testkit::vol::gpt::crc32(&header),
        primary_crc,
        "主头 CRC 必须自校验通过"
    );

    // 备份头在盘尾，且它的 current/backup LBA 与主头互换
    let last = total as usize - SECTOR;
    assert_eq!(&disk[last..last + 8], b"EFI PART", "备份头在盘尾");
    let backup_crc = u32::from_le_bytes(disk[last + 16..last + 20].try_into().unwrap());
    let mut bh = disk[last..last + 92].to_vec();
    bh[16..20].copy_from_slice(&[0, 0, 0, 0]);
    assert_eq!(hzip_format_testkit::vol::gpt::crc32(&bh), backup_crc);
    let primary_current = u64::from_le_bytes(disk[512 + 24..512 + 32].try_into().unwrap());
    let primary_backup = u64::from_le_bytes(disk[512 + 32..512 + 40].try_into().unwrap());
    let backup_current = u64::from_le_bytes(disk[last + 24..last + 32].try_into().unwrap());
    assert_eq!((primary_current, primary_backup), (1, backup_current));

    // 条目数组：条目的 LBA 与给定一致，且两处 CRC 相符
    let entries_crc = u32::from_le_bytes(disk[512 + 88..512 + 92].try_into().unwrap());
    let entries = &disk[2 * SECTOR..2 * SECTOR + 128 * 128];
    assert_eq!(hzip_format_testkit::vol::gpt::crc32(entries), entries_crc);
    assert_eq!(
        u64::from_le_bytes(entries[32..40].try_into().unwrap()),
        2048
    );
    assert_eq!(
        u64::from_le_bytes(entries[40..48].try_into().unwrap()),
        2048 + 4096 - 1
    );
    // 保护性 MBR
    assert_eq!(&disk[510..512], &[0x55, 0xAA]);
    assert_eq!(disk[446 + 4], 0xEE, "保护性 MBR 类型应为 0xEE");
}

// ------------------------------------------------------- 文件系统夹具有效性

#[test]
fn ext4_image_is_readable_by_the_upstream_driver() {
    let bytes = ext4_image(&[
        ("/etc/fstab", b"LABEL=root / ext4 defaults 0 1\n"),
        ("/empty", b""),
    ]);
    assert_eq!(
        bytes.len() as u64,
        hzip_format_testkit::vol::ext4::EXT4_IMAGE_BYTES
    );
    // 超级块布局：起点在 1024（**固定**，与块大小无关），`s_magic` 在其偏移 56
    // ⇒ 绝对偏移 **1080**，小端 0xEF53 = 字节 `53 EF`。
    // （1024 处是 `s_inodes_count`，不是魔数——别按“53 EF@1024”去认 ext4。）
    assert_eq!(
        &bytes[1080..1082],
        &[0x53, 0xEF],
        "ext4 s_magic 应在 1024+56"
    );
    assert_ne!(
        u32::from_le_bytes(bytes[1024..1028].try_into().unwrap()),
        0,
        "s_inodes_count 不应为 0（非空文件系统的 sanity）"
    );

    // 用上游驱动读回：夹具「真的」是能挂载的 ext4
    let dev = Arc::new(fs_ext4::fs_core_bridge::CoreDevice::new(
        MemDevice::from_bytes(bytes),
    ));
    let fs = fs_ext4::Filesystem::mount(dev).expect("夹具镜像应可挂载");
    let ino = fs.lookup_path_bytes(b"/etc/fstab").expect("应能解析路径");
    let mut buf = vec![0u8; 64];
    let n = fs.read_ino(ino, 0, &mut buf).expect("应能读文件");
    assert_eq!(&buf[..n], b"LABEL=root / ext4 defaults 0 1\n");
    assert_eq!(fs.stat_ino(ino).expect("应能 stat").size, 31);
    let empty = fs.lookup_path_bytes(b"/empty").expect("空文件应存在");
    assert_eq!(fs.stat_ino(empty).expect("应能 stat").size, 0);
}

#[test]
fn ext4_dirty_variant_clears_the_valid_fs_bit_and_still_mounts() {
    let clean = ext4_image(&[("/a", b"x")]);
    let dirty = ext4_image_dirty(&[("/a", b"x")]);
    // 干净卷：s_state 的 VALID_FS 位置位（0x0001）
    let clean_state = u16::from_le_bytes(clean[1082..1084].try_into().unwrap());
    assert_ne!(clean_state & 0x0001, 0, "干净卷应置 VALID_FS");
    let dirty_state = u16::from_le_bytes(dirty[1082..1084].try_into().unwrap());
    assert_eq!(dirty_state & 0x0001, 0, "脏卷应清 VALID_FS（需核查）");
    // 两份只差超级块那一小段（s_state 与其校验和）
    let diffs: Vec<usize> = clean
        .iter()
        .zip(dirty.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i)
        .collect();
    assert!(
        diffs
            .iter()
            .all(|i| (1082..1084).contains(i) || (2044..2048).contains(i)),
        "脏卷夹具只应改 s_state 与其校验和（差异字节：{diffs:?}）"
    );

    // **真挂载**：这是本夹具可用性的硬证明（2026-10-16 补：原先只比字节，
    // 漏掉了「改了超级块却没重算校验和 ⇒ 上游 BadChecksum 挂不上」这一缺陷）。
    let dev = Arc::new(fs_ext4::fs_core_bridge::CoreDevice::new(
        MemDevice::from_bytes(dirty.clone()),
    ));
    let fs = fs_ext4::Filesystem::mount_lazy(dev).expect("脏卷夹具必须仍可挂载（校验和要重算）");
    let ino = fs.lookup_path_bytes(b"/a").expect("脏卷里的文件仍应可读");
    let mut buf = [0u8; 1];
    assert_eq!(fs.read_ino(ino, 0, &mut buf).unwrap(), 1);
    assert_eq!(buf[0], b'x');
}

#[test]
fn fat32_image_is_readable_by_the_upstream_driver() {
    let bytes = fat32_image(&[("/HELLO.TXT", b"hi-fat"), ("/EFI/BOOT/BOOTX64.EFI", b"EFI")]);
    assert_eq!(
        bytes.len() as u64,
        hzip_format_testkit::vol::fat::FAT32_IMAGE_BYTES
    );
    let mut cur = std::io::Cursor::new(bytes);
    let fs = fatfs::FileSystem::new(&mut cur, fatfs::FsOptions::new()).expect("夹具应可挂载");
    assert_eq!(fs.fat_type(), fatfs::FatType::Fat32);
    let root = fs.root_dir();
    let f = root.open_file("HELLO.TXT").expect("根下应能打开 HELLO.TXT");
    let mut s = String::new();
    std::io::Read::read_to_string(&mut { f }, &mut s).unwrap();
    assert_eq!(s, "hi-fat");
    let nested = root
        .open_dir("EFI")
        .expect("应能进 EFI")
        .open_dir("BOOT")
        .expect("应能进 BOOT")
        .open_file("BOOTX64.EFI")
        .expect("应能打开 ESP 里的启动文件");
    let mut s2 = String::new();
    std::io::Read::read_to_string(&mut { nested }, &mut s2).unwrap();
    assert_eq!(s2, "EFI");
}

// ----------------------------------------------------------------- 整盘形态

#[test]
fn raw_with_mbr_places_the_partition_and_the_image_where_it_claims() {
    let disk = raw_with_mbr(FixtureFs::Ext4, &[("/etc/fstab", b"x")]);
    assert_eq!(&disk[510..512], &[0x55, 0xAA]);
    let start_lba = u32::from_le_bytes(disk[446 + 8..446 + 12].try_into().unwrap());
    let sectors = u32::from_le_bytes(disk[446 + 12..446 + 16].try_into().unwrap());
    assert_eq!(u64::from(start_lba) * 512, 1 << 20, "分区自 1 MiB 起");
    assert_eq!(disk[446 + 4], 0x83, "ext4 ⇒ 类型 0x83");

    let off = (u64::from(start_lba) * 512) as usize;
    assert_eq!(
        &disk[off..off + 2],
        &[0x00, 0x00][..],
        "分区前 2 B 属于引导区"
    );
    assert_eq!(
        &disk[off + 1080..off + 1082],
        &[0x53, 0xEF],
        "分区内应有 ext4 超级块（s_magic @ 分区起点 + 1024 + 56）"
    );
    // 盘长 = 分区起点 + 镜像长（不多不少）
    assert_eq!(disk.len() as u64, (1 << 20) + u64::from(sectors) * 512);
}

#[test]
fn raw_bare_is_just_the_filesystem() {
    let bare = raw_bare(FixtureFs::Fat32, &[("A.TXT", b"a")]);
    assert_eq!(&bare[510..512], &[0x55, 0xAA], "FAT 引导扇区也有 55 AA");
    assert_eq!(
        bare.len() as u64,
        hzip_format_testkit::vol::fat::FAT32_IMAGE_BYTES
    );
}

#[test]
fn write_fs_into_refuses_when_the_disk_is_too_small() {
    let mut disk = vec![0u8; 4096];
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        write_fs_into(&mut disk, 0, FixtureFs::Ext4, &[]);
    }));
    assert!(r.is_err(), "装不下必须 panic，不得静默截断");
}

// --------------------------------------------------------------- 确定性

#[test]
fn fixtures_are_deterministic_across_calls() {
    let files: &[(&str, &[u8])] = &[("/etc/fstab", b"deterministic")];
    assert_eq!(
        ext4_image(files),
        ext4_image(files),
        "ext4 夹具应逐字节可复现"
    );
    assert_eq!(
        ext4_image_dirty(files),
        ext4_image_dirty(files),
        "脏卷夹具应逐字节可复现"
    );
    assert_eq!(
        mbr_only(&[(0x83, 2048, 4096)], 1 << 20),
        mbr_only(&[(0x83, 2048, 4096)], 1 << 20)
    );
    let parts = [GptPartition {
        ty: PartitionType::EfiSystem,
        start_lba: 2048,
        len_lba: 1024,
        name: "esp",
    }];
    assert_eq!(gpt_only(&parts, 2 << 20), gpt_only(&parts, 2 << 20));
    assert_eq!(
        fat32_image(&[("A.TXT", b"x")]),
        fat32_image(&[("A.TXT", b"x")]),
        "FAT32 夹具应逐字节可复现（时间戳经 FixedTime 注入）"
    );
}
