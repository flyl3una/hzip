//! hzip-vol：卷层框架（磁盘镜像 → 分区 → 文件系统）。
//!
//! # 职责
//!
//! 用**一个块设备接口**（[`fs_core`] 的 `BlockRead`/`BlockDevice`）把三层接起来：
//! ① 镜像容器（raw/qcow2/vmdk，各一个 `hzip-fmt-*` 实现）→ ② 分区表（GPT/MBR，
//! 每卷切一个子设备）→ ③ 文件系统驱动（ext4/NTFS/FAT，各一个 `hzip-fs-*` 实现）。
//! 向上只暴露 `hzip-format` 的既有契约（`ArchiveReader`/`ArchiveEditor`），
//! 于是卷内文件与原档案走**同一条**浏览/预览/提取/编辑链路。
//!
//! # 写回与安全性
//!
//! 默认**覆盖写**：改动先落 [`OverlayDevice`]（读穿透 + 脏块表），提交时只把脏块
//! 回放到原文件——提交前原文件一个字节不动，取消即丢。设置页可切「就地写」/「只读」。
//!
//! # 上游类型不外泄（硬约束）
//!
//! 本 crate 的实现依赖 `rust-fs-core` / `rust-disk-partitions` / `rust-img-*` /
//! `rust-fs-*`，但**它们没有一个是本 crate 或上层公开类型的一部分**：上游 0.x 且
//! 刚改过名（`am-fs-*` → `rust-fs-*`），换实现时只应改这一层的适配。故分区表种类、
//! 文件系统种类等都用本地枚举（[`TableKind`]、[`FsKind`]）表达——只有
//! [`VolumeHandler::open`]/[`FsDriver::mount`] 的参数用到 `Arc<dyn BlockDevice>`，
//! 那是整个家族的公共接缝，值得。
//!
//! # 计划与接口冻结
//!
//! 接口在该计划文件 §2.1 冻结，落地任务与验收清单同文件：`docs/tasks/T-IMG/T-IMG2-vol框架.md`。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hzip_format::{Change, EntryId, EntryInfo};
use hzip_progress::CancellationToken;

pub mod device;
pub mod editor;
pub mod error;
pub mod materialize;
pub mod overlay;
pub mod partition;
pub mod reader;
pub mod registry;

pub use error::{ReadOnlyReason, VolError, VolResult};
pub use fs_core::{BlockDevice, BlockRead};
pub use overlay::{OVERLAY_BLOCK, OverlayDevice};

/// 分区在**本卷内的序号**（0 起；显示时 +1）。
pub type PartitionId = usize;

// ---------------------------------------------------------------- 数据类

/// 分区表种类（本地枚举；上游 `disk_partitions::TableKind` 不外泄）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableKind {
    /// GUID 分区表。
    Gpt,
    /// 主引导记录（含扩展分区的容器项）。
    Mbr,
}

/// 文件系统种类（本地枚举；上游 `disk_partitions::FsKind` 不外泄）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FsKind {
    /// ext2/ext3/ext4（共用一族布局，特性位区分）。
    Ext4,
    /// NTFS。
    Ntfs,
    /// exFAT（本期无驱动 ⇒ 只读「未知卷」）。
    ExFat,
    /// FAT12/16/32。
    Fat,
    /// XFS（v4/v5；上游 `rust-fs-xfs`：读 + 覆盖写/建删改名走日志）。魔数 `XFSB`（**大端**）。
    Xfs,
    /// 其它被嗅探到但本期无驱动的文件系统（ISO9660/SquashFS/btrfs/APFS…）。
    Other,
}

impl FsKind {
    /// 显示名（类型列与日志用；不翻译——它是数据，不是界面词）。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ext4 => "ext4",
            Self::Ntfs => "NTFS",
            Self::ExFat => "exFAT",
            Self::Fat => "FAT",
            Self::Xfs => "XFS",
            Self::Other => "?",
        }
    }
}

