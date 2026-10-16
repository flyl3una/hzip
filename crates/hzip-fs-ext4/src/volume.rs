//! 已挂载的 ext4 视图（[`hzip_vol::FsVolume`] 实现）：递归列树 + 按 inode 惰性读 +
//! 五类变更。
//!
//! # `EntryId` → inode / 路径的映射
//!
//! `entries()` 递归列树时建两张平行的表（`EntryId` 即下标）：**inode**（`read_at`/
//! `size_of` 用）与**规范化路径**（`apply_*` 用——上游的写接口都是路径版）。
//! `read_at`/`size_of` 因此是「惰性、乱序、可重复」的（契约红线 2）：它们只查表 +
//! 一次 `read_ino`/`stat_ino`，不依赖任何游标。
//!
//! 两表在一次 `entries()` 里整体重建（调用即刷新）；`apply()` 不改表——
//! 变更集引用的是**列树那一刻**的快照，这一点与 `ArchiveEditor::rewrite` 的语义一致
//! （core 在提交后重新列树）。`entries()` **失败**（取消/镜像损坏）时不动旧快照：
//! 上一次成功的列表仍然可用，不会被半列出来的结果替换。
//!
//! # 写侧口径（`Change` → 上游 `apply_*`）
//!
//! | `Change` | 上游 | 说明 |
//! |---|---|---|
//! | `Replace { entry, src }` | `apply_replace_file_content`（不存在则先 `apply_create`） | 之后 `apply_utimens` 保本地 mtime |
//! | `Delete(id)` | 目录 `apply_rmdir`、其余 `apply_unlink` | 目录非空 ⇒ 上游报错 ⇒ `vol.err.dir-not-empty`（**不递归删**） |
//! | `Append { src, name }` | 逐级 `apply_mkdir` + `apply_create` + `apply_replace_file_content` | 名字以 `/` 结尾 ⇒ 只建目录 |
//! | `Rename { from, to }` | `apply_rename(from, to, false)` | `to` 是裸名 ⇒ 以 `from` 的父目录为准；目录 ⇒ 上游整子树 |
//! | `MakeDir { name }` | 逐级 `apply_mkdir` | 父目录不存在就建 |
//!
//! # 不列出的东西
//!
//! 设备/字符设备/管道/套接字**不列出**（避免误导：它们的「内容」不是可提取的字节），
//! 计数进 [`EntryTree::skipped`]；同理，表示不成 `EntryName` 的坏名字（含 NUL/控制
//! 字符）与读不到 inode 的目录项也只计数、不列出。目录本身**要**列出。

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use fs_ext4::Filesystem;
use fs_ext4::fs::{TIME_OMIT, UTIME_OMIT};
use fs_ext4::inode::{S_IFDIR, S_IFLNK, S_IFMT, S_IFREG};
use hzip_format::{
    Change, EntryAttrs, EntryId, EntryInfo, EntryName, NameEncoding, parse_entry_name,
};
use hzip_progress::CancellationToken;
use hzip_vol::error::{ReadOnlyReason, VolError, VolResult};
use hzip_vol::{AuditSummary, EntryLimit, EntryTree, FsCaps, FsVolume};

use crate::error::map_err;

/// ext4 名字上限（`EXT4_NAME_LEN` = 255 **字节**）。
pub const NAME_MAX: usize = 255;

/// 新建文件/目录的权限位（上游 `apply_*` 的 mode 语义 = 低 12 位）。
const FILE_MODE: u16 = 0o644;
/// 新建目录的权限位。
const DIR_MODE: u16 = 0o755;

/// 自检的遍历上限（与 `EntryLimit::default()` 同量级：防恶意镜像把自检拖死）。
const AUDIT_MAX_DIRS: u32 = 100_000;
const AUDIT_MAX_ENTRIES: u32 = 100_000;

/// `EntryId`（下标）→ inode + 规范化路径。见模块文档。
#[derive(Debug, Default)]
struct EntryMap {
    inodes: Vec<u32>,
    paths: Vec<String>,
}

