//! 卷 → `hzip-format` 的**读取器投影**：**分区即顶层目录**。
//!
//! # 投影形状（用户口径 2026-10-16 的落点）
//!
//! ```text
//! Partition 1/                 ← 分区行（目录；名字 = 分区 label，无 label 则 `Partition N`）
//! Partition 1/EFI/             ← 分区内文件系统的目录
//! Partition 1/EFI/BOOT/BOOTX64.EFI
//! root/                        ← 第二个分区（label = "root"）
//! root/etc/fstab
//! ```
//!
//! 为什么是**同一层的顶层目录**而不是「嵌套会话的一层」：`hzip-core` 的
//! `Browser::enter` 只按**条目**下钻（且按扩展名选 handler），分区不是条目——
//! 嵌不进那条路由（`docs/tasks/T-IMG/README.md` §4.3）。故分区以目录行出现，
//! 之后一切（浏览/预览/提取/编辑）与原档案**完全同链路**。
//!
//! # 名字的两条规则
//!
//! | 情况 | 名字 | 备注 |
//! |---|---|---|
//! | 分区有 label | `label`（清理控制字符、超长截断） | 用户口径：「分区名称使用分区 label」 |
//! | 无 label | `Partition {n}`（n 从 1 起） | **ASCII 稳定**，只作树路径；界面按语言显示「分区 N」 |
//! | 重名 | 追加 ` (2)`,` (3)`… | 树路径必须唯一，否则导航/选择会串行 |
//!
//! 本地化**不进这个名字**：名字是数据（树路径、剪贴板、日志都要稳定），语言相关的显示
//! 由界面按 [`hzip_format::VolumeRowMeta`]（本模块经 `volume_rows()` 提供）渲染。
//!
//! # 惰性读
//!
//! `read_entry` 返回的是**流式壳**（[`FsEntryReader`]，1 MiB 分块向文件系统要数据），
//! 不落临时文件——预览只读头部、提取按块推进，大文件也不会先写一份临时副本。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use hzip_error::HzipError;
use hzip_format::{
    ArchiveReader, EntryId, EntryInfo, ExtractCtx, ExtractReport, FormatCaps, PasswordRef,
    Selection, VolumeRowMeta,
};

use crate::error::{ReadOnlyReason, VolError, VolResult};
use crate::{EntryLimit, FsVolume, Geometry, PartitionView, VolCaps, Volume};

/// 流式读取的分块大小（1 MiB：与仓库其它桥接循环一致）。
const READ_CHUNK: usize = 1 << 20;

/// 一条投影记录：全局 id ↔ （分区, 该文件系统内的 id / 路径）。
#[derive(Debug)]
struct Record {
    info: EntryInfo,
    part: Option<usize>,
    local: EntryId,
}

/// 分区在投影里的元信息（顶层行）。
#[derive(Debug, Clone)]
struct RootRow {
    meta: VolumeRowMeta,
}

/// 卷读取器：把 `Volume` + 各分区已挂载的 `FsVolume` 投影成 `ArchiveReader`。
pub struct VolumeReader {
    vol: Box<dyn Volume>,
    geometry: Geometry,
    vol_caps: VolCaps,
    records: Vec<Record>,
    roots: Vec<RootRow>,
    mounts: Mutex<HashMap<usize, Arc<dyn FsVolume>>>,
    truncated: bool,
    skipped: u64,
}

