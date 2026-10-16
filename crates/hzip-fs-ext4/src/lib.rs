//! hzip-fs-ext4：ext2/ext3/ext4 文件系统驱动（`hzip_vol::FsDriver` 实现）。
//!
//! # 读与写
//!
//! 读：`entries()` 递归列目录树（元数据只读，`EntryInfo` 带 size/mtime/mode/符号链接
//! 目标）；`read_at()` 按 inode 惰性读（支持乱序/重复 → 契约红线 2）；`audit()` 走
//! 上游的结构审计（对应 `hzip t` 的「测试」语义）。
//!
//! 写：`ChangeSet` 的五类变更映射到上游 `Filesystem::apply_*`（create/unlink/mkdir/
//! rmdir/rename/replace_file_content/utimens），**日志化（JBD2）由上游负责**；
//! 脏卷（需重放日志）默认只读，用户显式确认后才 `replay_journal_if_dirty()` 再写。
//!
//! # 计划与验收
//!
//! 只读侧 `docs/tasks/T-IMG/T-IMG3-raw与ext4只读.md`；写侧 `T-IMG5-ext4读写与设备链.md`。
//!
//! # 上游类型不外泄
//!
//! `fs_ext4` 只在本 crate 内部出现：对外只有 `hzip_vol` 的 `FsDriver`/`FsVolume`
//! 契约与本地枚举。设备经 `fs_ext4::fs_core_bridge::CoreDevice` 桥过上游 trait
//! （上游用**它自己的** `BlockDevice`，`Arc<dyn hzip_vol::BlockDevice>` 是唯一接缝）。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod driver;
mod error;
pub mod volume;

pub use driver::Ext4Driver;
pub use volume::Ext4Volume;