/// 已挂载的 ext4 卷。
pub struct Ext4Volume {
    fs: Filesystem,
    /// 列树时建的映射（`entries()` 重建；`apply()` 只读）。
    map: RwLock<EntryMap>,
    caps: FsCaps,
}

impl Ext4Volume {
    /// 只读卷（脏卷拒写、按设置只读、设备不可写三种情况共用）。
    pub(crate) fn read_only(fs: Filesystem, reason: ReadOnlyReason) -> Self {
        Self {
            fs,
            map: RwLock::new(EntryMap::default()),
            caps: FsCaps::read_only(reason),
        }
    }

    /// 可写卷（写侧能力全开；见 [`crate::driver::writable_caps`]）。
    pub(crate) fn writable(fs: Filesystem) -> Self {
        Self {
            fs,
            map: RwLock::new(EntryMap::default()),
            caps: crate::driver::writable_caps(),
        }
    }

    /// 条目表读锁（中毒只在持锁线程 panic 时发生；此时如实报错，不返回假数据）。
    fn map(&self) -> VolResult<std::sync::RwLockReadGuard<'_, EntryMap>> {
        self.map
            .read()
            .map_err(|_| VolError::Image("条目表锁中毒".into()))
    }

    /// `EntryId` → inode。
    fn inode_of(&self, id: EntryId) -> VolResult<u32> {
        self.map()?
            .inodes
            .get(id)
            .copied()
            .ok_or_else(|| VolError::Image(format!("条目 ID 越界: {id}（先调 entries()）")))
    }

    /// `EntryId` → 规范化路径。
    fn path_of(&self, id: EntryId) -> VolResult<String> {
        self.map()?
            .paths
            .get(id)
            .cloned()
            .ok_or_else(|| VolError::Image(format!("条目 ID 越界: {id}（先调 entries()）")))
    }

    /// `EntryId` → (inode, 路径)。
    fn entry_of(&self, id: EntryId) -> VolResult<(u32, String)> {
        let map = self.map()?;
        match (map.inodes.get(id).copied(), map.paths.get(id)) {
            (Some(ino), Some(path)) => Ok((ino, path.clone())),
            _ => Err(VolError::Image(format!(
                "条目 ID 越界: {id}（先调 entries()）"
            ))),
        }
    }

    /// 逐级建目录（已存在即跳过）——`Append`/`MakeDir` 的「父目录不存在就建」
    /// 是既有档案语义（zip/7z 会自动建隐式目录）。
    fn ensure_dir(&self, path: &str) -> VolResult<()> {
        let mut cur = String::new();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            if !cur.is_empty() {
                cur.push('/');
            }
            cur.push_str(seg);
            match self.fs.lookup_path_bytes(abs(&cur).as_bytes()) {
                Ok(_) => {}
                Err(fs_ext4::Error::NotFound) => {
                    self.fs.apply_mkdir(&abs(&cur), DIR_MODE).map_err(map_err)?;
                }
                Err(e) => return Err(map_err(e)),
            }
        }
        Ok(())
    }

    /// 建 `path` 的父目录段（无父段 = 卷根，直接成功）。
    fn ensure_parent(&self, path: &str) -> VolResult<()> {
        match path.rsplit_once('/') {
            Some((parent, _)) => self.ensure_dir(parent),
            None => Ok(()),
        }
    }

    /// 把本地文件的 mtime 写进卷内条目（`Replace` 的时间戳保真）。
    ///
    /// atime 用 `UTIME_OMIT` 保持不动：我们只搬「用户看得见的那一个」时间。
    fn set_local_mtime(&self, path: &str, local: &Path) -> VolResult<()> {
        let meta = std::fs::metadata(local).map_err(VolError::Io)?;
        let (sec, nsec) = mtime_parts(&meta)?;
        self.fs
            .apply_utimens(&abs(path), TIME_OMIT, UTIME_OMIT, sec, nsec)
            .map_err(map_err)
    }
}

