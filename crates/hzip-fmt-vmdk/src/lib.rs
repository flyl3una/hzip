//! hzip-fmt-vmdk：VMware VMDK 磁盘镜像容器（`hzip_vol::VolumeHandler` 实现）。
//!
//! # 能力边界（上游 `rust-img-vmdk` 决定）
//!
//! 读：`monolithicSparse` 与 `streamOptimized`（DEFLATE grain，只读）。
//! 写：仅 `monolithicSparse`（分配 grain、维护冗余 GD、content id、`uncleanShutdown`）。
//! 其余布局**按 create type / 魔数报名字拒绝**，绝不误读：`monolithicFlat`、
//! `twoGbMaxExtent{Sparse,Flat}`、ESXi `vmfsSparse`（`COWD`）、快照 delta/linked clone。
//!
//! 注意 VMDK 头是**小端**（sparse extent 魔数 `KDMV`），与 qcow2 的大端魔数相反。
//!
//! # 模块
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`probe`] | 头部探针（`KDMV` + 字段合理性；不猜） |
//! | [`volume`] | `Volume` 实现：**先判布局再要写权限** + 分区表探测 + 每卷挂载文件系统 |
//! | [`handler`] | `FormatHandler`：签名/能力/`open_reader`/`open_editor` |
//!
//! # 计划与验收
//!
//! `docs/tasks/T-IMG/T-IMG8-vmdk容器.md`（探针、夹具、拒写口径、`vmdk.zip` 场景）。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod handler;
pub mod probe;
pub mod volume;

pub use handler::VmdkHandler;
pub use probe::{PROBE_HEAD_BYTES, vmdk_probe};
pub use volume::{Layout, VmdkVolume, classify_layout};
