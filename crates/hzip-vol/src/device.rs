//! 设备链：**写模式开关的全部实现就在这一个文件**。
//!
//! # 三种末端
//!
//! | 模式 | 末端设备 | 语义 |
//! |---|---|---|
//! | [`WriteMode::ReadOnly`] | `ReadOnlyDevice<FileDevice::open>` | 不写任何字节 |
//! | [`WriteMode::InPlace`] | `FileDevice::open_rw`（真文件） | 写即落盘；原子性由格式/文件系统自持 |
//! | [`WriteMode::Overlay`]（默认） | `FileDevice::open` + [`OverlayDevice`] | 改动暂存，提交时只回放脏块 |
//!
//! 三种末端**以上**的一切（镜像容器、分区层、文件系统驱动、条目投影、`ChangeSet`）
//! 完全共用同一份代码——这就是「设置页切一个开关」的全部代价。用户裁定 ②：
//! 默认 `Overlay`，可切 `InPlace`/只读。
//!
//! # 源的两种形态
//!
//! - `Source::File`：直接用该路径（未压缩顶层镜像的常态）；
//! - `Source::Stream`：组合链（`.qcow2.gz`）或档案内条目（`v.zip` 里的 `disk.vmdk`）
//!   交给我们的形态 ⇒ **先 spool 到临时文件**（[`crate::materialize`]），因为块设备
//!   必须可随机访问。`Stdin`/`Volumes` 本期不支持（稳定键拒绝）。
//!
//! # 提交（[`Chain::commit`]）
//!
//! | 情形 | 动作 |
//! |---|---|
//! | Overlay + 有真文件 | `open_rw` 真文件 → **只回放脏块** → flush（用户日常路径） |
//! | Overlay + 调用方给了目标设备 | 回放到该设备 |
//! | InPlace | `flush`（早已落盘） |
//! | ReadOnly | 空操作（调用方本不该有改动） |
//! | Overlay + 源是流 | 报错：流源无处回放，调用方必须走 [`Chain::stream_into`]（外层重写） |

use std::path::PathBuf;
use std::sync::Arc;

use fs_core::BlockDevice as _;
use hzip_format::{Source, VolumeWriteMode};
use hzip_progress::CancellationToken;

use crate::error::{ReadOnlyReason, VolError, VolResult};
use crate::materialize::{self, SpoolFile};
use crate::overlay::OverlayDevice;

/// 读缓存块大小（64 KiB：与 qcow2 簇 / VMDK grain 同量级，一次命中可服务十余次
/// 文件系统块读）。
const CACHE_BLOCK: u64 = 64 * 1024;

/// 写模式（与 `hzip-format::VolumeWriteMode` 一一对应；core 从配置下发）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteMode {
    /// 只读。
    ReadOnly,
    /// 就地写（直接写原文件）。
    InPlace,
    /// 覆盖写（默认）：
    #[default]
    Overlay,
}

impl From<VolumeWriteMode> for WriteMode {
    fn from(m: VolumeWriteMode) -> Self {
        match m {
            VolumeWriteMode::Overlay => Self::Overlay,
            VolumeWriteMode::InPlace => Self::InPlace,
            VolumeWriteMode::ReadOnly => Self::ReadOnly,
        }
    }
}

/// 提交统计（进进度/日志：改了多少、怎么提交的）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitStats {
    /// 回放的脏块数（`InPlace`/`ReadOnly` 为 0）。
    pub dirty_blocks: u64,
    /// 回放的脏字节（= 块数 × 64 KiB，上界）。
    pub dirty_bytes: u64,
    /// 实际使用的模式。
    pub mode: WriteMode,
}

/// 覆盖写链的返回：设备 + 覆盖层（两者指向同一对象，分开是为了提交时能拿到覆盖层）。
type OverlayPair = (Arc<dyn fs_core::BlockDevice>, Option<Arc<OverlayDevice>>);

/// 解析后的源：`path` = 设备落点（可能是 spool 临时件），`real` = 可写回的真文件。
struct ResolvedSource {
    path: PathBuf,
    real: Option<PathBuf>,
    spool: Option<SpoolFile>,
}

/// 设备链：源 → 可随机读写的块设备 + 提交所需的一切。
pub struct Chain {
    mode: WriteMode,
    device: Arc<dyn fs_core::BlockDevice>,
    overlay: Option<Arc<OverlayDevice>>,
    real_path: Option<PathBuf>,
    spool: Option<SpoolFile>,
    warnings: Vec<&'static str>,
    read_only_reason: Option<ReadOnlyReason>,
}

