//! 卷镜像夹具（磁盘镜像 → 分区 → 文件系统；T-IMG 系列计划的测试基础设施）。
//!
//! 由 `vol` feature 门控：默认关闭，只有卷相关 crate（hzip-vol / hzip-fmt-raw /
//! hzip-fs-ext4 / 后续 qcow2、vmdk 驱动）在自己的 `dev-dependencies` 里开
//! `features = ["vol"]` 才编译。默认依赖树（02 §2.1：TK --> FMTA 合法边）不受影响。
//!
//! 三类夹具，逐层叠加，每层都能单独消费：
//!
//! 1. **块设备**（[`MemDevice`]）——所有 vol 测试共用的内存盘；既能当 base，
//!    也能当差异对照（`to_bytes` / `from_bytes`）。
//! 2. **分区表**（[`mbr`] / [`mbr_only`] / [`gpt_only`]）——只测分区层的镜像：
//!    字节级手写，CRC32 现场算（不引第三方 crc 依赖）。
//! 3. **文件系统与整盘镜像**（[`ext4_image`] / [`fat32_image`] / [`raw_bare`] /
//!    [`raw_with_mbr`] / [`write_fs_into`]）——文件系统镜像用上游构造器**现场造**，
//!    不是录下来的字节：上游换了布局，夹具跟着换，不会悄悄过期。
//!
//! # 红线
//!
//! - **确定性**：不读系统时钟、不取随机数。ext4 的 UUID、FAT 的卷 ID 与时间戳
//!   都是固定值（ext4 的时钟通过 `fs_ext4` 的 `Runtime` 注入）。同一组入参
//!   两次调用得到逐字节相同的镜像。
//! - **体量**：ext4 固定 8 MiB；FAT32 的 34 MiB 是格式本身的簇数下界
//!   （65525 簇 × 512 B，见 [`fat32_image`] 文档），不是夹具选择。
//! - **越界即错**：块设备的越界读写返回 `Err`，不 panic；目标缓冲装不下文件
//!   系统镜像时夹具直接断言失败（宁可炸测试，不可静默截断）。

// 夹具是**测试专用**代码：`vol` feature 门控、dev-only crate、不进任何发行二进制。
// 坏输入就该当场炸掉调用方的测试（少一个分区、盘装不下、上游 mkfs 失败），
// 而不是返回 `Err` 让调用方有机会忘了处理——故本模块豁免 `unwrap_used`/`expect_used`。
#![allow(clippy::unwrap_used, clippy::expect_used)]

pub mod ext4;
pub mod fat;
pub mod gpt;
pub mod mbr;
pub mod mem;
pub mod ntfs;
pub mod raw;

pub use ext4::{ext4_image, ext4_image_dirty};
pub use fat::fat32_image;
pub use gpt::{GptPartition, PartitionType, gpt_only};
pub use mbr::{mbr, mbr_only};
pub use mem::MemDevice;
pub use raw::{FixtureFs, raw_bare, raw_with_gpt, raw_with_mbr, write_fs_into};
