//! FAT32 文件系统镜像夹具：用上游 `fatfs::format_volume` **现场造**。
//!
//! # 容量为什么是 34 MiB
//!
//! 不是我们挑的，是格式的下界：FAT32 要求簇数 ≥ 65525（低于此按规范应判 FAT16），
//! 而 `fatfs` 在该簇数下默认取 512 B 簇 ⇒ 65525 × 512 B ≈ 32 MiB。取 34 MiB 留出
//! 表区与对齐余量，让 `fat_type()` 稳定判定为 FAT32。
//!
//! # 确定性
//!
//! 时间戳经 [`FIXED_TIME`] 注入（`fatfs` 的 `TimeProvider`），并关闭**访问日期更新**
//! （`update_accessed_date(false)`）——后者还顺带保证「读操作不改盘」，正是 hzip 对
//! 只读浏览的红线要求。`fatfs` 的 `Date`/`Time`/`DateTime` 是公开字段的结构体
//! （没有公开构造函数），夹具直接写字面量，不取系统时钟。

use std::io::Cursor;

use fatfs::{Date, DateTime, Time, TimeProvider};

/// 夹具 FAT32 镜像容量（34 MiB；见模块文档）。
pub const FAT32_IMAGE_BYTES: u64 = 34 << 20;

/// 逻辑扇区字节数（FAT 的 LBA 单位）。
pub const FAT_SECTOR_BYTES: u16 = 512;

/// 固定卷标（11 字节，空格补齐）。
pub const FAT_VOLUME_LABEL: [u8; 11] = *b"HZIP-FAT32 ";

/// 固定时间（2023-11-14 22:13:20）——与 ext4 夹具的 `FIXED_UNIX_SECONDS` 同刻。
pub static FIXED_TIME: FixedTime = FixedTime;

/// 恒定时钟：所有时间戳取同一值（确定性夹具的一部分）。
#[derive(Debug)]
pub struct FixedTime;

impl TimeProvider for FixedTime {
    fn get_current_date(&self) -> Date {
        Date {
            year: 2023,
            month: 11,
            day: 14,
        }
    }

    fn get_current_date_time(&self) -> DateTime {
        DateTime {
            date: self.get_current_date(),
            time: Time {
                hour: 22,
                min: 13,
                sec: 20,
                millis: 0,
            },
        }
    }
}

/// 造一个含给定文件的 FAT32 镜像。
///
/// 路径按 `/` 分段建目录（FAT 没有隐式父目录的概念）；文件写完必须**先 drop**
/// 再 `unmount`（`fatfs` 在文件句柄关闭时落目录项，`File` 没有显式 flush）。
#[must_use]
pub fn fat32_image(files: &[(&str, &[u8])]) -> Vec<u8> {
    let total_sectors = u32::try_from(FAT32_IMAGE_BYTES / u64::from(FAT_SECTOR_BYTES))
        .expect("夹具：FAT 扇区数应落在 u32");
    let mut cur = Cursor::new(vec![0u8; FAT32_IMAGE_BYTES as usize]);
    let opts = fatfs::FormatVolumeOptions::new()
        .fat_type(fatfs::FatType::Fat32)
        .bytes_per_sector(FAT_SECTOR_BYTES)
        .total_sectors(total_sectors)
        .volume_label(FAT_VOLUME_LABEL);
    fatfs::format_volume(&mut cur, opts).expect("夹具：FAT32 格式化应成功");

    {
        let fs = fatfs::FileSystem::new(
            &mut cur,
            fatfs::FsOptions::new()
                .update_accessed_date(false)
                .time_provider(&FIXED_TIME),
        )
        .expect("夹具：刚格式化的 FAT32 应可挂载");
        assert_eq!(fs.fat_type(), fatfs::FatType::Fat32, "夹具：应为 FAT32");
        for (path, data) in files {
            put_file(&fs, path, data);
        }
        fs.unmount().expect("夹具：FAT32 卸载应成功");
    }
    cur.into_inner()
}

/// 建父目录（逐级）+ 建文件 + 写内容（文件写出块后 drop ⇒ 目录项落盘）。
fn put_file<T: fatfs::ReadWriteSeek>(fs: &fatfs::FileSystem<T>, path: &str, data: &[u8]) {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    assert!(!segs.is_empty(), "夹具：文件路径不能为空（{path}）");
    let mut dir = fs.root_dir();
    for seg in &segs[..segs.len() - 1] {
        dir = match dir.open_dir(seg) {
            Ok(existing) => existing,
            Err(_) => dir
                .create_dir(seg)
                .unwrap_or_else(|e| panic!("夹具：建目录 {seg} 失败：{e}")),
        };
    }
    let name = segs[segs.len() - 1];
    {
        let mut f = dir
            .create_file(name)
            .unwrap_or_else(|e| panic!("夹具：建文件 {name} 失败：{e}"));
        std::io::Write::write_all(&mut f, data)
            .unwrap_or_else(|e| panic!("夹具：写 {name} 失败：{e}"));
    }
}