impl std::fmt::Debug for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chain")
            .field("mode", &self.mode)
            .field("real_path", &self.real_path)
            .field("spooled", &self.spool.is_some())
            .field(
                "dirty_blocks",
                &self.overlay.as_ref().map(|o| o.dirty_blocks()),
            )
            .field("warnings", &self.warnings)
            .finish()
    }
}

impl Chain {
    /// 按 `mode` 打开设备链。
    ///
    /// `cancel` 只用于 spool 阶段（组合链/档案条目的整层解压）。
    pub fn open(
        src: Source,
        mode: WriteMode,
        opts: &crate::VolOptions,
        cancel: Option<&CancellationToken>,
    ) -> VolResult<Self> {
        let resolved = Self::resolve_source(src, opts, cancel)?;
        let path = resolved.path.clone();
        let spool = resolved.spool;
        let mut warnings = Vec::new();
        let mut read_only_reason = None;

        let (device, overlay): (Arc<dyn fs_core::BlockDevice>, Option<Arc<OverlayDevice>>) =
            match mode {
                WriteMode::ReadOnly => {
                    read_only_reason = Some(ReadOnlyReason::BySetting);
                    let f = fs_core::FileDevice::open(&path)?;
                    (Arc::new(fs_core::ReadOnlyDevice::new(f)), None)
                }
                WriteMode::InPlace => match fs_core::FileDevice::open_rw(&path) {
                    Ok(f) => (Arc::new(f), None),
                    Err(e) => {
                        // 打不开 r/w（只读介质/权限/被占用）⇒ 降级为覆盖写，**如实记提示**。
                        // （计划原文写「理由 = DeviceReadOnly」；落地改为「提示」——因为此时
                        // 仍可写，说成「只读理由」会误导 UI 与用户。）
                        tracing::warn!(path = %path.display(), "就地写打不开 r/w，降级覆盖写：{e}");
                        warnings.push("vol.warn.inplace-fallback");
                        Self::build_overlay(&path, opts)?
                    }
                },
                WriteMode::Overlay => Self::build_overlay(&path, opts)?,
            };

        let device: Arc<dyn fs_core::BlockDevice> = if opts.cache_blocks > 0 {
            fs_core::CachingDevice::new(device, CACHE_BLOCK, opts.cache_blocks)
        } else {
            device
        };

        Ok(Self {
            mode,
            device,
            overlay,
            real_path: resolved.real,
            spool,
            warnings,
            read_only_reason,
        })
    }

    /// `Source` → 可随机访问的路径 + 「可写回的真文件」+ 保活 spool。
    ///
    /// **`real` 与 `path` 必须分开**：流源 spool 出来的临时文件只是我们自己的暂存，
    /// 不是调用方的档案——提交时绝不能把它当成写回目标（那会「成功」地改掉一个马上就
    /// 被删掉的临时文件，而真正的源一个字节没动）。
    fn resolve_source(
        src: Source,
        opts: &crate::VolOptions,
        cancel: Option<&CancellationToken>,
    ) -> VolResult<ResolvedSource> {
        match src {
            Source::File(p) => Ok(ResolvedSource {
                path: p.clone(),
                real: Some(p),
                spool: None,
            }),
            Source::Stream(mut s) => {
                let spool = materialize::spool_from_stream(&mut *s, &opts.spool, cancel)?;
                let path = spool.path().to_path_buf();
                tracing::debug!(bytes = spool.len(), path = %path.display(), "组合链内层已 spool");
                Ok(ResolvedSource {
                    path,
                    real: None,
                    spool: Some(spool),
                })
            }
            Source::Stdin => Err(VolError::Unsupported("vol.reject.stdin-source")),
            Source::Volumes { .. } => Err(VolError::Unsupported("vol.reject.multi-volume")),
        }
    }

    /// 覆盖写链：base 只读打开 + 溢出临时文件（同前缀，孤儿回收可见）。
    fn build_overlay(path: &std::path::Path, opts: &crate::VolOptions) -> VolResult<OverlayPair> {
        let base = Arc::new(fs_core::FileDevice::open(path)?);
        crate::materialize::ensure_spool_dir(&opts.spool.dir)?;
        let spill = tempfile::Builder::new()
            .prefix(crate::materialize::SPOOL_PREFIX)
            .tempfile_in(&opts.spool.dir)
            .map_err(VolError::Io)?;
        let overlay = Arc::new(OverlayDevice::new(
            base,
            spill.into_file(),
            crate::overlay::OVERLAY_BLOCK,
        ));
        Ok((overlay.clone(), Some(overlay)))
    }

