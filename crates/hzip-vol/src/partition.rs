//! 分区层：块设备 → 分区表（GPT/MBR）→ **每卷一个可写子设备**。
//!
//! # 这一层只做两件事
//!
//! 1. 探测分区表与每卷的文件系统签名（`disk_partitions::probe` + `sniff`）；
//! 2. 把每个分区切成一个独立的块设备（`OwnedRwSlice`），交给文件系统驱动去挂载。
//!
//! 「分区内是什么文件系统」在这里只**填给人看**（[`PartitionView::fs`]，显示为
//! `1. EFI System (FAT)`）；真正挂载时由 `crate::registry` 按同一个结论挑驱动，
//! 并在挂载前用头部字节**复核一次**（签名嗅探读的是分区起始窗口，驱动复核能兜住
//! 「嗅探窗口与真实超级块偏移不同」这类偏差）。
//!
//! # 无分区表的三种情况都如实呈现
//!
//! | 情况 | `table` | `partitions` | 上层行为 |
//! |---|---|---|---|
//! | 真无签名（裸文件系统镜像、全零盘） | `None` | 空 | 把整卷当一个文件系统卷（`open_fs(None)`） |
//! | MBR 表项全零（`probe` 认为无表） | `None` | 空 | 同上 |
//! | 表存在但某项越界 | 有 | 该项**仍在**（`disk_partitions` 保证 `probe` 是全函数） | 只在读写该项时报越界错误 |

use std::sync::Arc;

use fs_core::BlockRead as _;

use crate::error::{VolError, VolResult};
use crate::{BlockDevice, FsKind, PartitionView, TableKind};

/// 一次分区探测的结果。
#[derive(Debug, Clone, Default)]
pub struct PartitionProbe {
    /// 分区表种类；`None` = 无表。
    pub table: Option<TableKind>,
    /// 分区视图（按磁盘槽位序）。
    pub partitions: Vec<PartitionView>,
    /// 提示（例：`vol.warn.table-corrupt`，表损坏但已尽力解析）。
    pub warnings: Vec<&'static str>,
}

/// 探测分区表与每卷的文件系统。
///
/// 表**解析失败**不算打开失败：`disk_partitions::probe` 对损坏表也尽力给出结果，
/// 我们只在完全无签名时把 `table` 置 `None`（那才是「无分区表」的语义）；
/// 其它错误（CRC 不符、结构自相矛盾）降级为提示 + 空表，让上层仍能按「整卷即文件系统」
/// 去看——**比直接打不开更有用**（用户的镜像是坏盘时，能看到内容比看到一个错误更有价值）。
pub fn probe_partitions(dev: &Arc<dyn BlockDevice>) -> VolResult<PartitionProbe> {
    let (table, parts) = match disk_partitions::probe(&**dev) {
        Ok(v) => v,
        Err(disk_partitions::Error::NoPartitionTable) => {
            return Ok(PartitionProbe::default());
        }
        Err(e) => {
            // 有签名但读不动：如实提示，按「无表」继续（上层可整卷尝试挂载）。
            tracing::warn!("分区表解析失败，按无表处理：{e}");
            return Ok(PartitionProbe {
                table: None,
                partitions: Vec::new(),
                warnings: vec!["vol.warn.table-corrupt"],
            });
        }
    };
    let our_table = map_table(table);
    let mut views = Vec::with_capacity(parts.len());
    for (i, p) in parts.iter().enumerate() {
        // 越界项不丢（`disk_partitions` 刻意保留）——只在读它时由切片报越界。
        let fs = match disk_partitions::sniff(&**dev, p) {
            Ok(k) if !matches!(k, disk_partitions::FsKind::Unknown) => map_fs(&k),
            Ok(_) => None,
            Err(e) => {
                tracing::debug!("分区 {i} 文件系统嗅探失败：{e}");
                None
            }
        };
        views.push(PartitionView {
            id: i,
            start: p.start,
            len: p.length,
            type_byte: match p.kind {
                disk_partitions::PartitionKind::Mbr { type_byte, .. } => Some(type_byte),
                _ => None,
            },
            label: label_of(p),
            type_name: type_name_of(p, our_table),
            fs,
        });
    }
    Ok(PartitionProbe {
        table: Some(our_table),
        partitions: views,
        warnings: Vec::new(),
    })
}

