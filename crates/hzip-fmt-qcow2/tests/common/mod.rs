//! qcow2 测试夹具：**手写**最小合法 qcow2 v3（大端字段），零外部工具、确定性。
//!
//! # 为什么手写
//!
//! 上游 `rust-img-qcow2` 只有 reader，**没有 create API**，测试要的「真 qcow2」只能
//! 自己拼：头 + refcount 表/块 + L1 + L2 + 数据簇。拼错的风险由测试自己兜住——
//! `tests/read.rs` 里有两道自证：① 夹具造出来的镜像必须能被上游
//! `Qcow2Reader::open_on_device` 打开，且 `virtual_size()`/`cluster_size()` 与构造
//! 参数一致；② 开发机有 `qemu-img` 时再由 `qemu-img info` 交叉复核（缺失则跳过，
//! 不进硬门）。
//!
//! # 磁盘布局（`cluster_bits = 16` ⇒ 64 KiB 簇）
//!
//! ```text
//! 宿主簇 0        头（104 字节有效，其余 0）
//! 宿主簇 1        refcount table（1 个 u64 项 = refcount block 的宿主偏移）
//! 宿主簇 2        refcount block（u16 项，每项 = 一个宿主簇的引用计数）
//! 宿主簇 3        L1 表（1 个 u64 项 = L2 表偏移 | COPIED）
//! 宿主簇 4        L2 表（每个 u64 项 = 数据簇偏移 | COPIED；0 = 未分配）
//! 宿主簇 5..      数据簇（按虚拟簇序，跳过 `holes` 里的洞）
//! 末尾若干        预留空闲簇（refcount = 0）
//! ```
//!
//! # 为什么末尾要预留空闲簇
//!
//! 上游分配器增长文件走 `BlockDevice::set_len`，而 `hzip-vol` 的覆盖层 `set_len`
//! **恒错**（本期不做镜像 resize）。夹具若把文件正好切到最后一个已用簇，第一次分配
//! 就会去要 `set_len` 而失败。预留几个「文件里存在但 refcount = 0」的簇，分配器就会
//! 把它们当空闲簇取走——这既是真实 qcow2 的形态（几乎总有未用尾部），也让写路径在
//! **不扩容**的前提下被完整走一遍。
//!
//! # 大端
//!
//! 所有多字节字段都是**大端**（规格如此）。`w_u16`/`w_u32`/`w_u64` 是这里唯一的
//! 字节序来源——别在别处直接 `copy_from_slice`。

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)] // 每个测试二进制只用其中一部分

use std::collections::BTreeSet;
use std::path::PathBuf;

use hzip_format_testkit::vol::{FixtureFs, raw_with_mbr};

/// 夹具簇位数（64 KiB 簇）：够小（9 MiB 盘 = 144 簇，元数据各占 1 簇也不浪费），
/// 又够大（一个 L1 项覆盖 512 MiB ⇒ `l1_size = 1`，夹具不必拼多级表）。
pub const CLUSTER_BITS: u32 = 16;
/// 夹具簇大小。
pub const CLUSTER_SIZE: u64 = 1 << CLUSTER_BITS;
/// 头部/refcount 表/refcount 块/L1/L2 五个固定宿主簇。
const FIXED_HOST_CLUSTERS: u64 = 5;
/// 数据簇 / L2 项里「该簇 refcount == 1，可直接原地写」的位（规格 bit 63）。
const COPIED: u64 = 1 << 63;

/// `incompatible_features` 位（规格；与上游 `header::incompat` 同值）。
pub const INCOMPAT_DIRTY: u64 = 1 << 0;
/// 镜像已被判定损坏。
pub const INCOMPAT_CORRUPT: u64 = 1 << 1;
/// 外部数据文件（`data-file=`）。
pub const INCOMPAT_DATA_FILE: u64 = 1 << 2;
/// 扩展 L2 项（64 位地址）。
pub const INCOMPAT_EXTENDED_L2: u64 = 1 << 4;

// ---------------------------------------------------------------- 裸镜像

/// 一块 MBR 盘（1 MiB 对齐的单分区），分区内是**真实** ext4（testkit 现场造）。
///
/// 大小恒为 1 MiB + 8 MiB = 9 MiB ⇒ 与 64 KiB 簇正好对齐，是夹具布局的前提。
pub fn raw_ext4_mbr(files: &[(&str, &[u8])]) -> Vec<u8> {
    raw_with_mbr(FixtureFs::Ext4, files)
}

// ---------------------------------------------------------------- qcow2 构造器

/// 把 `raw` 整段装进一个合法 qcow2 v3（未压缩、全部已分配）。
pub fn qcow2_image(raw: &[u8]) -> Vec<u8> {
    qcow2_image_with(raw, &[], 0, 0)
}

/// 稀疏变体：`holes` 列出的**虚拟簇**标为未分配（L2 项为 0 ⇒ 读回 0），
/// 且不为它们落数据簇。
pub fn qcow2_image_sparse(raw: &[u8], holes: &[u64]) -> Vec<u8> {
    qcow2_image_with(raw, holes, 0, 0)
}

