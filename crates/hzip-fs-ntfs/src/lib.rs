//! hzip-fs-ntfs：NTFS 文件系统驱动（`hzip_vol::FsDriver` 实现，读 + 写）。
//!
//! # 边界（上游 `rust-fs-ntfs` 决定，均已在能力矩阵登记）
//!
//! - **拒写**（判定序见计划 §2.3）：休眠卷（Windows 快速启动残留，**绝不清标志**）、
//!   `$LogFile` 含事务（除显式 `replay_journal`）、dirty 标志、只读设备/只读设置；
//! - **目录规模**：上游每目录最多 64 个索引块（常驻 `$Bitmap:$I30` = 8 字节 ⇒ 64 位，
//!   见 [`INDEX_BLOCKS_MAX`]）⇒ **写前拒绝**（`vol.unsupported.dir-too-large`，不半写）；
//! - **变换内容**：WOF（Compact OS）压缩、EFS 加密、LZNT1 压缩、稀疏 ⇒ 条目照列但
//!   标记只读，内容读/写在动手前拒（`vol.unsupported.compressed`）；
//! - **名字**：NTFS 是 UTF-16；转 UTF-8 用 `from_utf16_lossy`，非法代理对记 `U+FFFD`
//!   但**不丢条目**（[`NtfsVolume::lossy_name_count`]）。
//!
//! # 与设备层的接法（计划 §2.4 的决策记录）
//!
//! 上游是**基于路径、无状态**的（每次操作重开镜像），而我们的设备链末端可能是
//! 覆盖层（模式 B）而不是真文件。故本驱动在挂载时把 [`BlockDevice`] 的字节**物化**
//! 到临时镜像文件（`hzip-spool-` 前缀 + `tempfile`，由 [`hzip_vol::materialize`] 落盘，
//! 该前缀是启动期孤儿回收的识别依据），随后所有上游调用都走该路径。
//!
//! 于是写路径的形状是：
//!
//! ```text
//! apply(Change) ──► 临时镜像（只在临时文件里改）
//! flush()       ──► 与设备逐块比对，只把**不同**的块 write_at 回设备 + device.flush()
//! ```
//!
//! 提交前设备一个字节不动（取消即丢，与覆盖写语义等价）；**调用方必须在提交前调用
//! [`FsVolume::flush`]**，否则改动只留在临时镜像里、随 `Drop` 一起消失（`Drop` 不做
//! 回放，那是「取消即丢」的实现）。代价是一份完整镜像的临时副本（与 `Sink::File`
//! 路径同量级），已在能力矩阵里标注。
//!
//! # 与计划的三处**实测偏离**（都是上游事实，不是偷工）
//!
//! 1. **用上游 `read::` / `write::` / `fsck::` 的公开路径函数，而不是 `facade::Filesystem`**：
//!    `facade::Error` 只有一条 `String`——它把 `Kind` 丢了，于是 `ENOSPC`（`vol.err.no-space`）、
//!    `EPERM`、`ENOTEMPTY`（`vol.err.dir-not-empty`）都只能靠猜消息文本来分辨，而界面要的是
//!    **稳定键**。facade 本身就是这批公开路径函数的薄包装（同一个「每次调用重开镜像」语义，
//!    见上游 `facade.rs` 的 `write::x(...)` 调用），故语义不变、错误类别保住；挂载校验走
//!    `read::read_volume_info`，写挂载准备走 `fsck::prepare_rw_mount[_replaying]_io` +
//!    `fsck::upgrade_volume_version`——与 `facade::mount_rw_as` 逐句对应。
//! 2. **dirty 标志 ⇒ 只读**（计划 §2.3 ③ 写的是「可写 + 提示」）：上游 `prepare_rw_mount`
//!    对 dirty 卷**直接拒**（dirty 意味着「要求一次全卷检查」，上游不做那件事就不写盘），
//!    故这里如实给 `ReadOnlyReason::Dirty` + `audit()` 里的一条提示，不绕过上游。
//! 3. **`EntryTree` 没有 `warnings` 字段**（计划 §2.2 要求把异常名计数进去）：接口已冻结，
//!    本 crate 不改 `hzip-vol`。次数改由 [`NtfsVolume::lossy_name_count`] 暴露，并进
//!    `audit()` 的 findings。
//!
//! 另有一条**降级**口径（不是偏离，是契约的延续）：请求可写但上游准备阶段拒了（竞态或更坏的
//! 状态：脏卷/不可读日志/无法完整重放），挂载**不会失败**——降级为只读并把理由放进
//! `caps().read_only_reason`，因为「打不开」比「只读但说得出为什么」差得多。
//!
//! # 已知代价（写进能力矩阵，不留桩）
//!
//! - **`Replace`/`Append` 的内容必然在内存里放一份**：上游的整段内容写
//!   （`write_file_contents`）只吃 `&[u8]`，没有流式替换 API ⇒ 大文件的内存开销 = 文件大小；
//! - **`audit()` 是上游 fsck 的只读子集**（`fsck::check_io`：`$MFTMirr` 对照、在用记录的
//!   属性链、`$Bitmap` 与 run list 对账），**不是** Windows `chkdsk`，findings 里如实标注。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod driver;
pub mod names;
mod volume;

pub use driver::{DRIVER_ID, NtfsDriver};
pub use volume::{INDEX_BLOCKS_MAX, NtfsVolume};