/// 某个分区的子设备（**可写**切片；写会落到父设备的对应偏移上）。
#[must_use]
pub fn region(dev: &Arc<dyn BlockDevice>, start: u64, len: u64) -> Arc<dyn BlockDevice> {
    Arc::new(fs_core::OwnedRwSlice::new(dev.clone(), start, len))
}

/// 整卷子设备（无分区表时用；等价于 `region(dev, 0, size)`）。
#[must_use]
pub fn whole(dev: &Arc<dyn BlockDevice>) -> Arc<dyn BlockDevice> {
    region(dev, 0, dev.size_bytes())
}

fn map_table(kind: disk_partitions::TableKind) -> TableKind {
    match kind {
        disk_partitions::TableKind::Gpt => TableKind::Gpt,
        disk_partitions::TableKind::Mbr => TableKind::Mbr,
    }
}

fn map_fs(kind: &disk_partitions::FsKind) -> Option<FsKind> {
    use disk_partitions::FsKind as K;
    Some(match kind {
        K::Ext { .. } => FsKind::Ext4,
        K::Ntfs => FsKind::Ntfs,
        K::ExFat => FsKind::ExFat,
        K::Fat32 | K::Fat16 => FsKind::Fat,
        K::Xfs => FsKind::Xfs,
        K::Unknown => return None,
        _ => FsKind::Other,
    })
}

/// **真实分区标签**（界面用它当分区名）：GPT 的分区名（非空、清理过）；
/// MBR 没有标签概念 ⇒ `None`（由调用方回退成「分区 N"，再按语言显示）。
fn label_of(p: &disk_partitions::Partition) -> Option<String> {
    match &p.kind {
        disk_partitions::PartitionKind::Gpt { .. } => p
            .label
            .as_ref()
            .map(|s| clean_label(s))
            .filter(|s| !s.is_empty()),
        // MBR / Whole：没有标签这一概念（`label` 字段即使有值也不是用户给的标签）
        _ => None,
    }
}

/// **类型描述**（界面类型列用）：GPT 按类型 GUID、MBR 按类型字节。
fn type_name_of(p: &disk_partitions::Partition, table: TableKind) -> String {
    match (&p.kind, table) {
        (disk_partitions::PartitionKind::Gpt { type_guid, .. }, _) => {
            gpt_type_name(type_guid).unwrap_or("GPT").to_string()
        }
        (disk_partitions::PartitionKind::Mbr { type_byte, .. }, _) => {
            mbr_type_name(*type_byte).to_string()
        }
        _ => "Volume".to_string(),
    }
}

/// GPT 分区名可能带控制字符或很长（规范给 36 个 UTF-16 码元）——裁到 48 字节并去掉
/// 控制字符，避免把界面搞乱（名字来自别人的磁盘，不可信）。
fn clean_label(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= 48 {
        return trimmed.to_string();
    }
    trimmed.chars().take(47).collect::<String>() + "…"
}

/// 常见 MBR 类型字节 → 名字（只列测试与真实镜像里最常见的几种）。
fn mbr_type_name(b: u8) -> &'static str {
    match b {
        0x07 => "NTFS/exFAT",
        0x0B => "FAT32 (CHS)",
        0x0C => "FAT32 LBA",
        0x0E => "FAT16 LBA",
        0x82 => "Linux swap",
        0x83 => "Linux",
        0x8E => "Linux LVM",
        0xA5 => "FreeBSD",
        0xAF => "HFS+",
        0xEE => "GPT 保护性",
        0xEF => "EFI System",
        0x00 => "空项",
        _ => "MBR 分区",
    }
}

/// 常见 GPT 类型 GUID → 名字（磁盘字节序；只列常用的几种）。
fn gpt_type_name(guid: &[u8; 16]) -> Option<&'static str> {
    const EFI_SYSTEM: [u8; 16] = [
        0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9,
        0x3B,
    ];
    const LINUX_FS: [u8; 16] = [
        0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D,
        0xE4,
    ];
    const LINUX_LVM: [u8; 16] = [
        0x79, 0xD3, 0xD6, 0xE6, 0x07, 0xF5, 0xC2, 0x44, 0xA2, 0x3C, 0x23, 0x8F, 0x2A, 0x3D, 0xF9,
        0x28,
    ];
    const MICROSOFT_BASIC: [u8; 16] = [
        0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99,
        0xC7,
    ];
    match *guid {
        EFI_SYSTEM => Some("EFI System"),
        LINUX_FS => Some("Linux filesystem"),
        LINUX_LVM => Some("Linux LVM"),
        MICROSOFT_BASIC => Some("Microsoft basic data"),
        _ => None,
    }
}