/// 完整构造器。
///
/// - `holes`：未分配的虚拟簇（必须在 `raw` 覆盖范围内）；
/// - `tail_virtual_clusters`：虚拟容量末尾追加的未分配簇（虚拟盘比 `raw` 大）；
/// - `spare_free_clusters`：文件里预留、refcount = 0 的宿主簇（供上游分配器取用）。
pub fn qcow2_image_with(
    raw: &[u8],
    holes: &[u64],
    tail_virtual_clusters: u64,
    spare_free_clusters: u64,
) -> Vec<u8> {
    let cs = CLUSTER_SIZE;
    let raw_len = raw.len() as u64;
    assert_eq!(
        raw_len % cs,
        0,
        "夹具：raw 需按 {cs} 字节簇对齐（实际 {raw_len}）"
    );
    let data_clusters = raw_len / cs;
    let holes: BTreeSet<u64> = holes.iter().copied().collect();
    for h in &holes {
        assert!(
            *h < data_clusters,
            "夹具：洞 {h} 必须在 raw 覆盖的虚拟簇内（< {data_clusters}）"
        );
    }

    let allocated: Vec<u64> = (0..data_clusters).filter(|i| !holes.contains(i)).collect();
    let used_host = FIXED_HOST_CLUSTERS + allocated.len() as u64;
    let total_host = used_host + spare_free_clusters;
    let virtual_size = (data_clusters + tail_virtual_clusters) * cs;
    assert_eq!(
        virtual_size.div_ceil(cs * (cs / 8)),
        1,
        "夹具：虚拟容量需落在单个 L1 项内（超出就得拼多级表）"
    );

    let mut img = vec![0u8; usize::try_from(total_host * cs).unwrap()];

    // ---- 头（104 字节有效；`header_length = 104` ⇒ 无附加字段）----
    w_u32(&mut img, 0, 0x5146_49fb); // "QFI\xfb"（大端）
    w_u32(&mut img, 4, 3); // version
    w_u64(&mut img, 8, 0); // backing_file_offset
    w_u32(&mut img, 16, 0); // backing_file_size
    w_u32(&mut img, 20, CLUSTER_BITS);
    w_u64(&mut img, 24, virtual_size);
    w_u32(&mut img, 32, 0); // crypt_method
    w_u32(&mut img, 36, 1); // l1_size
    w_u64(&mut img, 40, 3 * cs); // l1_table_offset
    w_u64(&mut img, 48, cs); // refcount_table_offset
    w_u32(&mut img, 56, 1); // refcount_table_clusters
    w_u32(&mut img, 60, 0); // nb_snapshots
    w_u64(&mut img, 64, 0); // snapshots_offset
    w_u64(&mut img, 72, 0); // incompatible_features
    w_u64(&mut img, 80, 0); // compatible_features
    w_u64(&mut img, 88, 0); // autoclear_features
    w_u32(&mut img, 96, 4); // refcount_order ⇒ 16 位引用计数
    w_u32(&mut img, 100, 104); // header_length

    // ---- refcount 表（宿主簇 1）→ refcount 块（宿主簇 2）----
    w_u64(&mut img, usize::try_from(cs).unwrap(), 2 * cs);
    // refcount 块：已用宿主簇 refcount = 1；预留簇保持 0（= 空闲）
    for i in 0..used_host {
        w_u16(&mut img, usize::try_from(2 * cs + i * 2).unwrap(), 1);
    }

    // ---- L1（宿主簇 3）→ L2（宿主簇 4），带 COPIED ----
    w_u64(
        &mut img,
        usize::try_from(3 * cs).unwrap(),
        (4 * cs) | COPIED,
    );

    // ---- L2 + 数据簇 ----
    for (slot, virt) in allocated.iter().enumerate() {
        let host = FIXED_HOST_CLUSTERS + slot as u64;
        w_u64(
            &mut img,
            usize::try_from(4 * cs + virt * 8).unwrap(),
            (host * cs) | COPIED,
        );
        let src = usize::try_from(virt * cs).unwrap();
        let dst = usize::try_from(host * cs).unwrap();
        img[dst..dst + usize::try_from(cs).unwrap()]
            .copy_from_slice(&raw[src..src + usize::try_from(cs).unwrap()]);
    }

    img
}

/// 把头里的 `incompatible_features` / `crypt_method` 改成给定值（造拒写形态）。
pub fn qcow2_image_flagged(raw: &[u8], incompatible_features: u64, crypt_method: u32) -> Vec<u8> {
    let mut img = qcow2_image(raw);
    w_u64(&mut img, 72, incompatible_features);
    w_u32(&mut img, 32, crypt_method);
    img
}

/// 头里写上一个 backing 文件名（造「引用父镜像」的形态）。
pub fn qcow2_image_with_backing(raw: &[u8]) -> Vec<u8> {
    let mut img = qcow2_image(raw);
    let name = b"parent.qcow2";
    let off = 0x100usize; // 头（104 字节）之后、第一簇之内
    w_u64(&mut img, 8, off as u64);
    w_u32(&mut img, 16, name.len() as u32);
    img[off..off + name.len()].copy_from_slice(name);
    img
}

// ---------------------------------------------------------------- 小工具

/// `tempfile` 临时文件：返回目录守卫（必须保活）+ 路径。
pub fn write_temp(name: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(name);
    std::fs::write(&p, bytes).unwrap();
    (dir, p)
}

/// 文件的 sha256（「原档一字节未动」这类断言的统一口径）。
pub fn sha256_file(path: &std::path::Path) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    let bytes = std::fs::read(path).unwrap();
    Sha256::digest(bytes).into()
}

fn w_u16(img: &mut [u8], off: usize, v: u16) {
    img[off..off + 2].copy_from_slice(&v.to_be_bytes());
}

fn w_u32(img: &mut [u8], off: usize, v: u32) {
    img[off..off + 4].copy_from_slice(&v.to_be_bytes());
}

fn w_u64(img: &mut [u8], off: usize, v: u64) {
    img[off..off + 8].copy_from_slice(&v.to_be_bytes());
}
