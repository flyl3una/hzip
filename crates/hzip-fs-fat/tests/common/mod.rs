//! FAT 驱动测试共用的夹具与小工具。
//!
//! # 为什么 FAT12/16 的夹具在本目录而不在 `hzip-format-testkit`
//!
//! testkit 的 `vol` 模块本期只有 `fat32_image`（T-IMG12 计划里列的 `fat12_image`/
//! `fat16_image` 尚未落地），而本波次的硬约束是「只改 `crates/hzip-fs-fat/**`」。
//! 故本模块**照 testkit `vol/fat.rs` 的同一手法**（上游 `fatfs::format_volume`
//! 现场造，注入固定时钟）补齐 12/16 两档：同源、确定性、可复现。
//!
//! # 确定性
//!
//! 不读系统时钟：写侧的时间戳来自 [`FixedTime`]（2023-11-14 22:13:20，
//! 与 testkit 的 `FIXED_TIME` / ext4 的 `FIXED_UNIX_SECONDS` 同刻）。

#![allow(clippy::unwrap_used, clippy::expect_used)]
// 共用的测试模块：每个测试二进制只用其中一部分，未用到的项不算问题。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fatfs::{Date, DateTime, FatType, FormatVolumeOptions, FsOptions, Time, TimeProvider};
use hzip_format::EntryId;
use hzip_format::EntryInfo;
use hzip_format_testkit::vol::MemDevice;
use hzip_format_testkit::vol::fat::{FAT_SECTOR_BYTES, FAT32_IMAGE_BYTES};
use hzip_fs_fat::FatDriver;
use hzip_progress::CancellationToken;
use hzip_vol::{BlockDevice, EntryLimit, EntryTree, FsDriver, FsOptions as VolFsOptions, FsVolume};

/// 固定时刻（2023-11-14T22:13:20Z = ext4 夹具的 `FIXED_UNIX_SECONDS`）。
pub const FIXED_UNIX_SECONDS: i64 = 1_700_000_000;

/// 夹具卷标（11 字节，空格补齐；与 testkit 同值）。
pub const VOLUME_LABEL: [u8; 11] = *b"HZIP-FAT32 ";

/// 恒定时钟（写目录项时用）。
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

/// 固定时钟（`TimeProvider` 要求 `&'static`）。
pub static FIXED_TIME: FixedTime = FixedTime;

/// 挂载后的卷 + 它脚下的内存盘（`MemDevice::clone` 共享底层 ⇒ 写后能读出字节）。
pub struct Mounted {
    /// 卷视图。
    pub fs: Box<dyn FsVolume>,
    /// 内存盘句柄（`to_bytes()` 拿落盘字节）。
    pub dev: MemDevice,
}

/// 写模式的挂载选项。
///
/// 注意：`hzip_vol::FsOptions` 的 `Default` 是**派生**的 ⇒ `writable == false`（只读）。
/// 那是卷层有意的安全默认（要写必须显式声明）；测试里写明意图。
pub fn writable_opts() -> VolFsOptions {
    VolFsOptions {
        writable: true,
        ..VolFsOptions::default()
    }
}

/// 以写模式挂载。
pub fn mount(dev: MemDevice) -> Mounted {
    let shared = dev.clone();
    let device: Arc<dyn BlockDevice> = Arc::new(shared);
    let fs = FatDriver::new()
        .mount(device, &writable_opts())
        .expect("FAT 夹具应能挂载");
    Mounted { fs, dev }
}

/// 从镜像字节挂载。
pub fn mount_image(image: Vec<u8>) -> Mounted {
    mount(MemDevice::from_bytes(image))
}

/// 新取消令牌（根）。
pub fn cancel() -> CancellationToken {
    CancellationToken::new()
}

/// 列全树（默认上限）。
pub fn tree(fs: &dyn FsVolume) -> EntryTree {
    tree_with(fs, &EntryLimit::default())
}

/// 按给定上限列树。
pub fn tree_with(fs: &dyn FsVolume, limit: &EntryLimit) -> EntryTree {
    fs.entries(limit, &cancel()).expect("列目录应成功")
}

/// 按名字取条目（找不到即失败——测试里都是我们刚造的条目）。
pub fn entry<'t>(tree: &'t EntryTree, name: &str) -> &'t EntryInfo {
    tree.by_name(name)
        .unwrap_or_else(|| panic!("树里应有条目 {name}；实际 = {:?}", names(tree)))
}

/// 树里的名字（失败信息用）。
pub fn names(tree: &EntryTree) -> Vec<&str> {
    tree.entries.iter().map(|e| e.name.as_str()).collect()
}

/// 按名字取 id。
pub fn id_of(fs: &dyn FsVolume, name: &str) -> EntryId {
    entry(&tree(fs), name).id
}