impl std::fmt::Debug for VolumeReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VolumeReader")
            .field("geometry", &self.geometry)
            .field("rows", &self.roots.len())
            .field("entries", &self.records.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

impl VolumeReader {
    /// 打开并**建索引**：探测分区 → 逐分区挂载文件系统 → 递归列树 → 扁平投影。
    ///
    /// 索引是急切的（`ArchiveReader::entries` 的契约如此）；`limit` 与 `cancel`
    /// 在列树过程中生效，超限置 `truncated` 并在报告里如实标注。
    pub fn build(
        vol: Box<dyn Volume>,
        limit: &EntryLimit,
        cancel: &hzip_progress::CancellationToken,
    ) -> VolResult<Self> {
        let geometry = vol.geometry();
        let vol_caps = vol.caps();
        let parts = vol.partitions()?;
        let views: Vec<PartitionView> = if parts.is_empty() {
            vec![crate::partition::whole_view(geometry.virtual_size)]
        } else {
            parts
        };

        let mut reader = Self {
            vol,
            geometry,
            vol_caps,
            records: Vec::new(),
            roots: Vec::new(),
            mounts: Mutex::new(HashMap::new()),
            truncated: false,
            skipped: 0,
        };
        reader.index(&views, limit, cancel)?;
        Ok(reader)
    }

    /// 逐分区建索引（名字唯一化 → 挂载 → 递归列树 → 前缀投影）。
    fn index(
        &mut self,
        views: &[PartitionView],
        limit: &EntryLimit,
        cancel: &hzip_progress::CancellationToken,
    ) -> VolResult<()> {
        let has_table = self
            .vol
            .partitions()
            .map(|p| !p.is_empty())
            .unwrap_or(false);
        let roots = volume_root_names(views, has_table);
        for (idx, view) in views.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(VolError::Cancelled);
            }
            // 1) 顶层行名字：label 优先，其次 `Partition N`；重名追加序号
            //    （与写侧路由**同一份**命名逻辑，见 `volume_root_names` 的文档）
            let (name, label) = roots[idx].clone();
            // 2) 挂载该分区的文件系统（无表 ⇒ 整卷；未知文件系统 ⇒ None）
            let part = if has_table { Some(idx) } else { None };
            let mount = self
                .vol
                .open_fs(part)
                .map(|maybe| maybe.map(Arc::<dyn FsVolume>::from));
            let fs = match mount {
                Ok(fs) => fs,
                Err(e) => {
                    // 挂载失败不是「未知卷」：如实上抛，让用户看到原因。
                    return Err(e);
                }
            };
            let fs_name = fs.as_ref().map(|f| f.fs_id().to_string());
            self.roots.push(RootRow {
                meta: VolumeRowMeta {
                    name: name.clone(),
                    label,
                    kind: Some(view.type_name.clone()),
                    fs: fs_name,
                    index: idx,
                    start: view.start,
                    len: view.len,
                },
            });
            // 3) 顶层目录行本身
            self.push_dir(&name)?;
            let Some(fs) = fs else {
                continue; // 未识别卷：只有顶层行（界面标「未知卷」）
            };
            let tree = fs.entries(limit, cancel)?;
            self.skipped += tree.skipped;
            self.truncated |= tree.truncated;
            for e in &tree.entries {
                // **全局重编号**：`entries()` 里 `info.id` 必须是 `read_entry` 认的 id，
                // 即本投影 records 的下标。直接沿用文件系统的**本地 id** 会与分区行
                // 撞车（分区行 0 与首个文件 0）⇒ 按 `info.id` 去读会读到别的条目
                // （实测：读到目录、返回空流）。本地 id 另存 `Record::local`。
                let mut info = prefixed(e, &name);
                info.id = self.records.len();
                self.records.push(Record {
                    info,
                    part,
                    local: e.id,
                });
            }
            self.mounts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(idx, fs);
        }
        Ok(())
    }

    /// 追加一个顶层目录行（分区行；无数据流）。
    fn push_dir(&mut self, name: &str) -> VolResult<()> {
        let info = dir_info(self.records.len(), name)?;
        self.records.push(Record {
            info,
            part: None,
            local: 0,
        });
        Ok(())
    }

    /// 已挂载的分区文件系统（内部）。
    fn mount_of(&self, part: usize) -> Option<Arc<dyn FsVolume>> {
        self.mounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&part)
            .cloned()
    }

    /// 投影出的全局 id → 记录。
    fn record(&self, id: EntryId) -> VolResult<&Record> {
        self.records
            .get(id)
            .ok_or_else(|| VolError::Image(format!("条目 {id} 不存在")))
    }

    /// 只读（不可写）的**理由**：卷级理由优先，其次文件系统级。
    #[must_use]
    pub fn read_only_reason(&self) -> Option<ReadOnlyReason> {
        if let Some(r) = self.vol_caps.read_only_reason {
            return Some(r);
        }
        for root in &self.roots {
            let reason = self
                .mount_of(root.meta.index)
                .and_then(|fs| fs.caps().read_only_reason);
            if reason.is_some() {
                return reason;
            }
        }
        None
    }

    /// 能力矩阵（`edit` 取各分区文件系统的**合取**：任一分区只读 ⇒ 整体不可编辑）。
    fn caps_of(&self) -> FormatCaps {
        let writable = self.vol_caps.writable
            && self
                .roots
                .iter()
                .filter_map(|r| self.mount_of(r.meta.index))
                .all(|fs| fs.caps().write);
        FormatCaps {
            read: true,
            write: false,
            append: false,
            edit: writable,
            ..Default::default()
        }
    }
}