impl FsVolume for Ext4Volume {
    fn fs_id(&self) -> &'static str {
        "ext4"
    }

    fn caps(&self) -> FsCaps {
        self.caps
    }

    fn entries(&self, limit: &EntryLimit, cancel: &CancellationToken) -> VolResult<EntryTree> {
        let root = self.fs.lookup_path_bytes(b"/").map_err(map_err)?;
        let mut tree = EntryTree::default();
        let mut map = EntryMap::default();
        // 显式栈的深度优先：`(目录 inode, 路径前缀, 深度)`。根不列在树里，
        // 它的孩子就是顶层条目（名字无前导 `./`）。
        let mut stack: Vec<(u32, String, usize)> = vec![(root, String::new(), 0)];

        'walk: while let Some((dir, prefix, depth)) = stack.pop() {
            check_cancel(cancel)?;
            if depth >= limit.max_depth {
                // 到深度顶不再深入：树没列全，如实标注。
                tree.truncated = true;
                continue;
            }
            for entry in self.fs.read_dir_ino(dir).map_err(map_err)? {
                check_cancel(cancel)?;
                if entry.name == b"." || entry.name == b".." {
                    continue;
                }
                let Some((name, name_encoding)) = join_name(&prefix, &entry.name) else {
                    tracing::debug!(ino = entry.inode, "条目名表示不成 EntryName ⇒ 不列出");
                    tree.skipped += 1;
                    continue;
                };
                let inode = match self.fs.stat_ino(entry.inode) {
                    Ok(i) => i,
                    Err(e) => {
                        tracing::debug!(ino = entry.inode, error = %e, "目录项指向的 inode 读不到 ⇒ 不列出");
                        tree.skipped += 1;
                        continue;
                    }
                };
                let kind = inode.mode & S_IFMT;
                if !matches!(kind, S_IFREG | S_IFDIR | S_IFLNK) {
                    // 设备/管道/套接字：不列出（计数如实上报）。
                    tree.skipped += 1;
                    continue;
                }
                if tree.entries.len() >= limit.max_entries {
                    tree.truncated = true;
                    break 'walk;
                }

                let is_dir = kind == S_IFDIR;
                let is_symlink = kind == S_IFLNK;
                let symlink_target = if is_symlink {
                    let raw = self.fs.read_link_ino(entry.inode).map_err(map_err)?;
                    Some(PathBuf::from(String::from_utf8_lossy(&raw).into_owned()))
                } else {
                    None
                };
                let path = name.as_str().to_string();
                tree.entries.push(EntryInfo {
                    id: tree.entries.len(),
                    name,
                    is_dir,
                    is_symlink,
                    symlink_target,
                    // 目录没有可提取的字节（仓库既有口径：tar/zip/cpio 三处同为 0）；
                    // 文件与符号链接（大小 = 目标路径长度）如实报 inode 的大小。
                    size_raw: if is_dir { 0 } else { inode.size },
                    // 卷内占用：`i_blocks` 以 512 B 扇区计。稀疏文件的这块会小于
                    // `size_raw`，那正是它在盘上实际占的字节数。
                    size_packed: inode.blocks.saturating_mul(512),
                    mtime: unix_time(inode.mtime),
                    mode: Some(u32::from(inode.mode)),
                    attributes: EntryAttrs {
                        read_only: inode.mode & 0o200 == 0,
                        executable: Some(inode.mode & 0o111 != 0),
                        ..Default::default()
                    },
                    encrypted: false,
                    header_encrypted: false,
                    crc32: None,
                    codec: None,
                    name_encoding,
                    encryption: None,
                    compression_level: None,
                });
                map.inodes.push(entry.inode);
                if is_dir {
                    stack.push((entry.inode, path.clone(), depth + 1));
                }
                map.paths.push(path);
            }
        }

        *self
            .map
            .write()
            .map_err(|_| VolError::Image("条目表锁中毒".into()))? = map;
        tracing::debug!(
            entries = tree.entries.len(),
            skipped = tree.skipped,
            truncated = tree.truncated,
            "ext4 列树完成"
        );
        Ok(tree)
    }

    fn read_at(&self, id: EntryId, offset: u64, buf: &mut [u8]) -> VolResult<usize> {
        let ino = self.inode_of(id)?;
        self.fs.read_ino(ino, offset, buf).map_err(map_err)
    }

    fn size_of(&self, id: EntryId) -> VolResult<u64> {
        let ino = self.inode_of(id)?;
        Ok(self.fs.stat_ino(ino).map_err(map_err)?.size)
    }

    fn apply(
        &mut self,
        change: &Change,
        local: Option<&Path>,
        cancel: &CancellationToken,
    ) -> VolResult<()> {
        if !self.caps.write {
            // 拒写必须带理由（枚举 ⇒ 文案单源）。
            return Err(VolError::ReadOnly(
                self.caps
                    .read_only_reason
                    .unwrap_or(ReadOnlyReason::BySetting),
            ));
        }
        check_cancel(cancel)?;
        match change {
            Change::Replace { entry, src } => {
                let path = self.path_of(*entry)?;
                let data = read_local(local, src)?;
                if self.fs.lookup_path_bytes(abs(&path).as_bytes()).is_err() {
                    // 变更集与卷内现状不一致（core 的编辑批通常先列树，故这是异常路径）：
                    // 建出来再写，总比整个编辑保存失败好。
                    tracing::warn!(path, "替换的目标不在卷里 ⇒ 先建后写");
                    self.fs
                        .apply_create(&abs(&path), FILE_MODE)
                        .map_err(map_err)?;
                }
                self.fs
                    .apply_replace_file_content(&abs(&path), &data)
                    .map_err(map_err)?;
                // 时间戳保真：本地文件 mtime（编辑保存后时间变了会被当 bug 报）。
                self.set_local_mtime(&path, local.unwrap_or(src.as_path()))?;
                tracing::debug!(path, bytes = data.len(), "ext4 替换条目");
            }
            Change::Delete(id) => {
                let (ino, path) = self.entry_of(*id)?;
                let inode = self.fs.stat_ino(ino).map_err(map_err)?;
                if inode.mode & S_IFMT == S_IFDIR {
                    // 目录非空 ⇒ 上游报 DirectoryNotEmpty ⇒ `vol.err.dir-not-empty`。
                    // 不递归删：镜像里的删除应当是显式的。
                    self.fs.apply_rmdir(&abs(&path)).map_err(map_err)?;
                } else {
                    self.fs.apply_unlink(&abs(&path)).map_err(map_err)?;
                }
                tracing::debug!(path, "ext4 删除条目");
            }
            Change::Append { src, name } => {
                let path = name.as_str().to_string();
                if path.ends_with('/') {
                    // 目录条目（尾 `/`）：只建目录。
                    self.ensure_dir(path.trim_end_matches('/'))?;
                } else {
                    let data = read_local(local, src)?;
                    self.ensure_parent(&path)?;
                    match self.fs.lookup_path_bytes(abs(&path).as_bytes()) {
                        Ok(_) => tracing::warn!(path, "追加的目标已存在 ⇒ 覆盖其内容"),
                        Err(fs_ext4::Error::NotFound) => {
                            self.fs
                                .apply_create(&abs(&path), FILE_MODE)
                                .map_err(map_err)?;
                        }
                        Err(e) => return Err(map_err(e)),
                    }
                    self.fs
                        .apply_replace_file_content(&abs(&path), &data)
                        .map_err(map_err)?;
                    tracing::debug!(path, bytes = data.len(), "ext4 追加条目");
                }
            }
            Change::Rename { from, to } => {
                let from = from.as_str();
                let to = rename_target(from, to.as_str());
                // `replace_if_exists = false`：覆盖既有条目由 core 的重名判定拦在
                // 前面，驱动侧不替用户做这个决定。
                self.fs
                    .apply_rename(&abs(from), &abs(&to), false)
                    .map_err(map_err)?;
                tracing::debug!(from, to, "ext4 改名条目");
            }
            Change::MakeDir { name } => {
                let path = name.as_str().trim_end_matches('/').to_string();
                self.ensure_dir(&path)?;
                tracing::debug!(path, "ext4 建目录");
            }
            // `Change` 是 `#[non_exhaustive]`（T20 冻结契约可扩展）：明确拒绝，
            // 而不是静默忽略一条我们读不懂的变更。
            _ => return Err(VolError::Unsupported("vol.unsupported.change")),
        }
        Ok(())
    }

    fn flush(&mut self) -> VolResult<()> {
        self.fs.flush().map_err(map_err)
    }

    fn audit(&self) -> VolResult<AuditSummary> {
        let report = self
            .fs
            .audit(AUDIT_MAX_DIRS, AUDIT_MAX_ENTRIES)
            .map_err(map_err)?;
        if report.is_clean() {
            return Ok(AuditSummary::clean());
        }
        let mut findings: Vec<String> = report.anomalies.iter().map(|a| format!("{a:?}")).collect();
        if findings.is_empty() {
            // 上限内的异常计数大于逐条记录数时（上游可能只计数）。
            findings.push(format!("发现 {} 处异常", report.anomalies_count));
        }
        tracing::debug!(count = report.anomalies_count, "ext4 自检发现异常");
        Ok(AuditSummary {
            findings,
            skipped: false,
        })
    }
}

