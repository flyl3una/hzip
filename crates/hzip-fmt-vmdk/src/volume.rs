//! VMDK 卷：设备由 [`hzip_vol::device::Chain`] 决定末端，本模块负责
//! **布局判定 → 打开上游读器 → 分区/文件系统**。
//!
//! # 判定序：先判布局，再要写权限
//!
//! 顺序是硬要求。把「这是什么布局」放在「能不能写」**之前**，是因为方向搞反的
//! 代价不对称：
//!
//! - 误判成**可写** ⇒ 我们把一个只读布局按 sparse 打开并写进去 ⇒ **改坏用户的镜像**
//!   （比打不开危险得多）；
//! - 误判成**不可写** ⇒ 用户看到一个明确的只读理由 ⇒ 最多是不方便。
//!
//! 故 [`classify_layout`] 是**纯函数**（头 512 字节 + 描述符文本 → 布局枚举），
//! 只看元数据、不读数据区，且由独立单测逐分支钉死。
//!
//! # 打开判定表
//!
//! | 布局 | 处置 | 稳定键 / 理由 |
//! |---|---|---|
//! | `monolithicSparse` | 读 + 写（按 `opts.writable`） | — |
//! | `streamOptimized` | 只读 | [`ReadOnlyReason::Layout`] |
//! | `monolithicFlat`（含 `fullDevice`/`partitionedDevice`） | **按名字拒绝** | `vol.unsupported.layout-flat` |
//! | `twoGbMaxExtent*` | **按名字拒绝**（多 extent） | `vol.unsupported.multi-extent` |
//! | `vmfsSparse`（`COWD` 或 `createType`） | **按名字拒绝** | `vol.unsupported.layout-vmfs` |
//! | 快照 delta / linked clone（有父链） | **按名字拒绝** | `vol.unsupported.snapshot-delta` |
//! | 描述符缺失/不可解析 | **按名字拒绝**（不猜） | `vol.unsupported.descriptor` |
//!
//! `uncleanShutdown` 置位 ⇒ 读可、写拒（理由 [`ReadOnlyReason::Dirty`]）。
//!
//! # `has_backing`
//!
//! 快照 delta 在打开判定里已被拒绝，故能被打开的镜像**没有父链** ⇒
//! [`Geometry::has_backing`] 恒为 `false`（不是「没查」，是「断言得了」）。

use std::sync::Arc;

use hzip_vol::error::{VolError, VolResult};
use hzip_vol::partition::{PartitionProbe, open_region, probe_partitions, sniff_whole_device};
use hzip_vol::{
    BlockDevice, BlockRead as _, FsVolume, Geometry, PartitionId, PartitionView, ReadOnlyReason,
    VolCaps, VolOptions, Volume,
};
use img_vmdk::VmdkReader;
use img_vmdk::header::offsets;

use crate::probe::{PROBE_HEAD_BYTES, VMDK_SPARSE_MAGIC, VMDK_VMFS_MAGIC};

// ---------------------------------------------------------------- 稳定键

/// `monolithicFlat` / `fullDevice` / `partitionedDevice`：数据不在这个文件里。
pub const KEY_LAYOUT_FLAT: &str = "vol.unsupported.layout-flat";
/// `twoGbMaxExtent{Sparse,Flat}`：多 extent 切分。
pub const KEY_MULTI_EXTENT: &str = "vol.unsupported.multi-extent";
/// `vmfsSparse`（ESXi delta / redo 日志）。
pub const KEY_LAYOUT_VMFS: &str = "vol.unsupported.layout-vmfs";
/// 快照 delta / linked clone：描述符声明了父链。
pub const KEY_SNAPSHOT_DELTA: &str = "vol.unsupported.snapshot-delta";
/// 描述符缺失/为空/认不出 `createType`（**不猜**）。
pub const KEY_DESCRIPTOR: &str = "vol.unsupported.descriptor";
/// `createType` 取值不在本格式已知集合内。
pub const KEY_UNKNOWN_LAYOUT: &str = "vol.unsupported.layout-unknown";
/// 布局判对了、但上游读器仍拒绝（兜底键；不在预期路径上）。
pub const KEY_OPEN: &str = "vol.unsupported.open";

/// 描述符文件的上限（与上游同量级）：真实描述符只有几百字节。
const MAX_DESCRIPTOR_BYTES: u64 = 64 * 1024;
/// 内嵌描述符的扇区数上限（真实产物 20 以内；超出的「描述符」不是描述符）。
const MAX_DESCRIPTOR_SECTORS: u64 = 128;
/// VMDK 逻辑扇区（格式固定 512，不是「可配」）。
const SECTOR: u64 = 512;

