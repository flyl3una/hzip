//! `VolumeEditor`：把「卷内变更」接进 `ArchiveEditor` 契约。
//!
//! # 两条交付路径（与 `hzip-vol::device::Chain` 的分工）
//!
//! | 契约方法 | 干什么 | 谁决定 |
//! |---|---|---|
//! | [`ArchiveEditor::write_in_place`] | 把变更应用进卷，**提交回原文件**（只回放脏块） | core：源是常规文件 ∧ 非组合链 ∧ `supports_in_place_edit()` |
//! | [`ArchiveEditor::rewrite`] | 把变更应用进卷，再把**整量**交割到 `dst`（`Sink::File` 走 OS 级拷贝 + 回放；`Sink::Stream` 走外层编码器） | core：源是流/需重写外层（组合档） |
//!
//! # 写模式从哪来（关键接线）
//!
//! `open_editor(src, opts: &OpenOptions)` **看不到**写模式（它在 `CreateOptions` 里）。
//! 故本编辑器在 `open_editor` 时只**记住源**；真正开工时（`write_in_place`/`rewrite`
//! 拿到 [`WriteCtx`]）才按 `ctx.opts.volume_write` 建链 ⇒ 浏览路径不写、写入路径按
//! 用户设置（默认覆盖写）。
//!
//! # 每次调用都从干净中间态开始（core 会重试）
//!
//! core 的密码重试会对同一 editor 多次调用 ⇒ 每次调用**新建**设备链（覆盖层为空），
//! 绝不累积上一次的暂存。流源只在 `open_editor` 时 spool **一次**（流不可重放），
//! 之后各次调用都以那个临时文件为底。
//!
//! # 失败语义（如实，不假装原子）
//!
//! - 应用变更/自检阶段失败 ⇒ **原档一个字节未动**（覆盖层随链丢弃）；
//! - 提交回放中途失败 ⇒ 已回放的脏块留在原档上（真实存在「部分提交」），
//!   错误照实上抛并记 `tracing::error!`，不谎报成功。

use std::path::{Path, PathBuf};

use crate::device::{Chain, WriteMode};
use crate::error::{VolError, VolResult};
use crate::materialize::{self, SpoolFile};
use crate::{FsVolume, VolOptions, VolumeHandler};
use hzip_error::HzipError;
use hzip_format::{ArchiveEditor, Change, ChangeSet, EntryName, Sink, WriteCtx, WriteReport};

/// 编辑器持有的一次性源（流源在此 spool 一次，供多次调用复用）。
enum OpenedSource {
    /// 常规文件（可直接重开）。
    File(PathBuf),
    /// 流源物化后的临时文件（保活句柄在 `_keep`）。
    Spooled { path: PathBuf, _keep: SpoolFile },
}

impl OpenedSource {
    fn path(&self) -> &Path {
        match self {
            Self::File(p) => p,
            Self::Spooled { path, .. } => path,
        }
    }

    /// 能否作为「提交回放」的目标（流源不能：临时件随会话消失）。
    fn is_real(&self) -> bool {
        matches!(self, Self::File(_))
    }
}

/// 卷编辑器：持有 handler（构造卷）+ 源 + 注册表。
pub struct VolumeEditor<H: VolumeHandler + Send + Sync + 'static> {
    handler: H,
    src: OpenedSource,
    /// 卷选项（**文件系统驱动注册表就在其 [`VolOptions::fs_registry`] 里**，
    /// 故本结构不另存一份——避免两处注册表漂移）。
    opts: VolOptions,
}

impl<H: VolumeHandler + Send + Sync + 'static> std::fmt::Debug for VolumeEditor<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VolumeEditor")
            .field("handler", &self.handler.id())
            .field("src", &self.src.path())
            .finish()
    }
}