/// 从头部字节判文件系统（**单一事实源**：`hzip-fmt-raw` 的探针也调它）。
///
/// 覆盖两类位置：
/// - **偏移 0**：NTFS（OEM 名 `NTFS    `）、exFAT（`EXFAT   `）在偏移 3；
///   FAT12/16（`FAT12   `/`FAT16   ` @ `0x36`）、FAT32（`FAT32   ` @ `0x52`）；
/// - **偏移 1080**：ext2/3/4 的 `s_magic`（0xEF53）——超级块固定起于 1024，
///   魔数在其内部偏移 56。⚠ 1024 处是 `s_inodes_count`，**别按 `53 EF`@1024 认 ext4**。
///
/// 认出多少算多少：`None` = 这不是我们认识的文件系统（不代表不是镜像）。
#[must_use]
pub fn fs_kind_in_head(head: &[u8]) -> Option<FsKind> {
    if head.len() >= 90 {
        if &head[3..11] == b"NTFS    " {
            return Some(FsKind::Ntfs);
        }
        if &head[3..11] == b"EXFAT   " {
            return Some(FsKind::ExFat);
        }
        let fat12_16 = &head[0x36..0x3E] == b"FAT12   " || &head[0x36..0x3E] == b"FAT16   ";
        let fat32 = head.len() >= 0x5A && &head[0x52..0x5A] == b"FAT32   ";
        if fat12_16 || fat32 {
            return Some(FsKind::Fat);
        }
    }
    if head.len() >= 1082 && head[1080..1082] == [0x53, 0xEF] {
        let inodes = u32::from_le_bytes([head[1024], head[1025], head[1026], head[1027]]);
        let log_bs = u32::from_le_bytes([head[1048], head[1049], head[1050], head[1051]]);
        if inodes != 0 && log_bs <= 6 {
            return Some(FsKind::Ext4);
        }
    }
    None
}

/// 探测整卷（无分区表时的「整卷就是一个文件系统」）。
pub fn sniff_whole_device(dev: &Arc<dyn BlockDevice>) -> VolResult<Option<FsKind>> {
    let head = crate::registry::FsRegistry::read_head(dev)?;
    Ok(fs_kind_in_head(&head))
}

/// 分区数 0 时的**整卷**视图（无分区表镜像：把整卷当一个文件系统卷）。
#[must_use]
pub fn whole_view(size: u64) -> PartitionView {
    PartitionView {
        id: 0,
        start: 0,
        len: size,
        type_byte: None,
        // 整卷没有分区标签；名字由投影层用 ASCII 稳定的 `Volume` 起（界面按语言显示「卷」）
        label: None,
        type_name: "Volume".to_string(),
        fs: None,
    }
}

/// 便捷：探测失败时给出与「无表」一致的结果（上层不必分支处理错误）。
impl PartitionProbe {
    /// 是否有可用的分区表。
    #[must_use]
    pub fn has_table(&self) -> bool {
        self.table.is_some()
    }

    /// 分区数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.partitions.len()
    }

    /// 空（无表）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.partitions.is_empty()
    }
}

/// 把探测结果折成「卷列表」：有表 ⇒ 各分区；无表 ⇒ 单个整卷视图。
#[must_use]
pub fn volume_views(probe: &PartitionProbe, size: u64) -> Vec<PartitionView> {
    if probe.partitions.is_empty() {
        vec![whole_view(size)]
    } else {
        probe.partitions.clone()
    }
}

/// 打开第 `part` 个分区的设备（`None` = 无表时的整卷）。
pub fn open_region(
    dev: &Arc<dyn BlockDevice>,
    probe: &PartitionProbe,
    part: Option<usize>,
) -> VolResult<Arc<dyn BlockDevice>> {
    let Some(id) = part else {
        return Ok(whole(dev));
    };
    let view = probe
        .partitions
        .get(id)
        .ok_or_else(|| VolError::Image(format!("分区 {id} 不存在")))?;
    Ok(region(dev, view.start, view.len))
}