impl ArchiveReader for VolumeReader {
    fn caps(&self) -> FormatCaps {
        self.caps_of()
    }

    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
        Ok(self.records.iter().map(|r| r.info.clone()).collect())
    }

    fn read_entry(
        &self,
        id: EntryId,
        _pw: PasswordRef<'_>,
    ) -> Result<Box<dyn Read + Send>, HzipError> {
        let rec = self.record(id)?;
        let (Some(part), size) = (rec.part, rec.info.size_raw) else {
            // 顶层分区行（目录）没有数据流：给空流比报错更有用（界面偶尔会取它测大小）
            return Ok(Box::new(std::io::empty()));
        };
        let fs = self.mount_of(part).ok_or_else(|| HzipError::Unsupported {
            what: "vol.err.partition-not-mounted".into(),
        })?;
        Ok(Box::new(FsEntryReader {
            fs,
            local: rec.local,
            offset: 0,
            remaining: size,
        }))
    }

    fn extract(
        &self,
        sel: &Selection,
        ctx: &ExtractCtx<'_>,
        dest: &Path,
    ) -> Result<ExtractReport, HzipError> {
        let ids = self.resolve_selection(sel)?;
        let mut report = ExtractReport::default();
        for id in ids {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            let rec = &self.records[id];
            let out_path = dest.join(rec.info.name.as_str());
            if rec.info.is_dir {
                std::fs::create_dir_all(&out_path).map_err(HzipError::from)?;
                report.entries_done += 1;
                continue;
            }
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(HzipError::from)?;
            }
            let r = std::fs::File::create(&out_path)
                .map_err(HzipError::from)
                .and_then(|mut f| {
                    let mut rdr = self.read_entry(id, PasswordRef::none())?;
                    std::io::copy(&mut rdr, &mut f).map_err(HzipError::from)?;
                    Ok(())
                });
            match r {
                Ok(()) => {
                    report.entries_done += 1;
                    report.bytes_written += rec.info.size_raw;
                    if let Some(mt) = rec.info.mtime {
                        let _ = hzip_platform::metadata::set_mtime(&out_path, mt);
                    }
                }
                Err(e) => {
                    if ctx.best_effort {
                        report.failures.push((id, Box::new(e)));
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Ok(report)
    }

    /// 对卷而言「测试」= 各分区文件系统的一致性自检（`audit`）。
    fn test(&self, _ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
        let mut report = ExtractReport::default();
        for root in &self.roots {
            let Some(fs) = self.mount_of(root.meta.index) else {
                continue;
            };
            match fs.audit() {
                Ok(summary) => {
                    if summary.skipped {
                        report.failures.push((
                            0,
                            Box::new(HzipError::Unsupported {
                                what: summary
                                    .findings
                                    .first()
                                    .cloned()
                                    .unwrap_or_else(|| "vol.err.audit-skipped".to_string())
                                    .into(),
                            }),
                        ));
                    }
                    for f in summary.findings.iter().filter(|_| !summary.skipped) {
                        report.failures.push((
                            0,
                            Box::new(HzipError::Unsupported {
                                what: f.clone().into(),
                            }),
                        ));
                    }
                }
                Err(e) => {
                    report.failures.push((0, Box::new(e.into())));
                }
            }
        }
        report.entries_done = self.records.iter().filter(|r| !r.info.is_dir).count();
        Ok(report)
    }

    fn volume_rows(&self) -> Vec<VolumeRowMeta> {
        self.roots.iter().map(|r| r.meta.clone()).collect()
    }
}

impl VolumeReader {
    /// `Selection` → 全局 id（与 cpio/tar 同口径：越界 id 报坏档；未变体拒绝）。
    fn resolve_selection(&self, sel: &Selection) -> Result<Vec<EntryId>, HzipError> {
        match sel {
            Selection::All => Ok((0..self.records.len()).collect()),
            Selection::Ids(ids) => {
                if ids.iter().any(|&i| i >= self.records.len()) {
                    return Err(HzipError::Format {
                        format: hzip_error::FormatId::from("vol"),
                        kind: hzip_error::FormatErrorKind::BadHeader,
                    });
                }
                Ok(ids.clone())
            }
            Selection::Names(names) => Ok(self
                .records
                .iter()
                .enumerate()
                .filter(|(_, r)| names.contains(&r.info.name))
                .map(|(i, _)| i)
                .collect()),
            _ => Err(HzipError::Unsupported {
                what: "vol.err.unknown-selection".into(),
            }),
        }
    }

    /// 索引是否被 `EntryLimit` 截断（界面据此提示「部分列出」）。
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// 被跳过的目录项数（设备节点等）。
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// 几何（信息面板用）。
    #[must_use]
    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    /// 卷能力（提示与理由）。
    #[must_use]
    pub fn vol_caps(&self) -> &VolCaps {
        &self.vol_caps
    }
}

/// 顶层行名字（全部**ASCII 稳定**，语言相关的显示由界面按元数据渲染）：
///
/// | 情况 | 名字 |
/// |---|---|
/// | 有真实标签（GPT 分区名） | 标签本身（清理控制字符、超长截断） |
/// | 无标签的分区 | `Partition {n}`（n 从 1 起） |
/// | 无分区表的整卷 | `Volume` |
/// | 重名 | 追加 ` (2)`,` (3)`…（树路径必须唯一） |
///
/// **唯一事实源**：读侧投影（[`VolumeReader`]）与写侧路由（[`crate::editor`] 把
/// 「档案级路径」翻成「卷内相对路径」）都调它 —— 两边各写一份必然漂移，
/// 而漂移的后果是**写到错路径**（例如把 `Partition 1/etc/x` 当成卷内真路径）。
pub(crate) fn volume_root_names(
    views: &[PartitionView],
    has_table: bool,
) -> Vec<(String, Option<String>)> {
    let mut used: Vec<String> = Vec::new();
    views
        .iter()
        .enumerate()
        .map(|(idx, view)| {
            let label = view
                .label
                .as_ref()
                .map(|l| clean(l))
                .filter(|l| !l.is_empty());
            let base = label.clone().unwrap_or_else(|| {
                if has_table {
                    format!("Partition {}", idx + 1)
                } else {
                    "Volume".to_string()
                }
            });
            let mut name = base.clone();
            let mut n = 2;
            while used.contains(&name) {
                name = format!("{base} ({n})");
                n += 1;
            }
            used.push(name.clone());
            (name, label)
        })
        .collect()
}

/// 清理名字：去控制字符、裁到 48 字符（名字来自别人的磁盘，不可信）。
fn clean(s: &str) -> String {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    if cleaned.chars().count() <= 48 {
        return cleaned;
    }
    cleaned.chars().take(47).collect::<String>() + "…"
}

/// 给条目名加分区前缀（`parse_entry_name` 会做规范化与合法性校验；
/// 万一失败就保留原名——**不丢条目**比名字更漂亮重要）。
fn prefixed(e: &EntryInfo, root: &str) -> EntryInfo {
    let mut info = e.clone();
    let joined = format!("{root}/{}", e.name.as_str());
    if let Ok(name) = hzip_format::parse_entry_name(&joined) {
        info.name = name;
    } else {
        tracing::warn!(%joined, "分区前缀命名失败，保留原名");
    }
    info
}

/// 造一条目录目（`EntryInfo` 无 `Default`，字段逐个给；与 tar/cpio 同款）。
fn dir_info(id: EntryId, name: &str) -> VolResult<EntryInfo> {
    let name = hzip_format::parse_entry_name(name)
        .map_err(|e| VolError::Image(format!("分区名不合法（{name}）：{e}")))?;
    Ok(EntryInfo {
        id,
        name,
        is_dir: true,
        is_symlink: false,
        symlink_target: None,
        size_raw: 0,
        size_packed: 0,
        mtime: None,
        mode: Some(0o755),
        attributes: hzip_format::EntryAttrs::default(),
        encrypted: false,
        header_encrypted: false,
        crc32: None,
        codec: None,
        name_encoding: hzip_format::NameEncoding::Utf8,
        encryption: None,
        compression_level: None,
    })
}

/// 惰性条目流：按 1 MiB 向文件系统要数据（不落临时文件）。
struct FsEntryReader {
    fs: Arc<dyn FsVolume>,
    local: EntryId,
    offset: u64,
    remaining: u64,
}

impl Read for FsEntryReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min(READ_CHUNK).min(self.remaining as usize);
        match self.fs.read_at(self.local, self.offset, &mut buf[..want]) {
            Ok(0) => Ok(0),
            Ok(n) => {
                self.offset += n as u64;
                self.remaining = self.remaining.saturating_sub(n as u64);
                Ok(n)
            }
            Err(e) => Err(std::io::Error::other(e.to_string())),
        }
    }
}

