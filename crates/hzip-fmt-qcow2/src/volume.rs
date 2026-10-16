//! `Qcow2Volume`：qcow2 镜像的卷视图（`hzip_vol::Volume` 实现）。
//!
//! # 谁在读虚拟盘
//!
//! 设备链（[`hzip_vol::device::Chain`]）交给容器的，是**宿主文件**上的块设备，
//! 覆盖层已经在链里。qcow2 自己的元数据（L1/L2/refcount）也写在**宿主偏移**上，
//! 所以覆盖层必须挂在 qcow2 **下面**（`FileDevice → OverlayDevice → Qcow2Reader`）：
//! 提交时回放的是宿主脏块，改一个文件只写几 KB。
//!
//! 于是本卷比 `raw` 多一层：上游 `Qcow2Reader` 实现 `BlockDevice`，它就是**虚拟盘**。
//! 分区表探测、分区切片、文件系统挂载全部挂在虚拟盘上，而不是宿主设备上。
//!
//! # 打开时的判定序（顺序即优先级，全部落在 [`Qcow2Volume::open`]）
//!
//! | # | 条件（读自 qcow2 头） | 结果 |
//! |---|---|---|
//! | ① | `incompatible_features` 含 `EXTERNAL_DATA_FILE` | 拒绝 `vol.unsupported.external-data` |
//! | ② | `incompatible_features` 含 `EXTENDED_L2` | 拒绝 `vol.unsupported.ext-l2` |
//! | ③ | `crypt_method != 0` | 拒绝，理由 [`ReadOnlyReason::Encrypted`] |
//! | ④ | 头里带 backing 文件 | 拒绝 `vol.unsupported.backing-file`（理由见下） |
//! | ⑤ | dirty / corrupt 位 | **只读**，理由 [`ReadOnlyReason::Dirty`]（不给清标记入口） |
//!
//! ## ①② 为什么要自己先判一遍
//!
//! 上游 `Header::check_supported` 会把这两类折成一个自由文本的 `Unsupported`，
//! 另加「未知 incompatible 位」等我们**不打算分辨**的情况。判定序要求这两类各有
//! **稳定键**（UI/CLI 按键查 locale），故先用上游的解析器读出头部（不抄偏移量），
//! 再按位拒绝——文本匹配上游错误消息是更脆的做法。
//!
//! ## ③ 为什么「加密」是拒绝而不是「打开成只读」
//!
//! 计划 T-IMG7 写的是「加密 ⇒ 只读」。实际上游对 `crypt_method != 0` 在
//! `check_supported` 就拒绝，**读路径也过不去**（见 `open_inner`）——我们没有任何
//! 解密实现，给不出「能读」的卷。硬约束是不假装支持，故如实报出理由键
//! `vol.readonly.encrypted`（枚举里就有这一档「加密镜像/卷」）。
//!
//! ## ④ 为什么 backing 链是拒绝，而不是「可写 + `vol.warn.backing-copyup`」
//!
//! 计划原文要的是后者，那需要**按路径**打开子镜像才能解析出父镜像的名字并打开它。
//! 但 [`VolumeHandler::open`](hzip_vol::VolumeHandler::open) 拿到的只有一个
//! `Arc<dyn BlockDevice>`（设备链末端），**没有路径** ⇒ 上游 `open_*_on_device`
//! 对引用 backing 的镜像直接 `Unsupported`（`open_inner` 在 `parent_path == None`
//! 时拒绝解析父镜像）。
//!
//! 没有父镜像就意味着「未分配的簇读回 0」，而 backing 链下那里**本该读父镜像的数据**
//! ——那是**静默错误的数据**，比拒绝坏得多。故按名字拒绝，键里说明缺的是路径。
//! [`Qcow2Volume::caps`] 里 backing 的提示接线保留：上游哪天能在设备形态下解析父镜像，
//! 提示自动生效。

use std::sync::Arc;

use hzip_vol::error::{ReadOnlyReason, VolError, VolResult};
use hzip_vol::partition::{PartitionProbe, open_region, probe_partitions, sniff_whole_device};
use hzip_vol::{
    BlockDevice, BlockRead as _, FsOptions, FsVolume, Geometry, PartitionId, PartitionView,
    VolCaps, VolOptions, Volume,
};
use img_qcow2::Qcow2Reader;
use img_qcow2::header::{Header, incompat};

/// 读头部的字节数：v3 的固定字段到 104，含 `compression_type` 到 112。
///
/// 与上游 `open_inner` 读的 112 字节一致——多读 8 字节换掉「v3 + `header_length > 104`
/// 时读不到 `compression_type`」这一分支。
const HEAD_BYTES: usize = 112;

/// backing 链可写时的体积提示（体积会随 copy-up 增长）。
const WARN_BACKING_COPYUP: &str = "vol.warn.backing-copyup";