impl<H: VolumeHandler + Send + Sync + 'static> VolumeEditor<H> {
    /// 打开：`File` 源按路径记住；`Stream` 源**一次性** spool（流不可重放）。
    pub fn open(handler: H, src: hzip_format::Source, opts: VolOptions) -> VolResult<Self> {
        let opened = match src {
            hzip_format::Source::File(p) => OpenedSource::File(p),
            hzip_format::Source::Stream(mut s) => {
                let spool = materialize::spool_from_stream(&mut *s, &opts.spool, None)?;
                let path = spool.path().to_path_buf();
                OpenedSource::Spooled { path, _keep: spool }
            }
            hzip_format::Source::Stdin => {
                return Err(VolError::Unsupported("vol.reject.stdin-source"));
            }
            hzip_format::Source::Volumes { .. } => {
                return Err(VolError::Unsupported("vol.reject.multi-volume"));
            }
        };
        Ok(Self {
            handler,
            src: opened,
            opts,
        })
    }

    /// 新建一条设备链（每次调用都新链 ⇒ 覆盖层为空，等价于 reset）。
    fn chain(&self, mode: WriteMode) -> VolResult<Chain> {
        Chain::open(
            hzip_format::Source::File(self.src.path().to_path_buf()),
            mode,
            &self.opts,
            None,
        )
    }

    /// 挂载卷里**可写**的文件系统（取第一个可挂载的分区），并返回该卷的**顶层行名**
    /// （写侧路由要用它剥前缀——与读侧投影同一份命名逻辑，见 `volume_root_names`）。
    ///
    /// # 为什么这里必须先列一次条目（2026-10-16 由 vmdk 代理实测暴露的缺陷）
    ///
    /// 驱动们的 `EntryId` 是**它们 `entries()` 里那张表的下标**（ext4 = inode/路径表、
    /// FAT/NTFS = id→路径表）；`entries()` 之前表是空的，而 `apply` 只认那张表 ⇒
    /// 不先列一次就 `apply`，`Change::Replace{entry}` / `Delete(id)` 必然报
    /// 「条目 ID 越界（先调 entries()）」。故挂载后**先列一次**（结果丢弃，要的是建表
    /// 这个副作用）。`Append` 不需要 id，早先用例因此没暴露它。
    ///
    /// # 单卷限制（本里程碑）
    ///
    /// 多分区镜像的变更需要**按分区归集**（core 侧还得知道每个条目的分区），
    /// 本期明确拒绝（`vol.unsupported.change-route`）而不是猜一个分区去写。
    fn mount_writable(
        &self,
        chain: &Chain,
        cancel: &hzip_progress::CancellationToken,
    ) -> VolResult<(Box<dyn FsVolume>, String)> {
        let vol = self.handler.open(chain.device(), &self.opts)?;
        let parts = vol.partitions()?;
        let has_table = !parts.is_empty();
        let views: Vec<crate::PartitionView> = if has_table {
            parts.clone()
        } else {
            vec![crate::partition::whole_view(chain.device().size_bytes())]
        };
        let roots = crate::reader::volume_root_names(&views, has_table);
        if views.len() > 1 {
            return Err(VolError::Unsupported("vol.unsupported.change-route"));
        }
        let candidates: Vec<Option<usize>> = if has_table {
            parts.iter().map(|p| Some(p.id)).collect()
        } else {
            vec![None]
        };
        for part in candidates {
            if let Some(fs) = vol.open_fs(part)?
                && fs.caps().write
            {
                // 建索引（缺这一步 Replace/Delete 全废，见上方文档）
                let _ = fs.entries(&crate::EntryLimit::default(), cancel)?;
                return Ok((fs, roots[0].0.clone()));
            }
        }
        Err(VolError::ReadOnly(crate::ReadOnlyReason::OuterNotEditable))
    }

    /// 应用整份变更集（顺序即契约：core 已定序）。
    ///
    /// **先把「档案级」变更翻成「卷内」变更**（[`route_change`]）：读侧投影把条目名
    /// 加了分区前缀（`Partition 1/etc/x`），而文件系统驱动要的是卷内相对路径
    /// （`/etc/x`）；id 也一样（投影 id 含根行，卷内 id 从 0 起）。不翻就会写到
    /// **卷内一个叫 `Partition 1/...` 的错路径**。
    fn apply_all(
        fs: &mut Box<dyn FsVolume>,
        root: &str,
        changes: &ChangeSet,
        ctx: &WriteCtx<'_>,
    ) -> VolResult<usize> {
        let mut n = 0usize;
        for change in &changes.changes {
            if ctx.cancel.is_cancelled() {
                return Err(VolError::Cancelled);
            }
            let routed = route_change(change, root)?;
            let local = local_of(&routed);
            fs.apply(&routed, local.as_deref(), ctx.cancel)?;
            n += 1;
        }
        Ok(n)
    }

    /// 应用 + 自检；任一步失败都让调用方**不提交**（原档因此不变）。
    fn prepare(
        &self,
        mode: WriteMode,
        changes: &ChangeSet,
        ctx: &WriteCtx<'_>,
    ) -> VolResult<(Chain, usize)> {
        let chain = self.chain(mode)?;
        let (mut fs, root) = self.mount_writable(&chain, ctx.cancel)?;
        let applied = Self::apply_all(&mut fs, &root, changes, ctx)?;
        fs.flush()?;
        let audit = fs.audit()?;
        if !audit.is_clean() {
            tracing::warn!(findings = ?audit.findings, "提交前自检未通过，放弃写入");
            return Err(VolError::Unsupported("vol.err.audit"));
        }
        Ok((chain, applied))
    }
}