/// 描述符声明的 `parentCID` 哨兵值（= 无父）。
const NO_PARENT_CID: &str = "ffffffff";

// ---------------------------------------------------------------- 布局

/// VMDK 的布局分类（[`classify_layout`] 的结论）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// 单体稀疏：读 + 写。
    MonolithicSparse,
    /// 流式压缩稀疏（OVA 导出形态）：只读。
    StreamOptimized,
    /// 单体平坦 / 整设备：数据不在本文件，拒绝。
    MonolithicFlat,
    /// 2GB 切分的多 extent：本期拒绝。
    TwoGbMaxExtent,
    /// ESXi `vmfsSparse`（`COWD`）：本期拒绝。
    VmfsSparse,
    /// 快照 delta / linked clone：数据不全在本文件，拒绝。
    SnapshotDelta,
}

/// **纯函数**：头 + 描述符文本 → 布局（认不出 ⇒ 带稳定键的拒绝）。
///
/// 不碰数据区、不做 IO、不 panic。魔数分支优先：`COWD` 是 `vmfsSparse`，
/// 无论描述符说什么。
///
/// `createType` 的键与值都**大小写不敏感**地比对：畸形的大小写是生产工具的
/// 怪癖，不是另一种布局。上游 `Descriptor::parse` 更严（区分大小写），故极端
/// 大小写形态最终仍可能在打开阶段被上游拒绝——那种情况下错误会如实上抛，
/// 不会被当成「读成功」。
pub fn classify_layout(head: &[u8], descriptor: Option<&str>) -> VolResult<Layout> {
    if le_u32(head, offsets::MAGIC) == Some(VMDK_VMFS_MAGIC) {
        return Ok(Layout::VmfsSparse);
    }
    let Some(create_type) = descriptor.and_then(create_type_of) else {
        // 头是 KDMV 却读不到 createType：分卷 extent（描述符在旁挂的 `.vmdk` 里）
        // 或描述符被抹掉。两者都不是我们能读的形态，也不该猜。
        return Err(VolError::Unsupported(KEY_DESCRIPTOR));
    };
    let layout = match create_type.as_str() {
        "monolithicsparse" => Layout::MonolithicSparse,
        "streamoptimized" => Layout::StreamOptimized,
        "twogbmaxextentsparse" | "twogbmaxextentflat" => Layout::TwoGbMaxExtent,
        "monolithicflat" | "fulldevice" | "partitioneddevice" => Layout::MonolithicFlat,
        "vmfssparse" | "vmfs" => Layout::VmfsSparse,
        _ => return Err(VolError::Unsupported(KEY_UNKNOWN_LAYOUT)),
    };
    // 快照 delta 的 `createType` 与普通盘**一样**（sparse/streamOptimized），
    // 差别只在父链——所以它必须靠 `parentFileNameHint` / `parentCID` 认出来。
    if matches!(layout, Layout::MonolithicSparse | Layout::StreamOptimized)
        && descriptor.is_some_and(declares_parent)
    {
        return Ok(Layout::SnapshotDelta);
    }
    Ok(layout)
}

/// 描述符文本里的 `createType`（小写化）。空值/无该行 ⇒ `None`。
fn create_type_of(text: &str) -> Option<String> {
    for line in descriptor_lines(text) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("createType") {
            continue;
        }
        let value = value.trim().trim_matches('"').trim();
        return (!value.is_empty()).then(|| value.to_ascii_lowercase());
    }
    None
}

/// 描述符是否声明了父链（`parentFileNameHint` 非空，或 `parentCID` 非哨兵）。
///
/// 与上游 `descriptor::declares_parent` 同口径：`0x` 前缀的哨兵写法也认。
fn declares_parent(text: &str) -> bool {
    let mut hint = None;
    let mut cid = None;
    for line in descriptor_lines(text) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim().to_string();
        match key.trim() {
            "parentFileNameHint" => hint = Some(value),
            "parentCID" => cid = Some(value),
            _ => {}
        }
    }
    let names_a_parent_file = hint.is_some_and(|h| !h.is_empty());
    let carries_a_child_cid = cid.is_some_and(|c| {
        let c = c.as_str();
        let c = c
            .strip_prefix("0x")
            .or_else(|| c.strip_prefix("0X"))
            .unwrap_or(c);
        !c.is_empty() && !c.eq_ignore_ascii_case(NO_PARENT_CID)
    });
    names_a_parent_file || carries_a_child_cid
}

