//! hzip-fs-xfs：XFS 文件系统驱动（`hzip_vol::FsDriver` 实现）。
//!
//! # 与其它驱动的头号差别：XFS **大端**
//!
//! 磁盘上每个多字节字段都是大端（这个小家族里唯一的一个；ext4/Btrfs/NTFS 是小端）。
//! 但**校验和字段是小端**——内核 `xfs_end_cksum()` 返回 `~cpu_to_le32(crc)`，
//! 按大端读校验和会让**每一个真实文件系统都像坏的**（上游 README 记着这条曾真实出错）。
//!
//! # 契约与分工
//!
//! | 谁 | 干什么 |
//! |---|---|
//! | [`XfsDriver`] | `supports`（魔数 + 五条几何证据）、挂载策略（只读/可写/脏日志 ⇒ 只读）、能力集 |
//! | [`XfsVolume`] | 递归列全树、惰性读（`EntryId` ⇒ inode）、五类 `Change`、`flush`（干净卸载）、`audit`（上游 `check`） |
//! | [`error`] | 上游 `fs_xfs::Error` ⇒ [`hzip_vol::VolError`]（**拒绝理由即稳定键**） |
//!
//! # 读写能力（上游 `rust-fs-xfs` 0.14.1）
//!
//! 读：v4/v5，含脏日志**在内存里重放**（设备不动）；符号链接给出去的是**原样存储**
//! 的目标（可能是相对路径）。列出的元数据不撒谎：读不到的东西（realtime 文件的数据、
//! 名字表示不成的条目）按名拒绝或计数，不假装读到零/空。
//!
//! 写：五类 `Change` 里 `Replace`/`Delete`/`Append`/`Rename`/`MakeDir` **全部实现**
//! （走上游的日志化写路径：`truncate_to_zero`/`write`/`create_*`/`unlink_file`/
//! `remove_directory`/`rename_in_directory`/`change_attributes`）。**做不到的形态按名
//! 拒绝**，清单见下。
//!
//! # 未实现的清单（如实列出；键都不新增，复用 locale 表里既有的一族）
//!
//! | 做不到的事 | 表现 | 键 / 理由 |
//! |---|---|---|
//! | 脏日志卷**变可写** | 只读挂载 + 理由 | `vol.readonly.journal`。上游 0.14.1 只把日志重放进内存，没有「回写设备」的入口 ⇒ 即使 `FsOptions::replay_journal = true` 也兑现不了（记一条 warn，不假装做到了） |
//! | **跨目录**改名 | 按名拒绝 | `vol.err.dir-move`。上游只有 `rename_in_directory`（跨目录要记两个父目录并调整链接数） |
//! | 目录已**超出短格式**时改名 | 按名拒绝 | `vol.unsupported.dir-too-large`。上游的改名只覆盖短格式目录（大目录要记 buffer item）；本驱动**自己先判形态**，不按上游错误文本猜键 |
//! | realtime 文件、reflink 共享 extent 的 inode、数据叉是 B+tree 的 inode、v4 卷 | 按名拒绝 | `vol.err.fs-feature`（上游 `UnsupportedFeature`/`RealtimeDeviceAbsent`，细节进日志） |
//! | 单文件把数据叉 extent 预算写满（8 MiB × 21 条 = 168 MiB 是**上界**，碎片越多越小） | 按名拒绝（上游**在写任何字节之前**拒，故不半写） | `vol.err.fs-feature` |
//! | realtime 文件的数据（realtime 设备没接进来） | 读/写都按名拒绝 | `vol.err.fs-feature` |
//! | 名字**有损**（非 UTF-8）的条目**改名** | 按名拒绝 | `vol.err.entry-gone`。`Change::Rename` 的 `from` 只能是 `EntryName`（文本），没法回指原始字节名；`Replace`/`Delete` 按 id 走字节路径，**不受影响** |
//! | 建符号链接（`create_symlink` 已接好） | **无调用路径** | `hzip_format::Change` 的五类变体里没有这一条；`FsCaps::symlink` 如实报 `true`（卷的能力，不是本期的调用路径） |
//! | 改权限位/属主 | **无调用路径** | 同上：`Change` 里没有这一条（`caps.mode` 描述卷能力）；`change_attributes` 已用于写内容后的 **mtime 保真** |
//! | 外部日志（`sb_logstart == 0`）的卷 | 挂载即拒 | 上游 `log::inspect` 明确拒绝：那个设备它没有句柄，猜「日志干净」是唯一会静默丢数据的方向 |
//! | 可写挂载上的 `FsOptions::cache_blocks` | 不生效（如实说明） | 上游只有 `mount_with_cache`（只读）与 `mount_rw`（可写，无缓存参数）；可写挂载的缓存由卷层 `hzip_vol::VolOptions::cache_blocks` 承担 |
//! | `Change` 之外的新变体（`#[non_exhaustive]`） | 按名拒绝 | `vol.err.unknown-change` |
//!
//! # 夹具（`tests/common/`）
//!
//! 用上游 `mkfs`（v5）**现场造**，固定 UUID、块大小、容量与 AG 数 ⇒ 镜像是程序化的、
//! 不会随仓库里的一份二进制悄悄过期。**一处诚实的偏差**：上游 `mkfs` 与写路径都用
//! **系统时钟**给 inode 打时间戳（mkfs 给 3 个初始 inode、`create`/`write` 给新 inode），
//! 且 0.14.1 **没有时钟注入点**（ext4 那边的 `Runtime` 注入是 ext4 上游给的），
//! 故夹具**结构上逐字节确定**（超级块、AG 头、btree 根、日志首记录、文件内容），
//! 而 inode 的时间戳随运行时刻变——`tests/read.rs::fixture_is_structurally_deterministic`
//! 把范围钉死（两次构建的差异**恰好是 4 页**：inode chunk 的首页 + 内部日志的头 3 页），
//! 免得日后有人误以为整个镜像逐字节可复现，也免得差异真变大时没人发现。
//!
//! # 计划与验收
//!
//! `docs/tasks/T-IMG/` 的卷层契约（`T-IMG2` 的 `FsDriver`/`FsVolume`）+ 本文档。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod driver;
pub mod error;
pub mod volume;

pub use driver::{XfsDriver, is_xfs_superblock};
pub use volume::{NAME_MAX, XfsVolume};