impl<H: VolumeHandler + Send + Sync + 'static> ArchiveEditor for VolumeEditor<H> {
    fn rewrite(
        &self,
        changes: ChangeSet,
        dst: Sink,
        ctx: &WriteCtx<'_>,
    ) -> Result<WriteReport, HzipError> {
        // 组合/流源：变更先落在覆盖层，再把「当前视图」整量交割给 dst
        // （`.gz` 之类的外层必须重压，没有「只写脏块」的余地）。
        let (chain, applied) = self
            .prepare(WriteMode::Overlay, &changes, ctx)
            .map_err(HzipError::from)?;
        let bytes_out = match dst {
            Sink::File(path) => {
                // OS 级拷贝底档再回放脏块（比逐块读穿再写快，且不解码）
                std::fs::copy(self.src.path(), &path).map_err(|e| HzipError::Io {
                    source: e,
                    path: Some(path.clone()),
                })?;
                let target = fs_core::FileDevice::open_rw(&path).map_err(VolError::from)?;
                let n = chain
                    .commit(Some(&target), Some(ctx.cancel))
                    .map_err(HzipError::from)?;
                n.dirty_bytes
            }
            Sink::Stream(mut w) => chain
                .stream_into(&mut w, Some(ctx.cancel))
                .map_err(HzipError::from)?,
            Sink::Stdout => {
                let stdout = std::io::stdout();
                let mut lock = stdout.lock();
                chain
                    .stream_into(&mut lock, Some(ctx.cancel))
                    .map_err(HzipError::from)?
            }
            Sink::Split { .. } => {
                return Err(HzipError::Unsupported {
                    what: "vol.unsupported.split-image".into(),
                });
            }
        };
        Ok(WriteReport {
            entries_written: applied,
            bytes_in: 0,
            bytes_out,
            volume_paths: Vec::new(),
        })
    }

    fn write_in_place(
        &self,
        changes: ChangeSet,
        ctx: &WriteCtx<'_>,
    ) -> Result<WriteReport, HzipError> {
        // 写模式：由 core 经 `CreateOptions.volume_write` 下发（默认覆盖写）
        let mode = WriteMode::from(ctx.opts.volume_write);
        if mode == WriteMode::ReadOnly {
            return Err(VolError::ReadOnly(crate::ReadOnlyReason::BySetting).into());
        }
        if !self.src.is_real() {
            // 流源无处「就地回放」，必须走 rewrite（外层重写）——如实拒绝，不静默降级
            return Err(VolError::Unsupported("vol.reject.commit-spooled").into());
        }
        let (chain, applied) = self.prepare(mode, &changes, ctx).map_err(HzipError::from)?;
        let stats = chain.commit(None, Some(ctx.cancel)).map_err(|e| {
            // 提交中断：已回放的脏块留在原档（部分提交），如实记录
            tracing::error!("卷提交中断，镜像可能处于部分写入状态：{e}");
            HzipError::from(e)
        })?;
        Ok(WriteReport {
            entries_written: applied,
            bytes_in: 0,
            bytes_out: stats.dirty_bytes,
            volume_paths: Vec::new(),
        })
    }
}

/// 变更的本地文件（`Replace`/`Append` 才有）。
fn local_of(change: &Change) -> Option<PathBuf> {
    match change {
        Change::Replace { src, .. } | Change::Append { src, .. } => Some(src.clone()),
        _ => None,
    }
}