/// 逐个 `read_at` 读完一个条目（顺带把「惰性+任意偏移」走一遍）。
pub fn read_all(fs: &dyn FsVolume, id: EntryId) -> Vec<u8> {
    let size = fs.size_of(id).expect("size_of 应成功");
    let mut out = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    let mut buf = vec![0u8; 4096];
    let mut off = 0u64;
    loop {
        let n = fs.read_at(id, off, &mut buf).expect("read_at 应成功");
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        off += n as u64;
    }
    assert_eq!(out.len() as u64, size, "读出的字节数应与 size_of 一致");
    out
}

/// 按名字读全文。
pub fn read_name(fs: &dyn FsVolume, name: &str) -> Vec<u8> {
    read_all(fs, id_of(fs, name))
}

// ---------------------------------------------------------------- 镜像夹具

/// 造一个 FAT 镜像（12/16/32 三档通用；手法与 testkit `vol/fat.rs` 相同）。
///
/// `bytes` 是**卷容量**；`bytes_per_cluster` 为 `None` 时由上游按容量推。
pub fn fat_image(
    fat_type: FatType,
    bytes: u64,
    bytes_per_cluster: Option<u32>,
    files: &[(&str, &[u8])],
) -> Vec<u8> {
    let total_sectors =
        u32::try_from(bytes / u64::from(FAT_SECTOR_BYTES)).expect("夹具：扇区数应落在 u32");
    let mut image = vec![0u8; usize::try_from(bytes).expect("夹具：容量应落在 usize")];
    let mut options = FormatVolumeOptions::new()
        .fat_type(fat_type)
        .bytes_per_sector(FAT_SECTOR_BYTES)
        .total_sectors(total_sectors)
        .volume_label(VOLUME_LABEL);
    if let Some(bpc) = bytes_per_cluster {
        options = options.bytes_per_cluster(bpc);
    }
    fatfs::format_volume(std::io::Cursor::new(&mut image), options).expect("夹具：格式化应成功");
    {
        let mut cursor = std::io::Cursor::new(&mut image);
        let fs = fatfs::FileSystem::new(
            &mut cursor,
            FsOptions::new()
                .update_accessed_date(false)
                .time_provider(&FIXED_TIME),
        )
        .expect("夹具：刚格式化的卷应可挂载");
        assert_eq!(fs.fat_type(), fat_type, "夹具：FAT 宽度应与请求一致");
        for (path, data) in files {
            put_file(&fs, path, data);
        }
        fs.unmount().expect("夹具：卸载应成功");
    }
    image
}

/// FAT32 夹具（转发 testkit；34 MiB 是格式本身的簇数下界）。
pub fn fat32(files: &[(&str, &[u8])]) -> Vec<u8> {
    let image = hzip_format_testkit::vol::fat32_image(files);
    assert_eq!(image.len() as u64, FAT32_IMAGE_BYTES);
    image
}

/// 1.44 MiB 软盘形态的 FAT12 镜像（512 B 簇 ⇒ 约 2830 簇，落在 FAT12 区间）。
pub fn fat12(files: &[(&str, &[u8])]) -> Vec<u8> {
    fat_image(FatType::Fat12, 1_440 * 1024, Some(512), files)
}

/// 32 MiB 的 FAT16 镜像（8 KiB 簇 ⇒ 约 8100 簇，落在 FAT16 区间）。
pub fn fat16(files: &[(&str, &[u8])]) -> Vec<u8> {
    fat_image(FatType::Fat16, 32 << 20, Some(8 * 1024), files)
}

/// 建父目录（逐级）+ 建文件 + 写内容（写完 drop ⇒ 目录项落盘）。
fn put_file<T: fatfs::ReadWriteSeek>(fs: &fatfs::FileSystem<T>, path: &str, data: &[u8]) {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    assert!(!segments.is_empty(), "夹具：路径不能为空（{path}）");
    let mut dir = fs.root_dir();
    for segment in &segments[..segments.len() - 1] {
        dir = match dir.open_dir(segment) {
            Ok(existing) => existing,
            Err(_) => dir
                .create_dir(segment)
                .unwrap_or_else(|e| panic!("夹具：建目录 {segment} 失败：{e}")),
        };
    }
    let name = segments[segments.len() - 1];
    {
        let mut file = dir
            .create_file(name)
            .unwrap_or_else(|e| panic!("夹具：建文件 {name} 失败：{e}"));
        std::io::Write::write_all(&mut file, data)
            .unwrap_or_else(|e| panic!("夹具：写 {name} 失败：{e}"));
    }
}

// ---------------------------------------------------------------- 字节级改写

