//! FAT 驱动入口：`hzip_vol::FsDriver` 实现（嗅探复核 + 挂载）。
//!
//! # 挂载前**复核**头部，不只看嗅探结论
//!
//! 分区层嗅探读的是「分区起始的一小段窗口」，结论来自引导扇区里的类型标签
//! （`FAT12   `/`FAT16   ` 在 0x36，`FAT32   ` 在 0x52）。本驱动**另起一份证据**：
//! 按 BPB 的几何字段算出簇数，再与标签互相印证（见 [`is_fat_boot_sector`]）。
//! 两份证据都点头才挂载——于是「标签写着 FAT32、簇数却只够 FAT16」这种位翻转
//! 会被挡在挂载之前，而不是进去以后解析出乱七八糟的东西。
//!
//! # 绝不用 FAT 驱动读 exFAT
//!
//! exFAT 与 FAT 共享 0x55AA 引导扇区与偏移 3 的 OEM 名布局，但它是**另一个**文件
//! 系统（`FsKind::ExFat`，本期无驱动 ⇒ 显示「未知卷」）。故 [`is_fat_boot_sector`]
//! 显式拒绝 `EXFAT   `（以及同布局的 `NTFS    `）。

use std::sync::Arc;

use hzip_vol::{BlockDevice, FsDriver, FsKind, FsOptions, FsVolume, VolResult};

use crate::volume::FatVolume;

/// FAT12/16/32 驱动（注册进 `hzip_vol::FsRegistry`；见 `hzip-core` 的启动注册）。
#[derive(Debug, Default)]
pub struct FatDriver;

impl FatDriver {
    /// 新驱动实例（无状态：每次挂载各自建 `FatVolume`）。
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl FsDriver for FatDriver {
    fn id(&self) -> &'static str {
        "fat"
    }

    fn supports(&self, kind: FsKind, head: &[u8]) -> bool {
        kind == FsKind::Fat && is_fat_boot_sector(head)
    }

    fn mount(&self, dev: Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<Box<dyn FsVolume>> {
        Ok(Box::new(FatVolume::mount(dev, opts)?))
    }
}

/// 引导扇区复核：`head` 是不是一个**自洽**的 FAT 引导扇区。
///
/// 与上游 `fatfs::FileSystem::new` 的口径对齐（它也做 `BootSector::validate`），
/// 但这里额外要求「标签 ↔ 簇数」互相印证，并显式排除同布局的 exFAT/NTFS。
///
/// `head` 短于 512 字节、字段自相矛盾、算术溢出（`total - reserved - FAT - root`）
/// 一律返回 `false`——**不 panic、不夹取**。
#[must_use]
pub fn is_fat_boot_sector(head: &[u8]) -> bool {
    let Some(sector) = head.get(..512) else {
        return false; // 少于一个逻辑扇区：不可能是 FAT 引导扇区
    };
    if sector[510..512] != [0x55, 0xAA] {
        return false;
    }
    // 同布局的近亲：exFAT / NTFS 的 OEM 名占同一个位置，绝不认成 FAT。
    match &sector[3..11] {
        b"EXFAT   " | b"NTFS    " => return false,
        _ => {}
    }

    // ① BPB 几何：与上游 `BiosParameterBlock::validate` 同口径。
    let bytes_per_sector = u32::from(u16::from_le_bytes([sector[11], sector[12]]));
    if !bytes_per_sector.is_power_of_two() || !(512..=4096).contains(&bytes_per_sector) {
        return false;
    }
    let sectors_per_cluster = u32::from(sector[13]);
    if !sectors_per_cluster.is_power_of_two() || sectors_per_cluster > 128 {
        return false;
    }
    let reserved = u32::from(u16::from_le_bytes([sector[14], sector[15]]));
    let fats = u32::from(sector[16]);
    if reserved == 0 || fats == 0 {
        return false;
    }
    let root_entries = u32::from(u16::from_le_bytes([sector[17], sector[18]]));
    let total16 = u32::from(u16::from_le_bytes([sector[19], sector[20]]));
    let total32 = u32::from_le_bytes([sector[32], sector[33], sector[34], sector[35]]);
    let total = if total16 != 0 { total16 } else { total32 };
    let sectors_per_fat = match u16::from_le_bytes([sector[22], sector[23]]) {
        0 => u32::from_le_bytes([sector[36], sector[37], sector[38], sector[39]]),
        n => u32::from(n),
    };
    if total == 0 || sectors_per_fat == 0 {
        return false;
    }

    // ② 由几何字段**独立算出**簇数（上游也这样做，且它只按簇数定 FAT 类型）。
    let root_dir_sectors = (root_entries * 32).div_ceil(bytes_per_sector);
    let Some(table_sectors) = fats.checked_mul(sectors_per_fat) else {
        return false;
    };
    let Some(fixed) = reserved
        .checked_add(table_sectors)
        .and_then(|x| x.checked_add(root_dir_sectors))
    else {
        return false;
    };
    let Some(data_sectors) = total.checked_sub(fixed) else {
        return false; // 表区/根目录比整个卷还大 ⇒ 坏 BPB
    };
    let clusters = data_sectors / sectors_per_cluster;
    if clusters == 0 {
        return false;
    }
    let computed_fat32 = clusters >= 65_525;

    // ③ 两份证据互相印证：标签（嗅探用的那份）必须被簇数支持。
    let tag32 = sector.get(0x52..0x5A);
    let tag16 = sector.get(0x36..0x3E);
    if tag32 == Some(&b"FAT32   "[..]) {
        return computed_fat32;
    }
    if tag16 == Some(&b"FAT12   "[..]) || tag16 == Some(&b"FAT16   "[..]) {
        // FAT12/FAT16 的标签与簇数分界并不总是重合（2.88 MB 软盘标 FAT12 而簇数已过
        // 4085）⇒ 只要求「不带 FAT32 的簇数」。
        return !computed_fat32;
    }
    // 没有可用的类型标签：嗅探也不会给出 `FsKind::Fat`，这里就不认。
    false
}

#[cfg(test)]
mod tests {
    use super::is_fat_boot_sector;

    #[test]
    fn short_or_unlabeled_head_is_not_fat() {
        assert!(!is_fat_boot_sector(&[]));
        assert!(!is_fat_boot_sector(&[0u8; 511]));
        let mut head = vec![0u8; 512];
        head[510] = 0x55;
        head[511] = 0xAA;
        assert!(
            !is_fat_boot_sector(&head),
            "无 BPB 字段的裸 0x55AA 不算 FAT"
        );
    }

    #[test]
    fn exfat_and_ntfs_are_refused() {
        let mut head = vec![0u8; 512];
        head[3..11].copy_from_slice(b"EXFAT   ");
        head[510] = 0x55;
        head[511] = 0xAA;
        assert!(!is_fat_boot_sector(&head));
        head[3..11].copy_from_slice(b"NTFS    ");
        assert!(!is_fat_boot_sector(&head));
    }
}