/// 虚拟几何（容量、逻辑扇区、分区表、稀疏/backing 提示）。
#[derive(Debug, Clone)]
pub struct Geometry {
    /// 虚拟容量（字节）。
    pub virtual_size: u64,
    /// 逻辑扇区字节数（GPT/MBR 的 LBA 单位；通常 512）。
    pub sector_size: u32,
    /// 分区表种类；`None` = 无分区表（整卷就是一个文件系统）。
    pub table: Option<TableKind>,
    /// 镜像是否稀疏（qcow2/vmdk 为真；raw 为假）。
    pub is_sparse: bool,
    /// 是否有 backing/父镜像（qcow2 backing 链、VMDK 快照 delta）。
    pub has_backing: bool,
}

/// 卷的能力与理由。
#[derive(Debug, Clone)]
pub struct VolCaps {
    /// 可写（镜像层允许写，且卷自身没有脏/加密等硬伤）。
    pub writable: bool,
    /// 提交可**只回放脏块**（源是常规文件）；源是流时只能整量重写。
    pub in_place: bool,
    /// 不可写的理由（`writable == false` 时必有值）。
    pub read_only_reason: Option<ReadOnlyReason>,
    /// 能力**提示**（不阻止操作，只让用户知道代价）：例 `vol.warn.backing-copyup`。
    pub warnings: Vec<&'static str>,
}

/// 一个分区（显示与切片所需的一切）。
#[derive(Debug, Clone)]
pub struct PartitionView {
    /// 本卷内序号（0 起）。
    pub id: PartitionId,
    /// 起始字节。
    pub start: u64,
    /// 长度（字节）。
    pub len: u64,
    /// MBR 类型字节（GPT 分区为 `None`）。
    pub type_byte: Option<u8>,
    /// **真实分区标签**：GPT 的分区名（非空时）；MBR 没有标签概念 ⇒ `None`。
    ///
    /// 与 [`Self::type_name`] 严格分开：MBR 分区只有「类型描述」（`Linux`），
    /// 那不是标签——把类型当标签会导致界面把「Linux」显示成分区名（用户口径
    /// 要的是「有 label 用 label，没有则「分区 N」」）。
    pub label: Option<String>,
    /// 类型描述（界面类型列用）：GPT 的类型 GUID 名、MBR 的类型字节名、整卷为 `Volume`。
    pub type_name: String,
    /// 嗅探到的文件系统；`None` = 未识别（显示为「未知卷」，只读）。
    pub fs: Option<FsKind>,
}

impl PartitionView {
    /// 显示名（诊断/日志用）：`1. root (ext4)`；无标签时用类型描述。
    #[must_use]
    pub fn display(&self) -> String {
        let head = self
            .label
            .clone()
            .unwrap_or_else(|| format!("Partition {}", self.id + 1));
        match self.fs {
            Some(fs) => format!("{}. {} ({})", self.id + 1, head, fs.label()),
            None => format!("{}. {} ({})", self.id + 1, head, self.type_name),
        }
    }
}

/// 文件系统能力（写侧细粒度：能删不等于能改名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsCaps {
    /// 可写文件内容。
    pub write: bool,
    /// 可删。
    pub delete: bool,
    /// 可改名。
    pub rename: bool,
    /// 可建目录。
    pub mkdir: bool,
    /// 可建符号链接。
    pub symlink: bool,
    /// 可写时间戳。
    pub times: bool,
    /// 可写权限位（FAT 为假）。
    pub mode: bool,
    /// 名字长度上限（字节；FAT LFN 255、ext4 255、NTFS 255）。
    pub name_max: usize,
    /// 只读理由（`write == false` 时应有值）。
    pub read_only_reason: Option<ReadOnlyReason>,
}

impl FsCaps {
    /// 只读（不可写）的能力集，带理由。
    #[must_use]
    pub fn read_only(reason: ReadOnlyReason) -> Self {
        Self {
            write: false,
            delete: false,
            rename: false,
            mkdir: false,
            symlink: false,
            times: false,
            mode: false,
            name_max: 255,
            read_only_reason: Some(reason),
        }
    }
}