/// qcow2 卷视图（探测结果与能力判定都在构造时定一次）。
pub struct Qcow2Volume {
    /// 虚拟盘读取器（qcow2 解码 + 写侧的簇分配 / refcount 维护）。
    reader: Arc<Qcow2Reader>,
    /// 同一个对象的**设备视图**（分区层切片、文件系统挂载都吃它）。
    dev: Arc<dyn BlockDevice>,
    probe: PartitionProbe,
    opts: VolOptions,
    writable: bool,
    read_only_reason: Option<ReadOnlyReason>,
    warnings: Vec<&'static str>,
}

impl std::fmt::Debug for Qcow2Volume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Qcow2Volume")
            .field("virtual_size", &self.reader.virtual_size())
            .field("cluster_size", &self.reader.cluster_size())
            .field("version", &self.reader.version())
            .field("table", &self.probe.table)
            .field("partitions", &self.probe.partitions.len())
            .field("writable", &self.writable)
            .field("read_only_reason", &self.read_only_reason)
            .finish()
    }
}

impl Qcow2Volume {
    /// 打开卷：判定序 → 建读取器 → 探一次分区表。
    ///
    /// 分区表探测挂在构造上（与 `raw` 同款），避免 `partitions()` 被反复调用时重扫。
    pub fn open(dev: Arc<dyn BlockDevice>, opts: VolOptions) -> VolResult<Self> {
        let header = read_header(&dev)?;
        classify(&header)?;

        // dirty / corrupt 对**读**是提示（上游 `check_supported` 容忍），对**写**是硬门
        // （上游 `check_writable` 拒绝，理由：allocator 会信任那些已过期的 refcount 块，
        // 把在用簇当成空闲交出去）。故这里的处理是「降级为只读」，不是拒绝打开。
        let flagged = header.has_dirty_refcounts() || header.is_corrupt();
        // r/w 由**设备实际可写性**决定，不由调用方的愿望决定：浏览路径给的是只读链
        // （`Chain::open(.., WriteMode::ReadOnly, ..)`），此时我们只能只读打开。
        let want_rw = opts.writable && dev.is_writable() && !flagged;

        let reader = if want_rw {
            Qcow2Reader::open_rw_on_device(dev).map_err(map_upstream)?
        } else {
            Qcow2Reader::open_on_device(dev).map_err(map_upstream)?
        };

        let writable = reader.is_writable();
        let read_only_reason = if writable {
            None
        } else if flagged {
            Some(ReadOnlyReason::Dirty)
        } else {
            // 与 `raw` 同一口径：设备不可写（只读链 / 只读介质 / 按设置只读）。
            Some(ReadOnlyReason::DeviceReadOnly)
        };
        let mut warnings = Vec::new();
        if reader.has_backing() {
            warnings.push(WARN_BACKING_COPYUP);
        }

        let reader = Arc::new(reader);
        let dev: Arc<dyn BlockDevice> = reader.clone();
        let probe = probe_partitions(&dev)?;

        Ok(Self {
            reader,
            dev,
            probe,
            opts,
            writable,
            read_only_reason,
            warnings,
        })
    }

    /// 虚拟盘设备（诊断与测试用：它就是分区层脚下的那个块设备）。
    #[must_use]
    pub fn device(&self) -> Arc<dyn BlockDevice> {
        self.dev.clone()
    }

    /// 上游读取器（诊断用）。
    #[must_use]
    pub fn reader(&self) -> &Qcow2Reader {
        &self.reader
    }

    /// 探测结果（诊断用）。
    #[must_use]
    pub fn probe(&self) -> &PartitionProbe {
        &self.probe
    }
}

impl Volume for Qcow2Volume {
    fn geometry(&self) -> Geometry {
        Geometry {
            virtual_size: self.reader.virtual_size(),
            sector_size: 512,
            table: self.probe.table,
            // qcow2 恒为稀疏容器：已分配簇通常远少于虚拟容量
            is_sparse: true,
            has_backing: self.reader.has_backing(),
        }
    }

    fn partitions(&self) -> VolResult<Vec<PartitionView>> {
        Ok(self.probe.partitions.clone())
    }

    fn open_fs(&self, part: Option<PartitionId>) -> VolResult<Option<Box<dyn FsVolume>>> {
        let region = open_region(&self.dev, &self.probe, part)?;
        // 文件系统种类：有表 ⇒ 该分区的嗅探结论；无表（整卷）⇒ 现场按头部判
        let kind = match part {
            Some(i) => self.probe.partitions.get(i).and_then(|p| p.fs),
            None => sniff_whole_device(&region)?,
        };
        let Some(kind) = kind else {
            return Ok(None); // 未识别 ⇒ 上层显示「未知卷」（只读），不是错误
        };
        // 镜像层说不可写（只读链 / dirty / corrupt）⇒ 不给驱动可写性。
        // 少一次 AND 会让脏卷在驱动侧表现为「可写」，用户一路点到提交才失败。
        let fs = FsOptions {
            writable: self.opts.fs.writable && self.writable,
            ..self.opts.fs.clone()
        };
        self.opts.fs_registry.mount_best(&region, kind, &fs)
    }