/// 描述符的**有效行**（去 NUL 填充/空白/注释/空行）。
fn descriptor_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter_map(|raw| {
        let line = raw.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        (!line.is_empty() && !line.starts_with('#')).then_some(line)
    })
}

// ---------------------------------------------------------------- 卷

/// VMDK 卷视图。
///
/// 上游 [`VmdkReader`] 既是镜像解释器、也是**虚拟磁盘**（实现
/// `BlockRead`/`BlockDevice`）：分区探测与文件系统挂载都直接跑在它上面，
/// 于是「镜像层」换了容器而「分区/文件系统层」一行不改。
pub struct VmdkVolume {
    reader: Arc<VmdkReader>,
    probe: PartitionProbe,
    opts: VolOptions,
    reason: Option<ReadOnlyReason>,
}

impl std::fmt::Debug for VmdkVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmdkVolume")
            .field("virtual_size", &self.reader.virtual_size())
            .field("writable", &VmdkReader::is_writable(&self.reader))
            .field("table", &self.probe.table)
            .field("partitions", &self.probe.partitions.len())
            .finish()
    }
}

impl VmdkVolume {
    /// 打开卷：**先判布局**（不读数据区）→ 再按需取写权限 → 探一次分区表。
    pub fn new(dev: Arc<dyn BlockDevice>, opts: VolOptions) -> VolResult<Self> {
        let (layout, mut reason) = gate(&dev, &opts)?;
        // 要写权限的三重条件：调用方要写 ∧ 布局/脏标记不否决 ∧ 设备末端真的可写。
        // 少了第三条，只读末端（浏览链）会被上游以 `Error::ReadOnly` 拒掉——
        // 那是把「只读末端」误当错误，而它本该是一条只读理由。
        let want_rw = opts.writable && reason.is_none() && dev.is_writable();
        let reader = if want_rw {
            VmdkReader::open_rw_on_device(dev).map_err(map_vmdk_err)?
        } else {
            // `BlockDevice: BlockRead`（父 trait）⇒ 上转到读半即可（只读打开要的就是这个）。
            let read: Arc<dyn hzip_vol::BlockRead> = dev;
            VmdkReader::open_on_device(read).map_err(map_vmdk_err)?
        };
        let reader = Arc::new(reader);
        if !VmdkReader::is_writable(&reader) && reason.is_none() && opts.writable {
            // 布局判过、也要过写权限，但设备末端给不出写（如浏览链的只读末端）。
            reason = Some(ReadOnlyReason::DeviceReadOnly);
        }
        let vdev: Arc<dyn BlockDevice> = reader.clone();
        let probe = probe_partitions(&vdev)?;
        tracing::debug!(
            ?layout,
            writable = VmdkReader::is_writable(&reader),
            "vmdk 卷已打开"
        );
        Ok(Self {
            reader,
            probe,
            opts,
            reason,
        })
    }

    /// 虚拟磁盘（guest 视角的块设备）——分区层与文件系统挂载都跑在它上面。
    #[must_use]
    pub fn device(&self) -> Arc<dyn BlockDevice> {
        self.reader.clone()
    }

    /// 探测结果（诊断用）。
    #[must_use]
    pub fn probe(&self) -> &PartitionProbe {
        &self.probe
    }
}

impl Volume for VmdkVolume {
    fn geometry(&self) -> Geometry {
        Geometry {
            virtual_size: self.reader.virtual_size(),
            sector_size: 512,
            table: self.probe.table,
            // 稀疏元数据（grain 表）让占用可远小于虚拟容量
            is_sparse: true,
            // 快照 delta 已在打开判定里拒绝 ⇒ 打开的镜像没有父链
            has_backing: false,
        }
    }

    fn partitions(&self) -> VolResult<Vec<PartitionView>> {
        Ok(self.probe.partitions.clone())
    }

    fn open_fs(&self, part: Option<PartitionId>) -> VolResult<Option<Box<dyn FsVolume>>> {
        let vdev = self.device();
        let region = open_region(&vdev, &self.probe, part)?;
        let kind = match part {
            Some(i) => self.probe.partitions.get(i).and_then(|p| p.fs),
            None => sniff_whole_device(&region)?,
        };
        let Some(kind) = kind else {
            return Ok(None); // 未识别 ⇒ 上层显示「未知卷」（只读），不是错误
        };
        self.opts
            .fs_registry
            .mount_best(&region, kind, &self.opts.fs)
    }