/// 列表上限（防恶意/损坏镜像把内存吃光）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryLimit {
    /// 最多列出多少条目（超出 ⇒ `EntryTree::truncated`）。
    pub max_entries: usize,
    /// 目录递归深度上限（防循环）。
    pub max_depth: usize,
}

impl Default for EntryLimit {
    fn default() -> Self {
        // 10 万条 ≈ 20 MiB 的 `EntryInfo`（每条约 200 B）；深度 64 足够真实镜像。
        Self {
            max_entries: 100_000,
            max_depth: 64,
        }
    }
}

/// 一次列表的结果（含「被截断」与「跳过」两类如实标注）。
#[derive(Debug, Clone, Default)]
pub struct EntryTree {
    /// 扁平条目表（含目录条目；名字已用 `/` 规范化、无前导 `./`）。
    pub entries: Vec<EntryInfo>,
    /// 因 [`EntryLimit`] 截断。
    pub truncated: bool,
    /// 被跳过的条目数（设备/管道/套接字等不列出的类型）。
    pub skipped: u64,
}

impl EntryTree {
    /// 按名字找条目（测试与内部映射用）。
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&EntryInfo> {
        self.entries.iter().find(|e| e.name.as_str() == name)
    }
}

/// 一致性自检结果（提交前调用；对应 `hzip t` 的「测试」语义）。
#[derive(Debug, Clone, Default)]
pub struct AuditSummary {
    /// 发现的问题（空 = 干净）。
    pub findings: Vec<String>,
    /// 该驱动未实现自检（如实标注，不冒充「检查过了」）。
    pub skipped: bool,
}

impl AuditSummary {
    /// 干净。
    #[must_use]
    pub fn clean() -> Self {
        Self::default()
    }

    /// 未实现自检（不撒谎：调用方据此在 UI 上写「未做自检」）。
    #[must_use]
    pub fn not_implemented(reason: &str) -> Self {
        Self {
            findings: vec![reason.to_string()],
            skipped: true,
        }
    }

    /// 是否干净（未实现自检**不算**干净）。
    #[must_use]
    pub fn is_clean(&self) -> bool {
        !self.skipped && self.findings.is_empty()
    }
}

/// 卷容器的打开选项。
#[derive(Debug, Clone)]
pub struct VolOptions {
    /// 允许向上游要 r/w（false ⇒ 一律只读打开；写模式开关选「只读」时用）。
    pub writable: bool,
    /// LRU 读缓存块数（0 = 不装；镜像元数据遍历很吃缓存）。
    pub cache_blocks: usize,
    /// spool 策略（见 [`SpoolPolicy`]）。
    pub spool: SpoolPolicy,
    /// 文件系统驱动注册表：容器实现 `Volume::open_fs` 时按它挑驱动挂载。
    ///
    /// 由 core 在启动时填好（注册各 `hzip-fs-*` 驱动）；默认空表 ⇒ 一切卷都显示为
    /// 「未知卷（只读）」，**不 panic、不静默成空树**（`open_fs` 返回 `Ok(None)`）。
    pub fs_registry: std::sync::Arc<crate::registry::FsRegistry>,
    /// 文件系统挂载选项（可写/是否允许重放日志/读缓存）。
    ///
    /// 与 [`Self::writable`] 的分工：本字段管**文件系统层**（脏卷策略在这里生效），
    /// `writable` 管**镜像层**（设备链末端）。二者由 core 一起设置。
    pub fs: FsOptions,
}

impl Default for VolOptions {
    fn default() -> Self {
        Self {
            writable: true,
            cache_blocks: 8192,
            spool: SpoolPolicy::default(),
            fs_registry: std::sync::Arc::new(crate::registry::FsRegistry::new()),
            fs: FsOptions::default(),
        }
    }
}

/// 流 → 临时文件的策略（由 core 依 `services::space` 的口径填）。
#[derive(Debug, Clone)]
pub struct SpoolPolicy {
    /// 临时目录。
    pub dir: PathBuf,
    /// 文件名前缀；**必须** `hzip-spool-`（启动期孤儿回收按该前缀识别）。
    pub prefix: &'static str,
    /// 允许的最大 spool 字节（`None` = 只受磁盘可用空间约束）。
    pub max_bytes: Option<u64>,
}