/// 便捷：把整条条目读进内存（小文件；提取走流式路径）。
pub fn read_to_vec(reader: &VolumeReader, id: EntryId) -> VolResult<Vec<u8>> {
    let rec = reader.record(id)?;
    let (Some(part), size) = (rec.part, rec.info.size_raw) else {
        return Ok(Vec::new());
    };
    let fs = reader
        .mount_of(part)
        .ok_or(VolError::Unsupported("vol.err.partition-not-mounted"))?;
    let mut out = vec![0u8; usize::try_from(size).unwrap_or(0)];
    let mut off = 0usize;
    while off < out.len() {
        let n = fs.read_at(rec.local, off as u64, &mut out[off..])?;
        if n == 0 {
            break;
        }
        off += n;
    }
    out.truncate(off);
    Ok(out)
}

/// 把整卷写进外层流（组合链提交用；转调 `Chain::stream_into` 之外的便捷入口）。
pub fn stream_reader_tree(
    reader: &VolumeReader,
    w: &mut dyn Write,
    cancel: Option<&hzip_progress::CancellationToken>,
) -> VolResult<u64> {
    let mut buf = vec![0u8; READ_CHUNK];
    let mut total = 0u64;
    for id in 0..reader.records.len() {
        let rec = &reader.records[id];
        if rec.info.is_dir {
            continue;
        }
        let mut rdr = reader
            .read_entry(id, PasswordRef::none())
            .map_err(|e| match e {
                HzipError::Unsupported { what } => VolError::Unsupported(match what.as_ref() {
                    "vol.err.partition-not-mounted" => "vol.err.partition-not-mounted",
                    _ => "vol.err.read-entry",
                }),
                _ => VolError::Image("读取条目失败".to_string()),
            })?;
        loop {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(VolError::Cancelled);
            }
            let n = rdr.read(&mut buf).map_err(VolError::Io)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n]).map_err(VolError::Io)?;
            total += n as u64;
        }
    }
    Ok(total)
}
