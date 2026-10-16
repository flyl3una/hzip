//! 整盘镜像夹具：把「文件系统镜像」与「分区表」拼成一块可挂载/可探测的盘。
//!
//! 三种形态，对应三层被测代码：
//!
//! | 夹具 | 形态 | 主要服务 |
//! |---|---|---|
//! | [`raw_bare`] | 整块盘就是一个文件系统 | 裸卷识别（无分区表分支） |
//! | [`raw_with_mbr`] | MBR + 一个分区 | 分区层 + 文件系统（最常见形态） |
//! | [`raw_with_gpt`] | GPT + 一个分区 | 分区层（GPT 头 CRC / 备份头路径） |
//! | [`write_fs_into`] | 把文件系统写进既有盘的一段 | 组合场景（多分区、偏移对齐） |
//!
//! 分区起始一律 **1 MiB 对齐**——不是审美，是通行做法（GPT 的 `first_usable` 是
//! LBA 34 ≈ 17 KiB，但真实工具与镜像都按 1 MiB 对齐，夹具跟着真实走）。

use super::{ext4, fat, gpt, mbr};

/// 分区起始偏移（1 MiB 对齐）。
pub const PARTITION_START_BYTES: u64 = 1 << 20;

/// 夹具支持的文件系统种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureFs {
    /// ext4（干净卷）。
    Ext4,
    /// ext4（`s_state` 的 VALID_FS 位已清 ⇒ 脏卷）。
    Ext4Dirty,
    /// FAT32。
    Fat32,
}

impl FixtureFs {
    /// 造该文件系统的**裸镜像字节**（不含分区表）。
    #[must_use]
    pub fn image(self, files: &[(&str, &[u8])]) -> Vec<u8> {
        match self {
            Self::Ext4 => ext4::ext4_image(files),
            Self::Ext4Dirty => ext4::ext4_image_dirty(files),
            Self::Fat32 => fat::fat32_image(files),
        }
    }

    /// MBR 分区类型字节（`0x83` = Linux，`0x0C` = FAT32 LBA）。
    #[must_use]
    pub fn mbr_type(self) -> u8 {
        match self {
            Self::Ext4 | Self::Ext4Dirty => 0x83,
            Self::Fat32 => 0x0C,
        }
    }

    /// GPT 分区类型。
    #[must_use]
    pub fn gpt_type(self) -> gpt::PartitionType {
        match self {
            Self::Ext4 | Self::Ext4Dirty => gpt::PartitionType::LinuxFilesystem,
            Self::Fat32 => gpt::PartitionType::EfiSystem,
        }
    }
}

/// 把 `fs` 的文件系统镜像写进 `disk` 的 `offset` 处，返回写入字节数。
///
/// 装不下**直接 panic**：夹具不提供「截断到装得下」的宽容语义——那会让调用方
/// 拿到一个「看起来能挂载」的残镜像，把真实的越界缺陷掩盖掉。
pub fn write_fs_into(disk: &mut [u8], offset: u64, fs: FixtureFs, files: &[(&str, &[u8])]) -> u64 {
    let image = fs.image(files);
    let start = usize::try_from(offset).expect("offset 超出 usize");
    let end = start + image.len();
    assert!(
        end <= disk.len(),
        "夹具：盘装不下文件系统镜像（需 {end} B，盘 {} B）",
        disk.len()
    );
    disk[start..end].copy_from_slice(&image);
    image.len() as u64
}

/// 裸镜像：整块盘就是一个文件系统（无分区表）。
#[must_use]
pub fn raw_bare(fs: FixtureFs, files: &[(&str, &[u8])]) -> Vec<u8> {
    fs.image(files)
}

/// MBR 盘：1 MiB 对齐的单分区，分区内是 `fs`。
#[must_use]
pub fn raw_with_mbr(fs: FixtureFs, files: &[(&str, &[u8])]) -> Vec<u8> {
    let image = fs.image(files);
    let total = PARTITION_START_BYTES + image.len() as u64;
    let sectors = (image.len() as u64).div_ceil(u64::from(fat::FAT_SECTOR_BYTES));
    let start_lba = u32::try_from(PARTITION_START_BYTES / u64::from(fat::FAT_SECTOR_BYTES))
        .expect("夹具：起始 LBA 应落在 u32");
    let mut disk = mbr::mbr_only(&[(fs.mbr_type(), start_lba, sectors as u32)], total);
    write_fs_into(&mut disk, PARTITION_START_BYTES, fs, files);
    disk
}

/// GPT 盘：1 MiB 对齐的单分区，分区内是 `fs`。
#[must_use]
pub fn raw_with_gpt(fs: FixtureFs, files: &[(&str, &[u8])]) -> Vec<u8> {
    let image = fs.image(files);
    // 盘尾留出备份条目 + 备份头（33 扇区）+ 1 MiB 余量，保证 last_usable 覆盖分区
    let total = PARTITION_START_BYTES + image.len() as u64 + PARTITION_START_BYTES;
    let parts = [gpt::GptPartition {
        ty: fs.gpt_type(),
        start_lba: PARTITION_START_BYTES / gpt::GPT_SECTOR,
        len_lba: (image.len() as u64).div_ceil(gpt::GPT_SECTOR),
        name: "hzip-fixture",
    }];
    let mut disk = gpt::gpt_only(&parts, total);
    write_fs_into(&mut disk, PARTITION_START_BYTES, fs, files);
    disk
}
