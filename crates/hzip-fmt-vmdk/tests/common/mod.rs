//! VMDK 容器测试共用的夹具与小工具。
//!
//! # 为什么夹具在这里而不在 `hzip-format-testkit`
//!
//! 本波次的硬约束是「只改 `crates/hzip-fmt-vmdk/**`」，且与其它 agent 约定「夹具放
//! 各自 crate 的 `tests/common/`」（先例：`crates/hzip-fs-fat/tests/common/mod.rs`）。
//! 故这里**手写最小 VMDK 字节**：确定性、零外部工具、零上游构造器——
//! 上游改了实现，夹具不会跟着变（那才是夹具该有的独立性）。
//!
//! # 布局（全部**小端**，稀疏头 512 字节）
//!
//! ```text
//!   0   4  magic        = 'K''D''M''V'（小端 0x564D444B）
//!   4   4  version      = 1（普通 sparse）/ 3（streamOptimized）
//!   8   4  flags        = 0x2（冗余 grain 目录存在）
//!  12   8  capacity     （512B 扇区数 = 虚拟容量）
//!  20   8  grainSize    = 128 扇区（64 KiB）
//!  28   8  descriptorOffset / 36 8 descriptorSize（各 1 扇区）
//!  44   4  numGTEsPerGT = 512
//!  48   8  rgdOffset    / 56 8 gdOffset
//!  64   8  overHead     （首个 grain 的扇区号）
//!  72   1  uncleanShutdown
//!  77   2  compressAlgorithm（0 = 无、1 = DEFLATE）
//! ```
//!
//! 稀疏镜像的扇区分布：`0 头 | 1 描述符 | 2 RGD | 3 GD | 4.. 主 GT | .. 冗余 GT |
//! overHead.. grain 数据`。

#![allow(clippy::unwrap_used, clippy::expect_used)]
// 共用的测试模块：每个测试二进制只用其中一部分，未用到的项不算问题。
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use hzip_format_testkit::vol::MemDevice;
use hzip_fs_ext4::Ext4Driver;
use hzip_vol::registry::FsRegistry;

/// VMDK 逻辑扇区（格式固定 512）。
pub const SECTOR: usize = 512;
/// 每个 grain 的扇区数（128 × 512 B = 64 KiB）。
pub const GRAIN_SECTORS: u64 = 128;
/// 每张 grain 表的表项数。
pub const GTES_PER_GT: u64 = 512;

// ---- 稀疏头字段偏移（夹具独立写死；不从被测代码取，免得把 bug 镜像进夹具）----
const O_MAGIC: usize = 0;
const O_VERSION: usize = 4;
const O_FLAGS: usize = 8;
const O_CAPACITY: usize = 12;
const O_GRAIN_SIZE: usize = 20;
const O_DESC_OFF: usize = 28;
const O_DESC_SIZE: usize = 36;
const O_NUM_GTES: usize = 44;
const O_RGD_OFF: usize = 48;
const O_GD_OFF: usize = 56;
const O_OVER_HEAD: usize = 64;
/// `uncleanShutdown`（1 字节）。
pub const O_UNCLEAN: usize = 72;
const O_COMPRESS: usize = 77;

/// 稀疏头魔数（磁盘上即 `KDMV`）。
const MAGIC: u32 = 0x564D_444B;
/// ESXi `vmfsSparse` 魔数（磁盘上即 `COWD`）。
const MAGIC_VMFS: u32 = 0x4457_4F43;
/// `flags` 位 1：镜像带冗余 grain 目录。
const FLAG_REDUNDANT_GT: u32 = 0x2;

/// 真驱动注册表（只有 ext4；NTFS/FAT 由各自的 crate 提供）。
pub fn registry() -> Arc<FsRegistry> {
    let mut reg = FsRegistry::new();
    reg.register(Box::new(Ext4Driver::new()));
    Arc::new(reg)
}

/// 写一个临时文件，返回（保活目录，路径）。
pub fn write_temp(name: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(name);
    std::fs::write(&p, bytes).unwrap();
    (dir, p)
}

/// 文件 sha256（提交前后对比用）。
pub fn sha256_file(path: &std::path::Path) -> [u8; 32] {
    use sha2::Digest as _;
    let bytes = std::fs::read(path).unwrap();
    sha2::Sha256::digest(bytes).into()
}

