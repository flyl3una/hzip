//! hzip-fmt-raw：`raw` 磁盘镜像容器（含 MBR/GPT 与裸文件系统卷）。
//!
//! # 与其它镜像格式的根本差别
//!
//! raw **没有魔数**——它只是一段字节，是不是磁盘镜像要靠**内容结构**判定
//! （MBR 的 `55 AA` + 表项合理 / GPT 的 `EFI PART` / 偏移 0 的 FAT·NTFS·exFAT 签名 /
//! 偏移 160 的 ext4 超级块）。故本 handler 实现 `FormatHandler::probe`（内容探针档，
//! 置信度 `Medium`/`High`），扩展名（`.raw/.img/.dd`）只作回退。
//!
//! 判定通过后，镜像内部结构（分区表、文件系统）由 [`hzip_vol`] 的分区层与 FS 驱动
//! 继续识别：raw 自身只等于「整文件就是一个块设备」。
//!
//! # 模块
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`probe`] | 内容探针（三种强证据；不猜） |
//! | [`volume`] | `Volume` 实现：分区表探测 + 每卷挂载文件系统 |
//! | [`handler`] | `FormatHandler`：签名/能力/`open_reader`/`open_editor` |
//!
//! # 计划与验收
//!
//! `docs/tasks/T-IMG/T-IMG3-raw与ext4只读.md`（探针 + 夹具）、`T-IMG4-核心接入与CLI只读.md`
//! （注册与扩展名表）。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod handler;
pub mod probe;
pub mod volume;

pub use handler::RawHandler;
pub use probe::{PROBE_HEAD_BYTES, raw_probe};
pub use volume::RawVolume;