    fn caps(&self) -> VolCaps {
        // 点名上游方法：`Arc<VmdkReader>` 上还有 `BlockDevice for Arc<T>` 的转发实现同名
        let writable = VmdkReader::is_writable(&self.reader) && self.reason.is_none();
        VolCaps {
            writable,
            // 源是常规文件时提交可只回放脏块（设备链末端决定）
            in_place: writable,
            read_only_reason: if writable {
                None
            } else {
                self.reason.or(Some(ReadOnlyReason::DeviceReadOnly))
            },
            warnings: self.probe.warnings.clone(),
        }
    }

    fn flush(&mut self) -> VolResult<()> {
        // 只读打开时上游 flush 是空操作；可写时它同时降下 `uncleanShutdown` 标记。
        //
        // 用**全限定**写法点名上游的 `VmdkReader::flush`：`Arc<VmdkReader>` 上还有一条
        // `BlockDevice for Arc<T>` 的转发实现也提供 `flush`，方法解析会在**第一层**
        // autocderef 就命中它（它转发到同一个函数，但把错误类型换成了 `fs_core::Error`）。
        // 点名 = 消除这条静默歧义。
        VmdkReader::flush(&self.reader).map_err(map_vmdk_err)
    }
}

/// 挂载选项：浏览路径按「源是否可写的常规文件」决定**报给驱动的可写性**
/// （驱动据此报告真实 `FsCaps`；真正写入由 `open_editor` 的写模式设备链负责）。
#[must_use]
pub fn fs_options_for(writable: bool) -> hzip_vol::FsOptions {
    hzip_vol::FsOptions {
        writable,
        // 浏览永不重放脏日志（重放 = 改档案，必须用户显式同意）
        replay_journal: false,
        cache_blocks: 0,
    }
}

// ---------------------------------------------------------------- 门禁

/// 打开判定序：读头 → 读描述符（**只碰元数据区**）→ 分类 → 定理由/拒绝。
fn gate(
    dev: &Arc<dyn BlockDevice>,
    opts: &VolOptions,
) -> VolResult<(Layout, Option<ReadOnlyReason>)> {
    let head = read_head(dev, PROBE_HEAD_BYTES)?;
    let descriptor = read_descriptor(dev, &head)?;
    let layout = classify_layout(&head, descriptor.as_deref())?;

    let mut reason = match layout {
        Layout::MonolithicSparse => None,
        Layout::StreamOptimized => Some(ReadOnlyReason::Layout),
        Layout::MonolithicFlat => return Err(VolError::Unsupported(KEY_LAYOUT_FLAT)),
        Layout::TwoGbMaxExtent => return Err(VolError::Unsupported(KEY_MULTI_EXTENT)),
        Layout::VmfsSparse => return Err(VolError::Unsupported(KEY_LAYOUT_VMFS)),
        Layout::SnapshotDelta => return Err(VolError::Unsupported(KEY_SNAPSHOT_DELTA)),
    };
    // `uncleanShutdown`：读可、写拒。只读理由不覆盖更根本的布局理由
    // （streamOptimized 恒只读，理由说 Layout 更准确）。
    if head.get(offsets::UNCLEAN_SHUTDOWN).is_some_and(|&b| b != 0) {
        reason.get_or_insert(ReadOnlyReason::Dirty);
    }
    // 用户/设置要求只读：给得出理由，UI 才说得清「为什么不能改」。
    if !opts.writable {
        reason.get_or_insert(ReadOnlyReason::BySetting);
    }
    Ok((layout, reason))
}

/// 读头部窗口（源不足则读实际长度；空源 ⇒ 空缓冲，交给分类给拒绝）。
fn read_head(dev: &Arc<dyn BlockDevice>, want: usize) -> VolResult<Vec<u8>> {
    let size = dev.size_bytes();
    let len = want.min(usize::try_from(size).unwrap_or(want));
    let mut buf = vec![0u8; len];
    if len > 0 {
        dev.read_at(0, &mut buf)?;
    }
    Ok(buf)
}

