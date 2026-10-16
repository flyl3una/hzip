//! ext4 文件系统镜像夹具：用上游 `mkfs` + `Filesystem` **现场造**（不录字节）。
//!
//! # 为什么现场造
//!
//! 录一份 `.img` 到仓库里的代价是它在若干个月后**悄悄过期**：上游换了布局、
//! 特性位或校验方式，夹具还是那份老字节，测试于是测的是历史而不是现在。现场造
//! 的另一个好处是**参数化**——目录树与内容由调用方给，用例不必为每个场景存一份镜像。
//!
//! # 确定性
//!
//! 三处非确定源都被钉死：
//!
//! | 源 | 处理 |
//! |---|---|
//! | 卷 UUID | `mkfs` 传固定 16 字节（`VOLUME_UUID`） |
//! | inode 时间戳 | 挂载注入 [`FixedRuntime`]，秒数固定 |
//! | inode generation | 固定基准 + 单调计数器（可重放，不取随机数） |
//!
//! 由此同一组入参两次调用得到**逐字节相同**的镜像，可直接做黄金样本比较。
//!
//! # 与设备层的接法（后续驱动照抄）
//!
//! 上游 ext4 用的是**它自己的** `BlockDevice` trait，故 `fs_core` 侧的设备必须先经
//! `fs_core_bridge::CoreDevice` 桥过去；反向（ext4 设备喂给分区探测/容器）不需要桥。
//! 分区镜像因此长这样：`MemDevice → CoreDevice → Filesystem`。

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use fs_ext4::fs_core_bridge::CoreDevice;
use fs_ext4::runtime::Runtime;
use fs_ext4::{Filesystem, mkfs};

use super::MemDevice;

/// 夹具 ext4 镜像容量（8 MiB；元数据 ≈ 200 KiB，其余全归数据区）。
pub const EXT4_IMAGE_BYTES: u64 = 8 << 20;

/// 夹具 ext4 块大小（4 KiB；与真实发行版默认一致）。
pub const EXT4_BLOCK_SIZE: u32 = 4096;

/// 固定挂载时钟（2023-11-14T22:13:20Z）——确定性夹具的一部分。
pub const FIXED_UNIX_SECONDS: i64 = 1_700_000_000;

/// 固定卷 UUID（= ASCII `HZIP-fixture-ext4`）。
pub const VOLUME_UUID: [u8; 16] = *b"HZIPfixture-ext4";

/// 固定时钟 + 可重放 generation 的 [`Runtime`]。
#[derive(Debug, Default)]
pub struct FixedRuntime {
    generation: AtomicU32,
}

impl Runtime for FixedRuntime {
    fn now_unix_seconds(&self) -> i64 {
        FIXED_UNIX_SECONDS
    }

    fn next_inode_generation(&self) -> u32 {
        // 从 1 起单调递增：与「避免立刻复用」的上游约束一致，且可重放。
        1 + self.generation.fetch_add(1, Ordering::Relaxed)
    }
}

/// 造一个含给定文件的 ext4 镜像（干净卷）。
#[must_use]
pub fn ext4_image(files: &[(&str, &[u8])]) -> Vec<u8> {
    build(files, false)
}

/// 同上，但把超级块的「已挂载标志」清掉（`s_state` 的 `EXT4_VALID_FS` 位 = 0），
/// 用来测「脏卷默认只读、需显式重放/核查」这条策略。
#[must_use]
pub fn ext4_image_dirty(files: &[(&str, &[u8])]) -> Vec<u8> {
    build(files, true)
}

fn build(files: &[(&str, &[u8])], dirty: bool) -> Vec<u8> {
    let mem = MemDevice::new(EXT4_IMAGE_BYTES);
    {
        let dev = CoreDevice::new(mem.clone());
        mkfs::format_filesystem(
            &dev,
            Some("HZTEST"),
            Some(VOLUME_UUID),
            EXT4_IMAGE_BYTES,
            EXT4_BLOCK_SIZE,
        )
        .expect("夹具：ext4 格式化应成功");
        let fs = Filesystem::mount_with_runtime(Arc::new(dev), Arc::new(FixedRuntime::default()))
            .expect("夹具：刚格式化的 ext4 应可挂载");
        for (path, data) in files {
            put_file(&fs, path, data).expect("夹具：写文件应成功");
        }
        fs.finish().expect("夹具：收尾（flush）应成功");
    }
    let mut bytes = mem.to_bytes();
    if dirty {
        mark_dirty(&mut bytes);
    }
    bytes
}

/// 建父目录（逐级）+ 建文件 + 写内容。
fn put_file(fs: &Filesystem, path: &str, data: &[u8]) -> fs_ext4::Result<()> {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    assert!(!segs.is_empty(), "夹具：文件路径不能为空（{path}）");
    let mut cur = String::new();
    for seg in &segs[..segs.len() - 1] {
        cur.push('/');
        cur.push_str(seg);
        if fs.lookup_path_bytes(cur.as_bytes()).is_err() {
            fs.apply_mkdir(&cur, 0o755)?;
        }
    }
    let full = format!("/{}", segs.join("/"));
    fs.apply_create(&full, 0o644)?;
    fs.apply_replace_file_content(&full, data)?;
    Ok(())
}

/// 超级块 `s_state`（偏移 `1024 + 58`，u16 LE）清掉 `EXT4_VALID_FS`（`0x0001`），
/// **并重算超级块校验和**。
///
/// ext4 的口径是「**干净卸载**才置 VALID_FS」⇒ 位清零即「上次没干净卸载，需核查」。
///
/// # 为什么必须重算校验和（2026-10-16 子代理实测抓到的夹具缺陷）
///
/// 本夹具格式化的卷开着 `metadata_csum`（`s_feature_ro_compat` 含 `0x400`）⇒ 超级块
/// 末尾 4 字节（`1024 + 0x3FC`）是它自己的 crc32c（种子里外各取反一次，见
/// `fs_ext4::checksum::linux_crc32c`）。只改 `s_state` 不重算 ⇒ 上游 `mount_lazy`
/// 直接 `Error::BadChecksum("superblock checksum mismatch")`，**根本挂不上**，
/// 于是「脏卷 ⇒ 默认拒写」这条策略压根测不到（夹具假红/假绿都危险）。
fn mark_dirty(bytes: &mut [u8]) {
    assert!(bytes.len() >= 1082, "夹具：镜像太小，放不下超级块");
    let s_state = 1024 + 58;
    bytes[s_state..s_state + 2].copy_from_slice(&0u16.to_le_bytes());
    // 校验和覆盖超级块前 0x3FC 字节（含刚改的 s_state），结果存末尾 4 字节
    let csum = fs_ext4::checksum::linux_crc32c(!0, &bytes[1024..1024 + 0x3FC]);
    bytes[1024 + 0x3FC..1024 + 0x400].copy_from_slice(&csum.to_le_bytes());
}