/// 标 `uncleanShutdown`（模拟「未干净关闭」的镜像）。
pub fn mark_unclean_shutdown(image: &mut [u8]) {
    image[O_UNCLEAN] = 1;
}

/// 取出内嵌描述符里的 `CID=`（提交前后对比用；找不到 ⇒ panic）。
pub fn content_id(image: &[u8]) -> String {
    let text = String::from_utf8_lossy(image);
    text.lines()
        .find_map(|l| l.trim().strip_prefix("CID="))
        .map(|v| v.trim_matches(|c: char| !c.is_ascii_hexdigit()).to_string())
        .expect("夹具：描述符里应有 CID=")
}

// ---------------------------------------------------------------- 夹具

/// `monolithicSparse`：全部 grain 已分配。
#[must_use]
pub fn vmdk_sparse(raw: &[u8]) -> Vec<u8> {
    vmdk_sparse_inner(raw, &[])
}

/// `monolithicSparse`：`holes` 里的 grain 序号**未分配**（读回应为 0）。
#[must_use]
pub fn vmdk_sparse_with_holes(raw: &[u8], holes: &[u64]) -> Vec<u8> {
    vmdk_sparse_inner(raw, holes)
}

/// `monolithicSparse`（头 + 描述符 + RGD + GD + GT + grain，**小端**）。
fn vmdk_sparse_inner(raw: &[u8], holes: &[u64]) -> Vec<u8> {
    assert_eq!(raw.len() % SECTOR, 0, "夹具：raw 长度须为 512 的整数倍");
    let capacity = raw.len() as u64 / SECTOR as u64;
    assert!(capacity > 0, "夹具：raw 不得为空");

    let grains_total = capacity.div_ceil(GRAIN_SECTORS);
    let gt_count = grains_total.div_ceil(GTES_PER_GT);
    let gd_sectors = (gt_count * 4).div_ceil(SECTOR as u64);
    assert!(gd_sectors <= 1, "夹具：只造小镜像（GD 占一扇区）");
    let gt_sectors = (GTES_PER_GT * 4).div_ceil(SECTOR as u64);

    // 扇区块位
    let desc_sector = 1u64;
    let rgd_sector = 2u64;
    let gd_sector = rgd_sector + gd_sectors;
    let pri_gt_sector = gd_sector + gd_sectors;
    let red_gt_sector = pri_gt_sector + gt_count * gt_sectors;
    let over_head = red_gt_sector + gt_count * gt_sectors;

    // 分配 grain 宿主扇区（跳过的序号即为空洞）
    let hole_set: std::collections::HashSet<u64> = holes.iter().copied().collect();
    let mut grain_host = vec![0u32; grains_total as usize];
    let mut next = over_head;
    for g in 0..grains_total {
        if hole_set.contains(&g) {
            continue;
        }
        assert!(next <= u32::MAX as u64, "夹具：扇区号超出 u32");
        grain_host[g as usize] = next as u32;
        next += GRAIN_SECTORS;
    }
    let total_sectors = next;

    let mut img = vec![0u8; (total_sectors as usize) * SECTOR];
    // 头
    put_header(
        &mut img,
        Header {
            version: 1,
            flags: FLAG_REDUNDANT_GT,
            capacity,
            descriptor_offset: desc_sector,
            descriptor_size: 1,
            rgd_offset: rgd_sector,
            gd_offset: gd_sector,
            over_head,
            compress: 0,
        },
    );
    // 描述符
    put_text(&mut img, desc_sector, &sparse_descriptor(capacity));

    // 主 GT + 冗余 GT（两者逐字节一致；写侧负责保持同步）
    let mut gd = vec![0u8; (gd_sectors as usize) * SECTOR];
    let mut rgd = vec![0u8; (gd_sectors as usize) * SECTOR];
    for t in 0..gt_count {
        let mut gt = vec![0u8; (gt_sectors as usize) * SECTOR];
        gd[t as usize * 4..t as usize * 4 + 4]
            .copy_from_slice(&((pri_gt_sector + t * gt_sectors) as u32).to_le_bytes());
        rgd[t as usize * 4..t as usize * 4 + 4]
            .copy_from_slice(&((red_gt_sector + t * gt_sectors) as u32).to_le_bytes());
        for e in 0..GTES_PER_GT {
            // 空洞（`grain_host == 0`）保持表项为 0 ⇒ 读回全零
            let g = t * GTES_PER_GT + e;
            if g >= grains_total || grain_host[g as usize] == 0 {
                continue;
            }
            let at = e as usize * 4;
            gt[at..at + 4].copy_from_slice(&grain_host[g as usize].to_le_bytes());
        }
        put_bytes(&mut img, pri_gt_sector + t * gt_sectors, &gt);
        put_bytes(&mut img, red_gt_sector + t * gt_sectors, &gt);
    }
    put_bytes(&mut img, rgd_sector, &rgd);
    put_bytes(&mut img, gd_sector, &gd);
    // grain 数据
    for g in 0..grains_total {
        if grain_host[g as usize] == 0 {
            continue;
        }
        let src = &raw[(g * GRAIN_SECTORS) as usize * SECTOR..];
        let len = (GRAIN_SECTORS as usize * SECTOR).min(src.len());
        let at = grain_host[g as usize] as usize * SECTOR;
        img[at..at + len].copy_from_slice(&src[..len]);
    }
    img
}