/// 「档案级」变更 → 「卷内」变更（**单卷**口径；多卷在 `mount_writable` 已拒）。
///
/// | 变更 | 翻译 |
/// |---|---|
/// | `Replace{entry, ..}` / `Delete(id)` | 全局 id → 卷内 id：投影是 `[根行] ++ [卷内条目]`，故 `local = id - 1`（`id == 0` 是根行本身 ⇒ 拒绝） |
/// | `Append{name}` / `MakeDir{name}` | 剥 `"{根名}/"` 前缀 ⇒ 卷内相对路径（两者契约都是「档案内**完整**路径」） |
/// | `Rename{from, to}` | `from` 必是完整路径 ⇒ 剥前缀；`to` **两种形态都合法**（见下） |
/// | 其它（`#[non_exhaustive]` 新变体） | 原样交给驱动（由驱动决定支不支持） |
///
/// # `Rename.to` 的两种形态
///
/// [`Change::Rename`] 的文档契约是「`to` 不含父目录段」（裸叶名；驱动按
/// `parent(from) + '/' + to` 拼回去），但 GUI 传的是**完整路径**
/// （`app/state.rs::commit_name_edit` 传的 `new_full`），而 ext4/FAT 驱动两种都吃
/// （`rename_target` / `destination_name` 里的 `to.contains('/')` 分支）。故这里：
/// 带本卷前缀 ⇒ 剥；**不带 `/` 的裸叶名 ⇒ 原样放行**；带 `/` 却无本卷前缀 ⇒ 拒绝
/// （它指向别处，猜不得）。
///
/// 名字**必须以「根名 + `/`」开头**：不匹配说明这个变更不是冲这个卷来的（或界面传来的
/// 是错的），此时**拒绝**（`vol.unsupported.change-route`）比猜一个路径去写安全得多。
fn route_change(change: &Change, root: &str) -> VolResult<Change> {
    let strip = |name: &EntryName| -> VolResult<EntryName> {
        let raw = name.as_str();
        // 尾 `/` 是**语义标记**（ext4/FAT 的 `Append` 契约：名字以 `/` 结尾 ⇒ **只建目录**），
        // 而 `parse_entry_name` 按 N5 丢弃空段 ⇒ 会把 `etc/sub/` 规范化成 `etc/sub`，
        // 于是驱动转去建一个**空文件**。故先取下标记，规范化后再挂回去。
        let dir_marker = raw.ends_with('/');
        let rel = raw
            .strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or(VolError::Unsupported("vol.unsupported.change-route"))?;
        if rel.is_empty() {
            return Err(VolError::Unsupported("vol.unsupported.change-route"));
        }
        let normalized = hzip_format::parse_entry_name(rel)
            .map_err(|e| VolError::Image(format!("卷内路径不合法（{rel}）：{e}")))?;
        Ok(if dir_marker {
            EntryName::new(format!("{}/", normalized.as_str()))
        } else {
            normalized
        })
    };
    let local_id = |id: usize| -> VolResult<usize> {
        // 0 是根行（分区/整卷本身），不是卷内条目
        id.checked_sub(1)
            .ok_or(VolError::Unsupported("vol.unsupported.change-route"))
    };
    Ok(match change {
        Change::Replace { entry, src } => Change::Replace {
            entry: local_id(*entry)?,
            src: src.clone(),
        },
        Change::Delete(id) => Change::Delete(local_id(*id)?),
        Change::Append { src, name } => Change::Append {
            src: src.clone(),
            name: strip(name)?,
        },
        Change::MakeDir { name } => Change::MakeDir { name: strip(name)? },
        Change::Rename { from, to } => {
            let to = if to.as_str().contains('/') {
                strip(to)? // 完整路径（GUI 形态）
            } else {
                to.clone() // 裸叶名：驱动按 `parent(from) + '/' + to` 拼
            };
            Change::Rename {
                from: strip(from)?,
                to,
            }
        }
        other => other.clone(),
    })
}

/// 供 handler 复用：把 `EntryName` 转成文件系统内的相对路径（去掉潜在前导 `/`）。
#[must_use]
pub fn entry_rel(name: &EntryName) -> String {
    name.as_str().trim_start_matches('/').to_string()
}