impl Default for SpoolPolicy {
    fn default() -> Self {
        Self {
            dir: hzip_platform::dirs::temp_root(),
            prefix: crate::materialize::SPOOL_PREFIX,
            max_bytes: None,
        }
    }
}

/// 文件系统打开选项。
#[derive(Debug, Clone, Default)]
pub struct FsOptions {
    /// 请求可写（驱动可据脏卷策略降级为只读）。
    pub writable: bool,
    /// 卷内有待重放日志时，是否允许「先重放再写」（默认 false ⇒ 只读 + 理由）。
    pub replay_journal: bool,
    /// 读缓存块数。
    pub cache_blocks: usize,
}

// ---------------------------------------------------------------- 契约

/// 卷容器：镜像文件 → 一个可随机读写的虚拟块设备。
pub trait VolumeHandler: Send + Sync {
    /// 容器 id（`"qcow2"` / `"vmdk"` / `"raw"`；与 `hzip-format` 的 [`FormatId`] 同名）。
    ///
    /// [`FormatId`]: hzip_error::FormatId
    fn id(&self) -> &'static str;

    /// 打开卷（设备由 [`crate::device::Chain`] 按写模式决定末端）。
    fn open(&self, dev: Arc<dyn BlockDevice>, opts: &VolOptions) -> VolResult<Box<dyn Volume>>;
}

/// 一个镜像容器打开后的卷视图。
pub trait Volume: Send {
    /// 虚拟几何。
    fn geometry(&self) -> Geometry;

    /// 分区表；**空 = 无分区表**（整卷即一个文件系统卷，`open_fs(None)` 取它）。
    fn partitions(&self) -> VolResult<Vec<PartitionView>>;

    /// 打开某分区的文件系统；`Ok(None)` = 该卷的文件系统本期没有驱动/不认识。
    fn open_fs(&self, part: Option<PartitionId>) -> VolResult<Option<Box<dyn FsVolume>>>;

    /// 能力与理由。
    fn caps(&self) -> VolCaps;

    /// 落盘（镜像层 flush）。
    fn flush(&mut self) -> VolResult<()>;
}

/// 文件系统驱动（ext4 / NTFS / FAT 各一个实现；`crate::registry` 按 [`FsKind`] 挑）。
pub trait FsDriver: Send + Sync {
    /// 驱动 id（`"ext4"` / `"ntfs"` / `"fat"`）。
    fn id(&self) -> &'static str;

    /// 该驱动是否接这个卷（嗅探结论 + 头部复核，二者都要对）。
    fn supports(&self, kind: FsKind, head: &[u8]) -> bool;

    /// 挂载。
    fn mount(&self, dev: Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<Box<dyn FsVolume>>;
}

/// 已挂载的文件系统视图（读 + 变更）。
pub trait FsVolume: Send + Sync {
    /// 驱动 id。
    fn fs_id(&self) -> &'static str;

    /// 能力（写侧细粒度）。
    fn caps(&self) -> FsCaps;

    /// 全树（只读元数据）。实现负责 `limit` 与 `cancel`。
    fn entries(&self, limit: &EntryLimit, cancel: &CancellationToken) -> VolResult<EntryTree>;

    /// 惰性读：任意偏移、乱序、重复可用（对应 `ArchiveReader` 红线 2）。
    fn read_at(&self, id: EntryId, offset: u64, buf: &mut [u8]) -> VolResult<usize>;

    /// 条目大小（字节）。
    fn size_of(&self, id: EntryId) -> VolResult<u64>;

    /// 应用一条变更（`local` = `Change::Replace`/`Append` 的本地文件）。
    fn apply(
        &mut self,
        change: &Change,
        local: Option<&Path>,
        cancel: &CancellationToken,
    ) -> VolResult<()>;

    /// 落盘。
    fn flush(&mut self) -> VolResult<()>;

    /// 提交前一致性自检。
    fn audit(&self) -> VolResult<AuditSummary>;
}