/// 读描述符文本（**只碰元数据**，永不读数据区）。
///
/// 两种形态：
/// - 头是 `KDMV` 稀疏 extent ⇒ 描述符内嵌在 `descriptorOffset`/`descriptorSize` 处；
///   两者为 0（分卷 extent 的常态）或整段全 NUL ⇒ `Ok(None)`（交给分类按名拒绝）。
/// - 头不是稀疏头 ⇒ 整个文件可能就是一个**描述符文件**（flat / 多 extent / delta 的
///   形态）⇒ 小文件整体当文本读，非 UTF-8 ⇒ `Ok(None)`。
fn read_descriptor(dev: &Arc<dyn BlockDevice>, head: &[u8]) -> VolResult<Option<String>> {
    if le_u32(head, offsets::MAGIC) == Some(VMDK_VMFS_MAGIC) {
        return Ok(None); // vmfsSparse：布局由魔数定，描述符说什么都不改变结论
    }
    let size = dev.size_bytes();
    if le_u32(head, offsets::MAGIC) == Some(VMDK_SPARSE_MAGIC) {
        let off = le_u64(head, offsets::DESCRIPTOR_OFFSET).unwrap_or(0);
        let len = le_u64(head, offsets::DESCRIPTOR_SIZE).unwrap_or(0);
        if off == 0 || len == 0 {
            return Ok(None);
        }
        if len > MAX_DESCRIPTOR_SECTORS {
            return Err(VolError::Unsupported(KEY_DESCRIPTOR));
        }
        let byte_off = off
            .checked_mul(SECTOR)
            .ok_or_else(|| VolError::Image("VMDK 描述符偏移溢出".to_string()))?;
        let byte_len = len * SECTOR;
        let end = byte_off
            .checked_add(byte_len)
            .ok_or_else(|| VolError::Image("VMDK 描述符区间溢出".to_string()))?;
        if end > size {
            return Err(VolError::Unsupported(KEY_DESCRIPTOR));
        }
        let mut buf = vec![0u8; byte_len as usize];
        dev.read_at(byte_off, &mut buf)?;
        return Ok(text_if_any(&buf));
    }
    // 不是稀疏头 ⇒ 可能是一个描述符文本文件
    if size == 0 || size > MAX_DESCRIPTOR_BYTES {
        return Ok(None);
    }
    let mut buf = vec![0u8; size as usize];
    dev.read_at(0, &mut buf)?;
    Ok(text_if_any(&buf))
}

/// 有效文本（去 NUL/空白后非空且是 UTF-8）⇒ `Some`。
fn text_if_any(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let meaningful = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!meaningful.is_empty()).then(|| text.to_string())
}

// ---------------------------------------------------------------- 上游错误 → 稳定键

/// 上游 `img_vmdk::Error` → [`VolError`]。
///
/// 布局类拒绝**本 crate 已经在 [`classify_layout`] 里按名给过了**；这里兜住
/// 漏网的形态（如大小写怪癖、分卷描述符文件），仍尽量给稳定键而不是自由文本。
fn map_vmdk_err(e: img_vmdk::Error) -> VolError {
    match e {
        img_vmdk::Error::Io(io) => VolError::Io(io),
        img_vmdk::Error::Unsupported(msg) => VolError::Unsupported(named_key(msg)),
        img_vmdk::Error::NotVmdk => VolError::Image("不是 VMDK 镜像（稀疏头魔数不符）".to_string()),
        img_vmdk::Error::Corrupt(msg) => VolError::Image(format!("VMDK 结构不合法：{msg}")),
        img_vmdk::Error::OutOfBounds { offset, len, size } => VolError::Image(format!(
            "VMDK 读越界：[{offset}, {offset}+{len}) 超出 {size}"
        )),
        img_vmdk::Error::ReadOnly => VolError::ReadOnly(ReadOnlyReason::DeviceReadOnly),
    }
}

/// 把上游的 `Unsupported` 文案折成我们的稳定键。
fn named_key(msg: &str) -> &'static str {
    if msg.starts_with("twoGbMaxExtent") {
        KEY_MULTI_EXTENT
    } else if msg == "monolithicFlat" {
        KEY_LAYOUT_FLAT
    } else if msg == "vmfsSparse" || msg == "vmfs" {
        KEY_LAYOUT_VMFS
    } else if msg.contains("delta/child") {
        KEY_SNAPSHOT_DELTA
    } else {
        KEY_OPEN
    }
}

fn le_u32(bytes: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(off..off + 4)?.try_into().ok()?,
    ))
}

fn le_u64(bytes: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(off..off + 8)?.try_into().ok()?,
    ))
}
