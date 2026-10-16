//! `NtfsDriver`：嗅探复核、挂载决策（计划 §2.3 拒写判定序）、设备物化（§2.4）。
//!
//! # 为什么挂载要先物化
//!
//! 上游是**基于路径、无状态**的（每次操作重开镜像），而我们的设备链末端可能是
//! 覆盖层或内存盘——它们没有路径。故 `mount` 的第一件事是把 [`BlockDevice`] 的
//! 字节写进一个 `hzip-spool-` 前缀的临时镜像文件，之后所有上游调用都走该路径。
//! 该前缀是仓库的孤儿回收约定（`hzip-core::services::space::sweep_stale_spools`），
//! 换前缀 = 崩溃残留永不回收，故这里显式传 [`SPOOL_PREFIX`] 而不是依赖默认值。
//!
//! 改动的回放在 [`NtfsVolume::flush`]（提交前原镜像一个字节不动 ⇒ 取消即丢）；
//! 临时文件随 [`SpoolFile`] 的丢弃而删除。

use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;

use fs_ntfs::block_io::PathIo as BlockPathIo;
use fs_ntfs::error::{Error as NtfsError, Kind};
use fs_ntfs::{fsck, read};
use hzip_vol::materialize::{SPOOL_PREFIX, SpoolFile, spool_from_stream};
use hzip_vol::{
    BlockDevice, FsDriver, FsKind, FsOptions, FsVolume, ReadOnlyReason, SpoolPolicy, VolError,
    VolResult,
};

use crate::volume::NtfsVolume;

/// 驱动 id。
pub const DRIVER_ID: &str = "ntfs";

/// 嗅探复核需要的引导扇区字节数。
const BOOT_BYTES: usize = 512;
/// `$Boot` 的 OEM 串（偏移 `0x03`，8 字节）。
const OEM_OFFSET: usize = 0x03;
const OEM_ID: &[u8; 8] = b"NTFS    ";
/// 引导扇区结尾签名（`0x55AA`，偏移 `0x1FE`）。
const SIGNATURE_OFFSET: usize = 0x1FE;

/// NTFS 驱动（无状态：所有状态都在挂载出来的 [`NtfsVolume`] 里）。
#[derive(Debug, Default)]
pub struct NtfsDriver;

impl NtfsDriver {
    /// 新驱动（core 启动期 `FsRegistry::register` 用）。
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// 类型化挂载：物化 → 只读探测 → 判定 → （写挂载时）上游准备。
    ///
    /// [`FsDriver::mount`] 就是它的装箱版；类型化入口给需要读取断的队伍用
    /// （例如 [`NtfsVolume::lossy_name_count`]）。
    pub fn mount_volume(
        &self,
        dev: Arc<dyn BlockDevice>,
        opts: &FsOptions,
    ) -> VolResult<NtfsVolume> {
        let image = materialise(&dev)?;
        let probe = probe_image(image.path())?;
        let decision = decide(&probe, dev.is_writable(), opts);
        if !decision.writable {
            tracing::info!(
                path = %image.path().display(),
                reason = decision.reason.map_or("?", ReadOnlyReason::key),
                "NTFS 以只读挂载"
            );
        }
        NtfsVolume::open(dev, image, probe, decision)
    }
}

impl FsDriver for NtfsDriver {
    fn id(&self) -> &'static str {
        DRIVER_ID
    }

    /// 两份独立证据都要对：嗅探结论是 NTFS，且**头部**确实是 NTFS 引导扇区
    /// （OEM 串 + `0x55AA` 签名）。分区表嗅探的窗口可能不落在卷首，故复核不能省。
    fn supports(&self, kind: FsKind, head: &[u8]) -> bool {
        matches!(kind, FsKind::Ntfs)
            && head.len() >= BOOT_BYTES
            && &head[OEM_OFFSET..OEM_OFFSET + OEM_ID.len()] == OEM_ID
            && head[SIGNATURE_OFFSET..SIGNATURE_OFFSET + 2] == [0x55, 0xAA]
    }

    fn mount(&self, dev: Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<Box<dyn FsVolume>> {
        Ok(Box::new(self.mount_volume(dev, opts)?))
    }
}