/// 条目路径（无前导 `/`）→ 上游要的**绝对**路径。
///
/// 上游的路径接口全部经 `split_parent_and_base`：不含 `/` 的路径直接
/// `InvalidArgument("relative path")`。故每一次上游调用都要过这里一次——
/// 树里、UI 里、`EntryName` 里都是无前导 `/` 的相对形态（02 §7.2）。
fn abs(path: &str) -> String {
    format!("/{path}")
}

/// 目录项原始名 + 路径前缀 → 规范化条目名（`/` 分隔、无前导 `./`）。
///
/// `None` = 这个名字表示不成 [`EntryName`]（含 NUL/C0/C1 控制字符）⇒ 调用方按
/// 「不列出」处理。非 UTF-8 名用有损解码（`U+FFFD`），并如实标
/// [`NameEncoding::Unknown`]。
fn join_name(prefix: &str, raw: &[u8]) -> Option<(EntryName, NameEncoding)> {
    let lossy = String::from_utf8_lossy(raw);
    let full = if prefix.is_empty() {
        lossy.into_owned()
    } else {
        format!("{prefix}/{lossy}")
    };
    let encoding = if std::str::from_utf8(raw).is_ok() {
        NameEncoding::Utf8
    } else {
        NameEncoding::Unknown
    };
    parse_entry_name(&full).ok().map(|n| (n, encoding))
}

