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
//! # 多分区镜像：按分区归集（2026-10-16 起）
//!
//! 一个镜像可以有多个分区（EFI + root 是常态），读侧投影把每个分区做成一个**顶层行**
//! 并给其条目加 `"{分区行名}/"` 前缀；写侧因此必须**按分区归集**：
//!
//! ```text
//! 读侧投影 id（顺序编号）        写侧路由（route_to）
//! 0  顶层行：Partition 1         名字型变更 → 按 "{根名}/" 前缀选分区
//! 1  条目：Partition 1/etc       id 型变更 → 按 [entry_base, entry_base+count) 选分区
//! 2  条目：Partition 1/etc/x      ↓
//! 3  顶层行：Partition 2          目标分区的 FsVolume::apply（卷内路径 / 卷内 id）
//! 4  条目：Partition 2/boot
//! ```
//!
//! 两张表在 [`VolumeEditor::mount_routable`] 里**同时**建出来（同一遍列条目），且与读侧
//! 同序、同限、同命名（`volume_root_names` 是唯一事实源）⇒ 不会漂移。指不到任何分区的
//! 变更一律拒绝（`vol.unsupported.change-route`），**不猜**。
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
use hzip_format::{
    ArchiveEditor, Change, ChangeSet, EntryId, EntryName, Sink, WriteCtx, WriteReport,
};

/// 「这条变更指不到任何分区」的稳定拒绝键。
///
/// 命中条件：名字不带任何分区根名前缀、或 id 落在所有分区的条目区间之外（含顶层行）。
/// 拒绝好过猜——猜错分区会改坏**另一个**文件系统。
const KEY_CHANGE_ROUTE: &str = "vol.unsupported.change-route";

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

/// 一个**可写分区**的路由目标（[`VolumeEditor::mount_routable`] 的产物）。
///
/// 三件事必须齐全，缺一条就会写错地方：
/// - `root`：投影里的顶层行名（= 条目名前缀，唯一事实源 `volume_root_names`）；
/// - `entry_base` / `count`：本分区在**读侧投影**里占的 id 区间
///   （`[entry_base, entry_base + count)`）——id 型变更（`Replace`/`Delete`）靠它定位；
/// - `fs`：本分区已挂载（且已建索引）的文件系统，变更直接交给它。
struct Mounted {
    root: String,
    fs: Box<dyn FsVolume>,
    entry_base: usize,
    count: usize,
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

    /// 挂载卷里**每个可写分区**，并建出「全局投影 id ↔ （分区, 卷内 id）」的对照表。
    ///
    /// # 为什么这里必须先列一次条目（2026-10-16 由 vmdk 代理实测暴露的缺陷）
    ///
    /// 驱动们的 `EntryId` 是**它们 `entries()` 里那张表的下标**（ext4 = inode/路径表、
    /// FAT/NTFS = id→路径表）；`entries()` 之前表是空的，而 `apply` 只认那张表 ⇒
    /// 不先列一次就 `apply`，`Change::Replace{entry}` / `Delete(id)` 必然报
    /// 「条目 ID 越界（先调 entries()）」。
    ///
    /// # 列条目同时承担 **id 算术**（多分区写回的前提）
    ///
    /// 读侧投影（[`hzip_vol::reader::VolumeReader::build`]）的全局 id 是**顺序编号**：
    /// 逐分区依次「顶层行占 1 个 id → 该分区逐条占 id」，且同序同限（`EntryLimit::
    /// default()`）。故写侧要按 id 找到分区，必须**按同样顺序把每个分区都列一遍**并
    /// 累加偏移 —— 这也是为什么本函数对**不可写**的分区同样要列（它们照样占 id，
    /// 漏掉就会把变更写到隔壁分区去）。两份投影因此不会漂移（同序、同限、同命名
    /// `volume_root_names`）。
    ///
    /// # 单分区与多分区同一条路
    ///
    /// 曾经「多分区一律拒绝」（`vol.unsupported.change-route`，2026-10-16 用户实报
    /// 「该镜像含多个卷（分区），暂不支持在其内部编辑」）；现在按分区归集，只有
    /// **认不出目标分区**的变更才拒绝（见 [`route_to`]）——拒绝永远优于猜一个分区去写。
    fn mount_routable(
        &self,
        chain: &Chain,
        cancel: &hzip_progress::CancellationToken,
    ) -> VolResult<Vec<Mounted>> {
        let vol = self.handler.open(chain.device(), &self.opts)?;
        let parts = vol.partitions()?;
        let has_table = !parts.is_empty();
        let views: Vec<crate::PartitionView> = if has_table {
            parts
        } else {
            vec![crate::partition::whole_view(chain.device().size_bytes())]
        };
        let roots = crate::reader::volume_root_names(&views, has_table);
        let mut out: Vec<Mounted> = Vec::new();
        // 下一个**全局投影 id**（读侧同口径：顶层行先占一个，再逐条条目）
        let mut next_id = 0usize;
        for (idx, _view) in views.iter().enumerate() {
            let (root, _label) = roots[idx].clone();
            let part = if has_table { Some(idx) } else { None };
            next_id += 1; // 顶层行本身（`push_dir`）
            let Some(fs) = vol.open_fs(part)? else {
                continue; // 未识别卷：只有顶层行，0 条目
            };
            let tree = fs.entries(&crate::EntryLimit::default(), cancel)?;
            let count = tree.entries.len();
            if fs.caps().write {
                out.push(Mounted {
                    root,
                    fs,
                    entry_base: next_id,
                    count,
                });
            }
            next_id += count;
        }
        if out.is_empty() {
            // 没有任何分区可写（全部只读/未识别）——理由与单分区时代同一档
            return Err(VolError::ReadOnly(crate::ReadOnlyReason::OuterNotEditable));
        }
        Ok(out)
    }