/// 设备 → 临时镜像文件（`hzip-spool-` 前缀）。
fn materialise(dev: &Arc<dyn BlockDevice>) -> VolResult<SpoolFile> {
    let mut src = DeviceSource {
        dev: Arc::clone(dev),
        offset: 0,
    };
    let policy = SpoolPolicy {
        prefix: SPOOL_PREFIX,
        ..SpoolPolicy::default()
    };
    spool_from_stream(&mut src, &policy, None)
}

/// 顺序读的 [`Read`] 适配器：把块设备当作一个只前向的流交给 spool。
struct DeviceSource {
    dev: Arc<dyn BlockDevice>,
    offset: u64,
}

impl Read for DeviceSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let size = self.dev.size_bytes();
        if self.offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let want = usize::try_from(size - self.offset)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        self.dev
            .read_at(self.offset, &mut buf[..want])
            .map_err(io::Error::other)?;
        self.offset += want as u64;
        Ok(want)
    }
}

/// 挂载前只读探测到的事实（判定序 ①②③ 的输入）。
#[derive(Debug, Clone)]
pub(crate) struct Probe {
    /// `$Volume` 的 dirty 标志（未干净卸载）。
    pub dirty: bool,
    /// 休眠证据：`Some(说明)` = Windows 在这卷上休眠/快速启动过。
    pub hibernated: Option<String>,
    /// `$LogFile` 含事务（`LogfileState::Pending`）。
    pub journal_pending: bool,
    /// `$LogFile` 一句话状态（诊断与 `audit()` 用）。
    pub journal_state: String,
}

/// 只读探测：卷可解析 + dirty + 休眠 + 日志状态。**不写盘。**
pub(crate) fn probe_image(path: &Path) -> VolResult<Probe> {
    let mut io = BlockPathIo::open_ro(path)
        .map_err(|e| VolError::Image(format!("打开临时镜像失败: {e}")))?;
    // 与 `facade::Filesystem::mount` 同一条路：boot + MFT + $Volume 都要能解析。
    let info = read::read_volume_info(&mut io)
        .map_err(|e| VolError::Image(format!("不是可解析的 NTFS 卷: {e}")))?;
    tracing::debug!(
        label = %info.label,
        cluster_size = info.cluster_size,
        version = %format_args!("{}.{}", info.version_major, info.version_minor),
        "NTFS 卷探测"
    );

    let dirty = fsck::is_dirty_io(&mut io)
        .map_err(|e| VolError::Image(format!("读 $Volume 的 dirty 标志失败: {e}")))?;
    // 读不出 hiberfil.sys 时上游的口径是「**可能**休眠」⇒ 拒写（我们不猜它是干净）。
    let hibernated = match fsck::hibernation_io(&mut io) {
        Ok(why) => why,
        Err(e) => Some(format!("hiberfil.sys 读取失败（可能休眠）: {e}")),
    };
    let state = fsck::logfile_state_io(&mut io)
        .map_err(|e| VolError::Image(format!("读 $LogFile 状态失败: {e}")))?;
    Ok(Probe {
        dirty,
        hibernated,
        journal_pending: state.needs_replay(),
        journal_state: state.describe(),
    })
}

/// 挂载决策（判定序：设置 → 设备 → ①休眠 → ②日志 → ③dirty）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Decision {
    pub writable: bool,
    pub reason: Option<ReadOnlyReason>,
}

/// 按计划 §2.3 的判定序定「能不能写」。
///
/// ③ 与计划原文的一处**实测差异**：上游 `prepare_rw_mount` 对 dirty 卷是**直接拒**
/// （dirty 意味着「要求一次全卷检查」，上游不做那件事就不写盘），所以这里给的是
/// `ReadOnly(Dirty)` 而不是「可写 + 提示」——沿用上游口径，不绕过它。
pub(crate) fn decide(probe: &Probe, device_writable: bool, opts: &FsOptions) -> Decision {
    let read_only = |reason: ReadOnlyReason| Decision {
        writable: false,
        reason: Some(reason),
    };
    if !opts.writable {
        return read_only(ReadOnlyReason::BySetting);
    }
    if !device_writable {
        return read_only(ReadOnlyReason::DeviceReadOnly);
    }
    if probe.hibernated.is_some() {
        return read_only(ReadOnlyReason::Hibernated);
    }
    if probe.journal_pending && !opts.replay_journal {
        return read_only(ReadOnlyReason::JournalPending);
    }
    if probe.dirty {
        return read_only(ReadOnlyReason::Dirty);
    }
    Decision {
        writable: true,
        reason: None,
    }
}