/// 改名目标：`to` 是裸名（不含 `/`）⇒ 以 `from` 的父目录为准
/// （`Change::Rename.to` 的契约；GUI 传的是完整路径，此时原样使用）。
fn rename_target(from: &str, to: &str) -> String {
    if to.contains('/') {
        return to.to_string();
    }
    match from.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{to}"),
        None => to.to_string(),
    }
}

/// unix 秒 → `OffsetDateTime`（超出 `time` 的可表示范围 ⇒ `None`，不猜）。
fn unix_time(secs: i64) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::from_unix_timestamp(secs).ok()
}

/// 本地文件的 mtime → `(秒, 纳秒)`；1970 之前的时间取负秒（ext4 存得下）。
fn mtime_parts(meta: &std::fs::Metadata) -> VolResult<(i64, u32)> {
    let t = meta.modified().map_err(VolError::Io)?;
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let sec = i64::try_from(d.as_secs())
                .map_err(|_| VolError::Image("本地文件 mtime 超出 i64 秒".into()))?;
            Ok((sec, d.subsec_nanos()))
        }
        Err(e) => {
            let d = e.duration();
            let sec = i64::try_from(d.as_secs())
                .map_err(|_| VolError::Image("本地文件 mtime 超出 i64 秒".into()))?;
            Ok((-sec, 0))
        }
    }
}

/// 待写入的本地文件路径：优先用 `apply(local, …)` 给的，其次用变更自带的 `src`。
fn read_local(local: Option<&Path>, src: &Path) -> VolResult<Vec<u8>> {
    let path = local.unwrap_or(src);
    std::fs::read(path).map_err(VolError::Io)
}

/// 取消即中断（`entries()`/`apply()` 的每条路径前都问一次）。
fn check_cancel(cancel: &CancellationToken) -> VolResult<()> {
    if cancel.is_cancelled() {
        return Err(VolError::Cancelled);
    }
    Ok(())
}