    /// 应用整份变更集（顺序即契约：core 已定序）。
    ///
    /// 逐条经 [`route_to`] 定目标分区 + 翻成「卷内」变更：读侧投影给条目名加了分区
    /// 前缀（`Partition 1/etc/x`）、id 也整体偏移；而文件系统驱动要的是卷内相对路径与
    /// 本分区局部 id。不翻就会写到**卷内一个叫 `Partition 1/...` 的错路径**。
    fn apply_all(
        mounted: &mut [Mounted],
        changes: &ChangeSet,
        ctx: &WriteCtx<'_>,
    ) -> VolResult<usize> {
        let mut n = 0usize;
        for change in &changes.changes {
            if ctx.cancel.is_cancelled() {
                return Err(VolError::Cancelled);
            }
            let (ix, routed) = route_to(change, mounted)?;
            let local = local_of(&routed);
            mounted[ix]
                .fs
                .apply(&routed, local.as_deref(), ctx.cancel)?;
            n += 1;
        }
        Ok(n)
    }

    /// 应用 + 自检（**逐分区**）；任一步失败都让调用方**不提交**（原档因此不变）。
    fn prepare(
        &self,
        mode: WriteMode,
        changes: &ChangeSet,
        ctx: &WriteCtx<'_>,
    ) -> VolResult<(Chain, usize)> {
        let chain = self.chain(mode)?;
        let mut mounted = self.mount_routable(&chain, ctx.cancel)?;
        let applied = Self::apply_all(&mut mounted, changes, ctx)?;
        for m in mounted.iter_mut() {
            m.fs.flush()?;
            let audit = m.fs.audit()?;
            if !audit.is_clean() {
                tracing::warn!(
                    root = %m.root,
                    findings = ?audit.findings,
                    "提交前自检未通过，放弃写入"
                );
                return Err(VolError::Unsupported("vol.err.audit"));
            }
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

/// 「档案级」变更 → **（目标分区, 卷内变更）**。
///
/// | 变更 | 定位方式 | 翻译 |
/// |---|---|---|
/// | `Replace{entry, ..}` / `Delete(id)` | **id 区间**（`entry_base ≤ id < entry_base + count`） | 全局 id → 本分区卷内 id（`id - entry_base`）。落在任何分区区间之外（含顶层行本身）⇒ 拒绝 |
/// | `Append{name}` / `MakeDir{name}` | **名字前缀**（`"{根名}/"`） | 剥前缀 ⇒ 卷内相对路径（两者契约都是「档案内**完整**路径」） |
/// | `Rename{from, to}` | `from` 按名字前缀 | `from` 剥前缀；`to` **两种形态都合法**（见下） |
/// | 其它（`#[non_exhaustive]` 新变体） | — | 单分区时原样交给驱动（保持旧行为）；多分区时**无法判定目标 ⇒ 拒绝** |
///
/// # `Rename.to` 的两种形态
///
/// [`Change::Rename`] 的文档契约是「`to` 不含父目录段」（裸叶名；驱动按
/// `parent(from) + '/' + to` 拼回去），但 GUI 传的是**完整路径**
/// （`app/state.rs::commit_name_edit` 传的 `new_full`），而 ext4/FAT 驱动两种都吃
/// （`rename_target` / `destination_name` 里的 `to.contains('/')` 分支）。故这里：
/// 带**本分区**前缀 ⇒ 剥；**不带 `/` 的裸叶名 ⇒ 原样放行**；带 `/` 却指向**别的分区**
/// 或没有任何分区前缀 ⇒ 拒绝（跨分区改名/来路不明，猜不得）。
///
/// # 为什么宁可拒绝
///
/// 名字**必须以「某根名 + `/`」开头**、id **必须落在某分区的条目区间内**——两者都是
/// 「这条变更确实是冲那个分区来的」的证据。对不上时拒绝（`vol.unsupported.change-route`）
/// 远比猜一个分区去写安全：猜错的后果是**改坏另一个分区的文件系统**。
fn route_to(change: &Change, mounted: &[Mounted]) -> VolResult<(usize, Change)> {
    Ok(match change {
        Change::Replace { entry, src } => {
            let (ix, local) = split_id(*entry, mounted)?;
            (
                ix,
                Change::Replace {
                    entry: local,
                    src: src.clone(),
                },
            )
        }
        Change::Delete(id) => {
            let (ix, local) = split_id(*id, mounted)?;
            (ix, Change::Delete(local))
        }
        Change::Append { src, name } => {
            let (ix, rel) = strip_root(name, mounted)?;
            (
                ix,
                Change::Append {
                    src: src.clone(),
                    name: rel,
                },
            )
        }
        Change::MakeDir { name } => {
            let (ix, rel) = strip_root(name, mounted)?;
            (ix, Change::MakeDir { name: rel })
        }
        Change::Rename { from, to } => {
            let (ix, rel_from) = strip_root(from, mounted)?;
            let rel_to = if to.as_str().contains('/') {
                let (jx, rel) = strip_root(to, mounted)?;
                if jx != ix {
                    // 跨分区改名：语义含糊（两个分区各有一套 id/路径空间）⇒ 拒绝
                    return Err(VolError::Unsupported(KEY_CHANGE_ROUTE));
                }
                rel
            } else {
                to.clone() // 裸叶名：驱动按 `parent(from) + '/' + to` 拼
            };
            (
                ix,
                Change::Rename {
                    from: rel_from,
                    to: rel_to,
                },
            )
        }
        // 未登记的新变体：单分区时无从歧义（旧行为原样放行），多分区时无法判定目标
        other => {
            if mounted.len() == 1 {
                (0, other.clone())
            } else {
                return Err(VolError::Unsupported(KEY_CHANGE_ROUTE));
            }
        }
    })
}

/// **id 型**定位：全局投影 id → `(分区下标, 本分区卷内 id)`。
///
/// 区间来自 [`VolumeEditor::mount_routable`] 的 `entry_base`/`count`（与读侧投影同序同限）。
/// 顶层行本身不在任何条目区间内 ⇒ 命中不了 ⇒ 拒绝（改分区行没有意义）。
fn split_id(id: EntryId, mounted: &[Mounted]) -> VolResult<(usize, usize)> {
    mounted
        .iter()
        .enumerate()
        .find(|(_, m)| id >= m.entry_base && id < m.entry_base + m.count)
        .map(|(ix, m)| (ix, id - m.entry_base))
        .ok_or(VolError::Unsupported(KEY_CHANGE_ROUTE))
}

/// **名字型**定位：`"{根名}/{卷内路径}"` → `(分区下标, 卷内相对路径)`。
///
/// 尾 `/` 是**语义标记**（ext4/FAT 的 `Append` 契约：名字以 `/` 结尾 ⇒ **只建目录**），
/// 而 `parse_entry_name` 按 N5 丢弃空段 ⇒ 会把 `etc/sub/` 规范化成 `etc/sub`，
/// 于是驱动转去建一个**空文件**。故先取下标记，规范化后再挂回去。
fn strip_root(name: &EntryName, mounted: &[Mounted]) -> VolResult<(usize, EntryName)> {
    let raw = name.as_str();
    let dir_marker = raw.ends_with('/');
    for (ix, m) in mounted.iter().enumerate() {
        let Some(rel) = raw
            .strip_prefix(m.root.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            continue;
        };
        if rel.is_empty() {
            // 指向根行自身：不是卷内条目
            return Err(VolError::Unsupported(KEY_CHANGE_ROUTE));
        }
        let normalized = hzip_format::parse_entry_name(rel)
            .map_err(|e| VolError::Image(format!("卷内路径不合法（{rel}）：{e}")))?;
        let rel = if dir_marker {
            EntryName::new(format!("{}/", normalized.as_str()))
        } else {
            normalized
        };
        return Ok((ix, rel));
    }
    // 没有任何根名匹配 ⇒ 这条变更不是冲这个卷来的（或界面传来的是错的）
    Err(VolError::Unsupported(KEY_CHANGE_ROUTE))
}

/// 供 handler 复用：把 `EntryName` 转成文件系统内的相对路径（去掉潜在前导 `/`）。
#[must_use]
pub fn entry_rel(name: &EntryName) -> String {
    name.as_str().trim_start_matches('/').to_string()
}