    /// 暴露给镜像容器/分区层的设备。
    #[must_use]
    pub fn device(&self) -> Arc<dyn fs_core::BlockDevice> {
        self.device.clone()
    }

    /// 写模式。
    #[must_use]
    pub fn mode(&self) -> WriteMode {
        self.mode
    }

    /// 覆盖层（提交回放用；`InPlace`/`ReadOnly` 为 `None`）。
    #[must_use]
    pub fn overlay(&self) -> Option<&Arc<OverlayDevice>> {
        self.overlay.as_ref()
    }

    /// 提示（不阻止操作，只讲代价）：例 `vol.warn.inplace-fallback`。
    #[must_use]
    pub fn warnings(&self) -> &[&'static str] {
        &self.warnings
    }

    /// 只读理由（当前实现只在 readonly 模式下为 `BySetting`）。
    #[must_use]
    pub fn read_only_reason(&self) -> Option<ReadOnlyReason> {
        self.read_only_reason
    }

    /// 源是否来自流（spool 的临时文件）——提交时无处「就地回放」。
    #[must_use]
    pub fn is_spooled(&self) -> bool {
        self.spool.is_some()
    }

    /// 当前脏块数（进度/日志）。
    #[must_use]
    pub fn dirty_blocks(&self) -> u64 {
        self.overlay.as_ref().map_or(0, |o| o.dirty_blocks())
    }

    /// 提交：见模块文档的表格。
    pub fn commit(
        &self,
        dst: Option<&dyn fs_core::BlockDevice>,
        cancel: Option<&CancellationToken>,
    ) -> VolResult<CommitStats> {
        match self.mode {
            WriteMode::ReadOnly => {
                tracing::debug!("只读链提交：空操作");
                Ok(CommitStats {
                    mode: self.mode,
                    ..Default::default()
                })
            }
            WriteMode::InPlace => {
                self.device.flush()?;
                Ok(CommitStats {
                    mode: self.mode,
                    ..Default::default()
                })
            }
            WriteMode::Overlay => {
                let overlay = self
                    .overlay
                    .as_ref()
                    .ok_or_else(|| VolError::Image("覆盖写链缺覆盖层（内部错误）".to_string()))?;
                let blocks = match dst {
                    Some(target) => overlay.replay_into(target, cancel)?,
                    None => {
                        let path = self
                            .real_path
                            .as_ref()
                            .ok_or(VolError::Unsupported("vol.reject.commit-spooled"))?;
                        let target = fs_core::FileDevice::open_rw(path)?;
                        let n = overlay.replay_into(&target, cancel)?;
                        target.flush()?;
                        n
                    }
                };
                Ok(CommitStats {
                    dirty_blocks: blocks,
                    dirty_bytes: blocks * crate::overlay::OVERLAY_BLOCK,
                    mode: self.mode,
                })
            }
        }
    }

    /// 整量写进一条**只能前向写**的流（源是流 / 需重写外层时的唯一交付方式）。
    ///
    /// 与 [`Self::commit`] 的差别：这里没有「只写脏块」的可能——外层 `.gz` 之类
    /// 必须整段重压。进度以字节上报（core 的进度窗按字节）。
    pub fn stream_into(
        &self,
        w: &mut dyn std::io::Write,
        cancel: Option<&CancellationToken>,
    ) -> VolResult<u64> {
        let total = self.device.size_bytes();
        let mut buf = vec![0u8; (1 << 20).min(total as usize).max(1)];
        let mut off = 0u64;
        while off < total {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(VolError::Cancelled);
            }
            let len = (buf.len() as u64).min(total - off) as usize;
            self.device.read_at(off, &mut buf[..len])?;
            w.write_all(&buf[..len]).map_err(VolError::Io)?;
            off += len as u64;
        }
        w.flush().map_err(VolError::Io)?;
        Ok(off)
    }

    /// 重置到「源 + 空覆盖」：core 的密码重试会对同一会话重复提交，必须可重置。
    ///
    /// `InPlace` 链**不可重置**（改动已落盘）——返回 `Ok` 但记 `warn`，不假装回滚。
    pub fn reset(&mut self) -> VolResult<()> {
        if let Some(overlay) = self.overlay.as_ref() {
            overlay.reset()?;
        } else if self.mode == WriteMode::InPlace {
            tracing::warn!("就地写链不可重置（改动已落盘）");
        }
        Ok(())
    }
}
