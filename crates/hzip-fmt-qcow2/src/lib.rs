//! hzip-fmt-qcow2：QCOW2 磁盘镜像容器（读+写）。
//!
//! # 能力边界（由上游 `rust-img-qcow2` 决定）
//!
//! | 方向 | 支持 |
//! |---|---|
//! | 读 | v2/v3、未压缩 / zlib / zstd 压缩簇、稀疏与 v3 零簇、backing 链（**仅按路径**） |
//! | 写 | 簇分配 + refcount 维护 + 快照共享簇 copy-up（`open_rw_on_device`） |
//! | 拒写并给稳定键 | dirty / corrupt ⇒ 只读；加密 ⇒ 理由 `Encrypted`；外部数据文件 / 扩展 L2 / backing ⇒ 按名字拒绝 |
//!
//! 判定序与理由见 [`volume`] 的模块文档。
//!
//! # 与其它容器的根本差别
//!
//! qcow2 的元数据（L1/L2/refcount）写在**宿主文件偏移**上，故 `hzip-vol` 的覆盖层
//! 必须挂在 qcow2 **下面**（`FileDevice → OverlayDevice → Qcow2Reader`）——提交时
//! 回放的是宿主脏块。这正是「40 GB 镜像改一个文件只写几 KB」的来源，也是本 crate
//! 比 `raw` 多一层「虚拟盘设备」的原因。
//!
//! # 模块
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`probe`] | 魔数探针（`QFI\xfb` + 版本 2/3，**大端**） |
//! | [`volume`] | `Volume` 实现：判定序 + 虚拟盘 + 分区表探测 + 每卷挂载文件系统 |
//! | [`handler`] | `FormatHandler`/`VolumeHandler`：签名/能力/`open_reader`/`open_editor` |
//!
//! # 计划与验收
//!
//! `docs/tasks/T-IMG/T-IMG7-qcow2容器.md`（探针、夹具、拒写理由、读/写用例）。
//!
//! ⚠ 该文档的交付物表把夹具放在 `hzip-format-testkit`，并把「backing ⇒ 可写 + 提示」
//! 写成要求；本轮的实施约束是「夹具放各自 crate 的 `tests/common/`」，且
//! `VolumeHandler::open` 拿不到路径 ⇒ backing 只能按名字拒绝。两处偏离都在
//! [`volume`] 的模块文档里写了原因。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod handler;
pub mod probe;
pub mod volume;

pub use handler::Qcow2Handler;
pub use probe::{PROBE_HEAD_BYTES, qcow2_probe};
pub use volume::Qcow2Volume;