/// `streamOptimized`：头（v3 + DEFLATE）+ 描述符 + 全零 GD。
///
/// 全零 GD ⇒ 每个 grain 都是空洞 ⇒ 读出全零——足够验「读可用 + 只读判定」，
/// 无需真的压一份 DEFLATE 数据（本 crate 不引压缩依赖）。
#[must_use]
pub fn vmdk_stream_optimized(capacity_sectors: u64) -> Vec<u8> {
    let gt_count = capacity_sectors
        .div_ceil(GRAIN_SECTORS)
        .div_ceil(GTES_PER_GT);
    let gd_sectors = (gt_count * 4).div_ceil(SECTOR as u64).max(1);
    let desc_sector = 1u64;
    let gd_sector = 2u64;
    let over_head = gd_sector + gd_sectors;

    let mut img = vec![0u8; (over_head as usize) * SECTOR];
    put_header(
        &mut img,
        Header {
            version: 3,
            flags: 0,
            capacity: capacity_sectors,
            descriptor_offset: desc_sector,
            descriptor_size: 1,
            rgd_offset: 0,
            gd_offset: gd_sector,
            over_head,
            compress: 1, // DEFLATE
        },
    );
    put_text(&mut img, desc_sector, &stream_descriptor(capacity_sectors));
    img
}

/// 只有描述符文本的 `.vmdk`（flat / 多 extent / delta 的形态；**没有魔数**）。
#[must_use]
pub fn vmdk_descriptor_file(
    create_type: &str,
    extent_kind: &str,
    capacity_sectors: u64,
    parent: Option<&str>,
) -> Vec<u8> {
    let mut s = String::new();
    s.push_str("# Disk DescriptorFile\n");
    s.push_str("version=1\n");
    s.push_str("CID=fffffffe\n");
    s.push_str("parentCID=ffffffff\n");
    s.push_str(&format!("createType=\"{create_type}\"\n"));
    if let Some(parent) = parent {
        s.push_str(&format!("parentFileNameHint=\"{parent}\"\n"));
    }
    s.push('\n');
    // FLAT extent 行带一个尾部偏移（真实产物如此）
    if extent_kind == "FLAT" {
        s.push_str(&format!(
            "RW {capacity_sectors} FLAT \"hzip-fixture-flat.vmdk\" 0\n"
        ));
    } else {
        s.push_str(&format!(
            "RW {capacity_sectors} {extent_kind} \"hzip-fixture.vmdk\"\n"
        ));
    }
    s.into_bytes()
}

/// ESXi `vmfsSparse` extent：`COWD` 魔数的 4 KiB 文件（**不是** KDMV 头）。
#[must_use]
pub fn vmdk_vmfs_cowd() -> Vec<u8> {
    let mut img = vec![0u8; 4 * SECTOR];
    img[O_MAGIC..O_MAGIC + 4].copy_from_slice(&MAGIC_VMFS.to_le_bytes());
    // 头部其余字段给「像回事」的值，证明拒绝来自魔数分支而非别处
    img[O_VERSION..O_VERSION + 4].copy_from_slice(&1u32.to_le_bytes());
    img[O_CAPACITY..O_CAPACITY + 8].copy_from_slice(&2048u64.to_le_bytes());
    img
}