    fn caps(&self) -> VolCaps {
        VolCaps {
            writable: self.writable,
            // qcow2 的写落在宿主簇上 ⇒ 源是常规文件时提交可只回放脏块
            in_place: self.writable,
            read_only_reason: self.read_only_reason,
            warnings: self.warnings.clone(),
        }
    }

    fn flush(&mut self) -> VolResult<()> {
        // 显式走**固有**方法：`Arc<Qcow2Reader>` 上 `BlockDevice` 的 blanket impl 会
        // 在自动解引用之前被选中，得到的是被折叠成 `fs_core::Error` 的那一份。
        Qcow2Reader::flush(&self.reader).map_err(map_upstream)
    }
}

/// 判定序 ①②③④（见模块文档的表格）。
fn classify(header: &Header) -> VolResult<()> {
    if header.incompatible_features & incompat::DATA_FILE != 0 {
        return Err(VolError::Unsupported("vol.unsupported.external-data"));
    }
    if header.incompatible_features & incompat::EXTENDED_L2 != 0 {
        return Err(VolError::Unsupported("vol.unsupported.ext-l2"));
    }
    if header.crypt_method != 0 {
        return Err(VolError::ReadOnly(ReadOnlyReason::Encrypted));
    }
    // 两个字段**都**非零才是「有 backing」（规范口径：任一个为零 = 没有父镜像）。
    if header.backing_file_offset != 0 && header.backing_file_size != 0 {
        return Err(VolError::Unsupported("vol.unsupported.backing-file"));
    }
    Ok(())
}

/// 读出 qcow2 头（用上游解析器，不另抄一份偏移量表）。
fn read_header(dev: &Arc<dyn BlockDevice>) -> VolResult<Header> {
    let mut head = [0u8; HEAD_BYTES];
    dev.read_at(0, &mut head).map_err(VolError::from)?;
    Header::parse(&head).map_err(map_upstream)
}

/// 上游错误 → 卷层错误（**语义类别保留**，不把一切都压成字符串）。
fn map_upstream(e: img_qcow2::Error) -> VolError {
    match e {
        img_qcow2::Error::Io(io) => VolError::Io(io),
        img_qcow2::Error::ReadOnly => VolError::ReadOnly(ReadOnlyReason::DeviceReadOnly),
        img_qcow2::Error::Unsupported(msg) => {
            // 判定序没覆盖到的上游拒写（未知 incompat 位、非 16 位 refcount、
            // 缺 refcount 表…）：给一个通用稳定键，细节进日志。
            tracing::debug!(msg, "qcow2 上游拒绝的特性");
            VolError::Unsupported(unsupported_key(msg))
        }
        img_qcow2::Error::Corrupt(_)
        | img_qcow2::Error::NotQcow2
        | img_qcow2::Error::UnsupportedVersion(_)
        | img_qcow2::Error::InvalidClusterBits(_)
        | img_qcow2::Error::OutOfBounds { .. }
        | img_qcow2::Error::Decompress(_)
        | img_qcow2::Error::BadBackingPath
        | img_qcow2::Error::BackingTooDeep => VolError::Image(e.to_string()),
    }
}

/// 上游 `Unsupported` 的自由文本 → 稳定键。
///
/// 判定序（[`classify`]）已把这三类提前拦下，故这几条是**同一批键的兜底**：
/// 上游若从别的路径（写侧的 refcount 检查等）报出同一件事，用户看到的键不变。
/// 「flagged dirty/corrupt」不在此列——那件事的理由键属于 `ReadOnly` 语义，
/// 从 `Unsupported` 里报出去会让「拒绝」与「只读」两种语义在 UI 上串味。
fn unsupported_key(msg: &str) -> &'static str {
    match msg {
        "external data file" => "vol.unsupported.external-data",
        "extended L2 entries" => "vol.unsupported.ext-l2",
        _ if msg.contains("backing file") => "vol.unsupported.backing-file",
        _ => "vol.unsupported.qcow2-feature",
    }
}

/// 挂载选项：浏览路径按「源是否可写的常规文件」决定**报给驱动的可写性**
/// （驱动据此报告真实 `FsCaps`；真正写入由 `open_editor` 的写模式设备链负责）。
///
/// 与 `hzip-fmt-raw::volume::fs_options_for` 同一口径——两个容器不该对同一件事有两种说法。
#[must_use]
pub fn fs_options_for(writable: bool) -> FsOptions {
    FsOptions {
        writable,
        // 浏览永不重放脏日志（重放 = 改档案，必须用户显式同意；见 T-IMG5 §2.2）
        replay_journal: false,
        cache_blocks: 0,
    }
}