/// FAT32 根目录的字节偏移（按 BPB 算：数据区起点 + 根目录首簇）。
pub fn root_dir_offset(image: &[u8]) -> usize {
    let bps = usize::from(u16::from_le_bytes([image[11], image[12]]));
    let spc = usize::from(image[13]);
    let reserved = usize::from(u16::from_le_bytes([image[14], image[15]]));
    let fats = usize::from(image[16]);
    let root_entries = usize::from(u16::from_le_bytes([image[17], image[18]]));
    // FAT32 的 sectors-per-fat 在 0x24（0x16 那个 16 位字段为 0）。
    let spf = match u16::from_le_bytes([image[22], image[23]]) {
        0 => usize::try_from(u32::from_le_bytes([
            image[36], image[37], image[38], image[39],
        ]))
        .unwrap(),
        n => usize::from(n),
    };
    let root_cluster = usize::try_from(u32::from_le_bytes([
        image[44], image[45], image[46], image[47],
    ]))
    .unwrap();
    let root_sectors = (root_entries * 32).div_ceil(bps);
    let data_start = (reserved + fats * spf + root_sectors) * bps;
    let start = data_start + (root_cluster - 2) * bps * spc;
    // 自检：算出来的偏移必须指向一个目录项（否则后面的字节级改写会悄悄改错地方）。
    assert!(
        matches!(image[start + 11], 0x08 | 0x0F | 0x10 | 0x20 | 0x30),
        "夹具：根目录偏移 {start} 处不是目录项（attr = {:#04x}）",
        image[start + 11]
    );
    start
}

/// 把某 8.3 条目**前面的 LFN 项**标成已删除（0xE5）⇒ 造出「只有短名」的真实形态。
///
/// 上游 `fatfs` 建文件时总会写 LFN 项（哪怕名字本来就合规），所以「8.3 单独存在」
/// 这种形态只能这样造；真实世界的老工具/嵌入式写出来的盘就是这样。
pub fn strip_lfn(image: &mut [u8], sfn: &[u8; 11]) {
    let start = root_dir_offset(image);
    let dir_end = start + 32 * 512; // 根目录首簇（512 项）足够放下夹具的所有条目
    let mut pos = start;
    while pos + 32 <= dir_end {
        let entry = &image[pos..pos + 32];
        if entry[0] == 0x00 {
            break; // 目录结束标记
        }
        if entry[11] != 0x0F && entry[..11] == sfn[..] {
            // 往回删掉连续的 LFN 项
            let mut back = pos;
            while back >= start + 32 && image[back - 32 + 11] == 0x0F {
                back -= 32;
                image[back] = 0xE5;
            }
            return;
        }
        pos += 32;
    }
    panic!(
        "夹具：根目录里找不到短名 {:?}",
        String::from_utf8_lossy(sfn)
    );
}

// ---------------------------------------------------------------- 临时文件

/// 测试临时目录（Cargo 给集成测试准备的 `target/tmp` 子目录）。
pub fn temp_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("夹具：临时目录应可建");
    dir
}

/// 在临时目录里写一个本地文件（`Replace`/`Append` 的源）。
pub fn local_file(name: &str, data: &[u8]) -> PathBuf {
    let path = temp_dir().join(name);
    std::fs::write(&path, data).expect("夹具：本地文件应可写");
    path
}

/// 超过 FAT 上限（4 GiB - 1）的**临时源文件**（drop 即删）。
///
/// `apply` 的「写前硬门」只读它的 `metadata().len()`，**从不读内容**，所以这份文件
/// 不需要真的写数据：`set_len` 一次性把长度顶到 4 GiB + 1（NTFS 上创建/删除都是瞬时，
/// 但**确实占 4 GiB 空间** ⇒ 用完立刻还回去，不给别的活儿留坑）。
pub struct BigSource {
    path: PathBuf,
}

impl BigSource {
    /// 源文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for BigSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 造一份「超上限的源文件」（见 [`BigSource`]）。
pub fn over_limit_source() -> BigSource {
    let path = temp_dir().join("fat-over-limit.bin");
    let want = hzip_fs_fat::FAT_MAX_FILE_BYTES + 1;
    let file = std::fs::File::create(&path).expect("夹具：应能建 4 GiB 源文件");
    file.set_len(want)
        .unwrap_or_else(|e| panic!("夹具：需要 {want} 字节的临时文件（约 4 GiB 空间）：{e}"));
    drop(file);
    assert_eq!(
        std::fs::metadata(&path).expect("刚建的文件").len(),
        want,
        "夹具：长度应正好是 4 GiB + 1"
    );
    BigSource { path }
}

/// 只读设备上的挂载（走 `hzip-vol` 的设备链 `ReadOnly` 末端）。
///
/// 返回的 `Chain` 必须与卷同活（设备句柄共享底层文件）。
pub fn mount_read_only(path: &Path) -> (Box<dyn FsVolume>, hzip_vol::device::Chain) {
    use hzip_format::Source;
    use hzip_vol::VolOptions;
    use hzip_vol::device::{Chain, WriteMode};

    let chain = Chain::open(
        Source::File(path.to_path_buf()),
        WriteMode::ReadOnly,
        &VolOptions::default(),
        None,
    )
    .expect("夹具：只读设备链应能打开");
    assert!(!hzip_vol::BlockDevice::is_writable(&chain.device()));
    let dev = chain.device();
    let fs = FatDriver::new()
        .mount(dev, &writable_opts())
        .expect("只读设备也应能挂载（读侧不受影响）");
    (fs, chain)
}