/// 只读 + 计数的设备包装（拒写用例断言「不读数据区」）。
///
/// 返回（计数器，设备）：计数器的 `reads()`/`bytes()` 是该设备被读的量。
pub fn counting_read_only(
    img: Vec<u8>,
) -> (Arc<fs_core::CountingDevice>, Arc<dyn hzip_vol::BlockDevice>) {
    let inner: Arc<dyn hzip_vol::BlockRead> = Arc::new(MemDevice::from_bytes(img));
    let counter = Arc::new(fs_core::CountingDevice::new(inner));
    let dev: Arc<dyn hzip_vol::BlockDevice> =
        Arc::new(fs_core::ReadOnlyDevice::new(counter.clone()));
    (counter, dev)
}

// ---------------------------------------------------------------- 头部/文本写入

struct Header {
    version: u32,
    flags: u32,
    capacity: u64,
    descriptor_offset: u64,
    descriptor_size: u64,
    rgd_offset: u64,
    gd_offset: u64,
    over_head: u64,
    compress: u16,
}

fn put_header(img: &mut [u8], h: Header) {
    img[O_MAGIC..O_MAGIC + 4].copy_from_slice(&MAGIC.to_le_bytes());
    put_u32(img, O_VERSION, h.version);
    put_u32(img, O_FLAGS, h.flags);
    put_u64(img, O_CAPACITY, h.capacity);
    put_u64(img, O_GRAIN_SIZE, GRAIN_SECTORS);
    put_u64(img, O_DESC_OFF, h.descriptor_offset);
    put_u64(img, O_DESC_SIZE, h.descriptor_size);
    put_u32(img, O_NUM_GTES, GTES_PER_GT as u32);
    put_u64(img, O_RGD_OFF, h.rgd_offset);
    put_u64(img, O_GD_OFF, h.gd_offset);
    put_u64(img, O_OVER_HEAD, h.over_head);
    img[O_UNCLEAN] = 0;
    put_u16(img, O_COMPRESS, h.compress);
}

fn put_text(img: &mut [u8], sector: u64, text: &str) {
    let at = sector as usize * SECTOR;
    assert!(at + text.len() <= img.len(), "夹具：文本超出镜像");
    assert!(text.len() <= SECTOR, "夹具：文本超过一扇区");
    img[at..at + text.len()].copy_from_slice(text.as_bytes());
}

fn put_bytes(img: &mut [u8], sector: u64, bytes: &[u8]) {
    let at = sector as usize * SECTOR;
    img[at..at + bytes.len()].copy_from_slice(bytes);
}

fn put_u16(img: &mut [u8], off: usize, v: u16) {
    img[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(img: &mut [u8], off: usize, v: u32) {
    img[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(img: &mut [u8], off: usize, v: u64) {
    img[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn sparse_descriptor(capacity: u64) -> String {
    let mut s = String::new();
    s.push_str("# Disk DescriptorFile\n");
    s.push_str("version=1\n");
    s.push_str("encoding=\"UTF-8\"\n");
    s.push_str("CID=fffffffe\n");
    s.push_str("parentCID=ffffffff\n");
    s.push_str("createType=\"monolithicSparse\"\n");
    s.push('\n');
    s.push_str(&format!("RW {capacity} SPARSE \"hzip-fixture.vmdk\"\n"));
    s.push_str("ddb.virtualHWVersion = \"4\"\n");
    s
}

fn stream_descriptor(capacity: u64) -> String {
    let mut s = String::new();
    s.push_str("# Disk DescriptorFile\n");
    s.push_str("version=1\n");
    s.push_str("CID=fffffffe\n");
    s.push_str("parentCID=ffffffff\n");
    s.push_str("createType=\"streamOptimized\"\n");
    s.push('\n');
    s.push_str(&format!("RW {capacity} SPARSE \"hzip-fixture.vmdk\"\n"));
    s.push_str("ddb.virtualHWVersion = \"4\"\n");
    s
}