/// 写挂载前的准备：走上游 `ntfs.sys` 那条路（先拒绝、必要时先重放日志），
/// 再做 `$VOLUME_INFORMATION` 的 upgrade-on-mount（best-effort，与 `facade::mount_rw` 一致）。
///
/// 上游的拒绝（EPERM 语义）里带着原因，这里按原因折回[`ReadOnlyReason`]：**不半写**。
pub(crate) fn prepare_rw(path: &Path, replay: bool) -> VolResult<()> {
    let prepared = (|| -> Result<(), NtfsError> {
        let mut io = BlockPathIo::open_rw(path).map_err(NtfsError::refused)?;
        if replay {
            fsck::prepare_rw_mount_replaying_io(&mut io)?;
        } else {
            fsck::prepare_rw_mount_io(&mut io)?;
        }
        Ok(())
    })();
    if let Err(e) = prepared {
        let reason = if e.message().contains("dirty") {
            ReadOnlyReason::Dirty
        } else if e.message().contains("hibernat") {
            ReadOnlyReason::Hibernated
        } else {
            ReadOnlyReason::JournalPending
        };
        tracing::warn!(reason = reason.key(), "NTFS 写挂载被上游拒绝: {e}");
        return Err(VolError::ReadOnly(reason));
    }
    match fsck::upgrade_volume_version(path) {
        Ok(true) => tracing::info!("$VOLUME_INFORMATION 1.2 → 3.1（upgrade on mount）"),
        Ok(false) => tracing::debug!("$VOLUME_INFORMATION 无需升级"),
        // 与上游一致：升级失败只是 warn，卷仍可用。
        Err(e) => tracing::warn!("$VOLUME_INFORMATION 升级跳过: {e}"),
    }
    Ok(())
}

/// 上游 `fs_ntfs::error::Error` → [`VolError`]（**按 `Kind` 分类**，不靠消息文本）。
///
/// # 为什么不用上游 `facade`
///
/// `facade::Error` 只有一条 `String`——它把 `Kind` 丢掉了，于是 `ENOSPC` / `EPERM` /
/// `ENOTEMPTY` 都只能靠猜消息文本来分辨，而界面要的是**稳定键**
/// （`vol.err.no-space` 之类，见计划 §2.1 与 `hzip-vol` 的「理由即文案键」）。
/// `facade` 本身就是这批 `read::` / `write::` / `fsck::` 公开路径函数的薄包装
/// （同一个「每次调用重开镜像」语义），故本驱动直接用其底层函数以保住错误类别；
/// `supports`/挂载决策仍走 `read::` / `fsck::` 的同一批门（见 crate 文档）。
pub(crate) fn map_fs_error(what: &str, e: NtfsError) -> VolError {
    match e.kind() {
        Kind::NoSpace => VolError::Quota("vol.err.no-space"),
        Kind::NotEmpty => VolError::Unsupported("vol.err.dir-not-empty"),
        Kind::Exists => VolError::Unsupported("vol.err.exists"),
        Kind::NotFound => VolError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{what}: {e}"),
        )),
        Kind::NotDirectory | Kind::IsDirectory => VolError::Unsupported("vol.err.bad-type"),
        Kind::Invalid => VolError::Unsupported("vol.err.invalid-name"),
        Kind::Refused => VolError::Unsupported("vol.err.refused"),
        // `Io` / `Range` 以及上游以后新增的类别：如实带原文，不假装知道类别。
        _ => VolError::Image(format!("{what}: {e}")),
    }
}
