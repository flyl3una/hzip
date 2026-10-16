//! 已挂载的 XFS 卷：递归列表、惰性读、五类变更、自检。
//!
//! # 大端与小端：一处例外
//!
//! 本驱动不自己解析任何磁盘结构（那是上游的活），但**凡自己读头部字段**都按大端
//! （见 [`crate::driver::is_xfs_superblock`]）；唯一的小端字段是**校验和**
//! （内核 `xfs_end_cksum()` = `~cpu_to_le32(crc)`）。上游 README 记着「按大端读校验和
//! 会让每一个真实文件系统都像坏的」这条曾经真实发生的错——照它的口径走，别自己重算。
//!
//! # `EntryId` ⇒ inode / 名字
//!
//! `entries()` 建三张平行表（`EntryId` 即下标）：**inode**（`read_at`/`size_of` 用）、
//! **展示名**（`EntryName`，规范化后的文本）与**原始字节路径**（`apply_*` 定位用）。
//! 于是 `read_at`/`size_of` 是「惰性、乱序、可重复」的（契约红线 2）：它们只查表 +
//! 一次 `open_ino`，不依赖任何游标。
//!
//! ## 为什么展示名之外还要留一份**字节**路径
//!
//! XFS 的目录项名是**裸字节**（格式里没有编码字段 ⇒ 上游一律按字节比较，绝不猜
//! 编码），而 [`hzip_format::EntryName`] 是 `String`：**非 UTF-8 的名字只能有损表示**
//! （`U+FFFD`）。若写侧也拿展示名回查，一个名字里带 `0xFF` 的文件会被「找到另一个
//! 名字」（有损后同形的那个）或干脆找不到——于是 `Replace` 会**另建一个名字被改坏的
//! 副本**。故：**展示名给人看，字节路径给写侧用**，两者同源同长。
//!
//! `Change::Rename` 的 `from` 只能是 `EntryName`（core 手里只有规范化名字）⇒ 改名是
//! 唯一一处按展示名回查字节路径的地方（`path_bytes_of_name`）；快照里查不到就**按名
//! 当路径**用（批内前一条变更刚改出来的名字不在快照里），在不在由上游判——不在就是
//! `vol.err.entry-gone`，不猜。
//!
//! 表在每次 `entries()` 里**整体重建**（成功才替换旧表：取消/坏盘不会把上一次可用
//! 的列表换成半列出来的结果）；`apply()` **不改表**——变更集引用的是**列树那一刻**
//! 的快照，与 `ArchiveEditor::rewrite` 的语义一致（core 在提交后重新列树）。
//!
//! # 挂载句柄的持有方式（与 FAT 侧相反、与 ext4 侧相同）
//!
//! `Filesystem` **长期持有**（上游的读路径很吃它的读缓存，而挂载要把日志扫一遍：
//! 每次操作现挂等于每次操作都扫一圈日志）。`flush()` 是唯一把它**拿出去**的地方：
//! 走上游 `Filesystem::unmount()`——它把记在日志里的元数据推到各自的主块、把
//! superblock 的汇总计数写对、再写一条 unmount 记录，于是卷**干净**（否则每一次我们
//! 写过的卷都带脏日志：只有会重放的挂载才看得到那些写，`xfs_repair` 也会拒收，而
//! 本驱动自己的**可写**挂载同样会拒收）。拿出去之后按需**惰性重挂**。
//!
//! # `Change` → 上游写接口
//!
//! | `Change` | 上游 | 备注 |
//! |---|---|---|
//! | `Replace { entry, src }` | `truncate_to_zero`（文件非空时）+ 逐块 `write` | id 不在卷里 ⇒ 先 `create_file`；之后 `change_attributes` 保本地 mtime |
//! | `Delete(id)` | 目录 ⇒ `remove_directory`、其余 ⇒ `unlink_file` | 目录**不递归删**；非空 ⇒ `vol.err.dir-not-empty` |
//! | `Append { src, name }` | 逐级 `create_directory` + `create_file` + 写内容 | 同名已存在 ⇒ 覆盖（zip `-a` 的更新语义）；名字以 `/` 结尾 ⇒ 只建目录 |
//! | `Rename { from, to }` | `rename_in_directory`（**同目录**） | 跨目录 ⇒ `vol.err.dir-move`；目录已超出短格式 ⇒ `vol.unsupported.dir-too-large` |
//! | `MakeDir { name }` | 逐级 `create_directory` | 已存在 ⇒ 幂等成功 |
//!
//! # 写侧**做不到**的形态（如实拒绝，不半写）
//!
//! 上游把「这个形态我不写」都归到 `UnsupportedFeature`，本驱动折成
//! `vol.err.fs-feature`（细节进日志）：realtime 文件、reflink 共享 extent 的 inode、
//! 数据叉是 B+tree 的 inode、v4 卷；以及**数据叉 extent 预算**——一次 `write` 至少
//! 新增一条 extent 记录，而成于 512 字节 inode 的数据叉只放得下 21 条，故逐块写法
//! 下「约 168 MiB 以上的单文件」会被上游**在写任何字节之前**拒绝（见 [`COPY_CHUNK`]
//! 的文档）。这些都在 crate 文档的「未实现清单」里列着。
//!
//! # 条目命名
//!
//! 表示不成 [`hzip_format::EntryName`] 的名字（含 NUL/C0/C1 控制字符、空名）**不列出**、
//! 计数进 [`EntryTree::skipped`]；设备/管道/套接字同理不列出（它们的「内容」不是可
//! 提取的字节）。目录本身**要**列出。

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use fs_xfs::inode::{Format, Timestamp};
use fs_xfs::write::AttrChange;
use fs_xfs::{Error as XfsError, Filesystem};
use hzip_format::{Change, EntryAttrs, EntryId, EntryInfo, NameEncoding, parse_entry_name};
use hzip_progress::CancellationToken;
use hzip_vol::{
    AuditSummary, BlockDevice, BlockRead as _, EntryLimit, EntryTree, FsCaps, FsOptions, FsVolume,
    ReadOnlyReason, VolError, VolResult,
};

use crate::error::{
    ERR_BAD_TYPE, ERR_DIR_MOVE, ERR_DIR_TOO_LARGE, ERR_NAME_TOO_LONG, ERR_SOURCE_NOT_FILE,
    ERR_UNKNOWN_CHANGE, ERR_UNKNOWN_ENTRY, map_err,
};

/// XFS 名字上限（目录项里是 1 字节的名字长度 ⇒ 255 **字节**；非 ASCII 名更短）。
pub const NAME_MAX: usize = 255;

/// 新建文件的权限位（与 ext4 侧同口径）。
const FILE_MODE: u16 = 0o644;
/// 新建目录的权限位。
const DIR_MODE: u16 = 0o755;

/// 逐块写的块大小（8 MiB）。
///
/// # 为什么是这个量级
///
/// 上游 `write` 每次调用**至少新增一条 extent 记录**（顺序写同一文件时它不把新区间
/// 并进上一条记录），而成于 512 字节 inode 的数据叉只放得下
/// `(512 - 176) / 16 = 21` 条 ⇒ 逐块写法下「块数 × 21」就是单文件的实际上限
/// （约 168 MiB，且这是**上界**：分配越碎，一次 `take` 拿到的连续段越短、条数越多）。
/// 块越小越扛不住大文件，块越大一次要吃进的内存越多；8 MiB 是这两头之间的取中。
/// 超过预算时上游**在写任何字节之前**拒绝，本驱动折成
/// `vol.err.fs-feature` ⇒ 不会留下半写。
const COPY_CHUNK: usize = 8 * 1024 * 1024;

/// `EntryId`（下标）→ inode + 展示名 + 原始字节路径 + 是否目录。见模块文档。
#[derive(Debug, Default)]
struct EntryMap {
    inos: Vec<u64>,
    names: Vec<String>,
    paths: Vec<Vec<u8>>,
    dirs: Vec<bool>,
}

/// 已挂载的 XFS 卷（[`hzip_vol::FsVolume`] 实现）。
pub struct XfsVolume {
    /// 设备（`flush()` 之后重挂要它；挂载策略已由驱动定死）。
    dev: Arc<dyn BlockDevice>,
    /// 挂载句柄；`None` = 已被 `flush()` 干净卸载，下次访问现挂（见模块文档）。
    fs: Mutex<Option<Filesystem>>,
    /// 挂载选项（重挂要用同样的缓存口径）。
    opts: FsOptions,
    /// 文件系统块大小（超级块里读一次；`size_packed` 的换算要用）。
    blocksize: u32,
    /// 能力（挂载时定死）。
    caps: FsCaps,
    /// 条目表（`entries()` 重建；`apply()` 只读）。
    map: RwLock<EntryMap>,
}

impl XfsVolume {
    /// 用驱动已经挂好的句柄造一个卷（能力由驱动按挂载策略给）。
    pub(crate) fn new(
        dev: Arc<dyn BlockDevice>,
        fs: Filesystem,
        opts: FsOptions,
        caps: FsCaps,
    ) -> Self {
        if fs.was_replayed() {
            // 读侧的内存重放值得让用户知道：看到的是「内核恢复之后」的卷，不是设备
            // 里的那一份（设备一个字节没动）。
            tracing::debug!("XFS 卷的日志已在内存里重放（设备未改）");
        }
        Self {
            dev,
            // 块大小在超级块里读一次即可（挂载期定死；重挂也是同一个卷）。
            blocksize: fs.superblock().blocksize,
            fs: Mutex::new(Some(fs)),
            opts,
            caps,
            map: RwLock::new(EntryMap::default()),
        }
    }

    /// 取挂载句柄锁（中毒只在持锁线程 panic 时发生：报错，不返回假数据）。
    fn lock_fs(&self) -> VolResult<MutexGuard<'_, Option<Filesystem>>> {
        self.fs
            .lock()
            .map_err(|_| VolError::Image("XFS 卷状态已中毒（前次操作发生 panic）".into()))
    }

    /// 取条目表读锁。
    fn read_map(&self) -> VolResult<std::sync::RwLockReadGuard<'_, EntryMap>> {
        self.map
            .read()
            .map_err(|_| VolError::Image("XFS 条目表锁中毒".into()))
    }

    /// 现挂一个句柄（`flush()` 干净卸载之后走这里；口径与驱动一致）。
    fn mount_fs(&self) -> VolResult<Filesystem> {
        if self.caps.write {
            Filesystem::mount_rw(self.dev.clone()).map_err(map_err)
        } else {
            let rd = self.dev.clone() as Arc<dyn hzip_vol::BlockRead>;
            if self.opts.cache_blocks == 0 {
                Filesystem::mount(rd).map_err(map_err)
            } else {
                Filesystem::mount_with_cache(rd, self.opts.cache_blocks).map_err(map_err)
            }
        }
    }

    /// 拿着挂载句柄跑一段（没有就现挂）。
    ///
    /// # 锁序
    ///
    /// 本方法持 `fs` 锁。调用方若还要条目表的锁，**只能先取表锁、再进这里**（`collect()`
    /// 就是这么做的；`apply_*` 则是先读完 id 映射就放掉表锁、再进这里）——反过来
    /// 「持 `fs` 锁去等表锁」会和 `collect()` 成环。
    fn with_fs<T>(&self, f: impl FnOnce(&Filesystem) -> VolResult<T>) -> VolResult<T> {
        let mut guard = self.lock_fs()?;
        if guard.is_none() {
            *guard = Some(self.mount_fs()?);
        }
        match guard.as_ref() {
            Some(fs) => f(fs),
            // 上面刚把 `None` 填成 `Some`，这条不可达；给错误而不是 `expect`（库禁 unwrap）。
            None => Err(VolError::Image("XFS 卷句柄缺失".into())),
        }
    }

    /// `EntryId` →（inode, 原始字节路径, 展示名, 是否目录）。
    fn slot_of(&self, id: EntryId) -> VolResult<(u64, Vec<u8>, String, bool)> {
        let map = self.read_map()?;
        match (
            map.inos.get(id).copied(),
            map.paths.get(id).cloned(),
            map.names.get(id).cloned(),
            map.dirs.get(id).copied(),
        ) {
            (Some(ino), Some(path), Some(name), Some(is_dir)) => Ok((ino, path, name, is_dir)),
            _ => Err(VolError::Unsupported(ERR_UNKNOWN_ENTRY)),
        }
    }

    /// 按**展示名**回查原始字节路径（只有 `Change::Rename` 需要：它按名引用；见模块文档）。
    fn path_bytes_of_name(&self, name: &str) -> Option<Vec<u8>> {
        let map = self.read_map().ok()?;
        map.names
            .iter()
            .position(|n| n == name)
            .and_then(|i| map.paths.get(i).cloned())
    }

    /// 写一个本地文件的内容进 `path`（不存在则建、存在则截断重写），并保本地 mtime。
    ///
    /// 写前先查源是不是常规文件与末段名长度——**在动卷之前**。
    fn put_file(&self, path: &[u8], source: &Path, cancel: &CancellationToken) -> VolResult<()> {
        let meta = std::fs::metadata(source)?;
        if !meta.is_file() {
            return Err(VolError::Unsupported(ERR_SOURCE_NOT_FILE));
        }
        let (parent, base) = split_parent_bytes(path)?;
        if base.len() > NAME_MAX {
            return Err(VolError::Unsupported(ERR_NAME_TOO_LONG));
        }
        let source = source.to_path_buf();
        self.with_fs(|fs| {
            ensure_dir_bytes(fs, parent)?;
            let dir = dir_ino_bytes(fs, parent)?;
            let ino = match child_of(fs, dir, base) {
                Ok((ino, is_dir, is_regular, size)) => {
                    if is_dir || !is_regular {
                        // 目录/符号链接/设备：不拿它当常规文件写。
                        return Err(VolError::Unsupported(ERR_BAD_TYPE));
                    }
                    if size > 0 {
                        // 先清空再写：上游的 `write` 只覆盖既有字节、不缩短文件
                        // （`write_at` 更是明确拒绝写到 size 之外），故「短的替换长的」
                        // 必须走 `truncate_to_zero`。
                        fs.truncate_to_zero(ino).map_err(map_err)?;
                    }
                    ino
                }
                Err(XfsError::NotFound) => fs.create_file(dir, base, FILE_MODE).map_err(map_err)?.0,
                Err(e) => return Err(map_err(e)),
            };

            // 逐块写（8 MiB；见 [`COPY_CHUNK`]）。取消在块边界上生效。
            let mut input = std::fs::File::open(&source)?;
            let mut buf = vec![0u8; COPY_CHUNK];
            let mut offset = 0u64;
            loop {
                check_cancel(cancel)?;
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                fs.write(ino, offset, &buf[..n]).map_err(map_err)?;
                offset += n as u64;
            }
            // 时间戳保真：本地文件的 mtime（编辑保存后时间变了会被当 bug 报）。
            let (sec, nsec) = mtime_parts(&meta)?;
            fs.change_attributes(
                ino,
                &AttrChange {
                    mtime: Some(Timestamp { sec, nsec }),
                    ..AttrChange::default()
                },
            )
            .map_err(map_err)?;
            tracing::debug!(
                path = %String::from_utf8_lossy(path),
                bytes = offset,
                "XFS 写入条目内容"
            );
            Ok(())
        })
    }

    /// 列全树（见模块文档；成功才替换条目表）。
    fn collect(&self, limit: &EntryLimit, cancel: &CancellationToken) -> VolResult<EntryTree> {
        let mut walker = Walker {
            limit,
            cancel,
            blocksize: u64::from(self.blocksize),
            tree: EntryTree::default(),
            map: EntryMap::default(),
        };
        self.with_fs(|fs| {
            let root = fs.superblock().rootino;
            walker.run(fs, root)
        })?;
        *self
            .map
            .write()
            .map_err(|_| VolError::Image("XFS 条目表锁中毒".into()))? = walker.map;
        Ok(walker.tree)
    }
}

impl std::fmt::Debug for XfsVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XfsVolume")
            .field("caps", &self.caps)
            .field("size", &self.dev.size_bytes())
            .finish()
    }
}

impl FsVolume for XfsVolume {
    fn fs_id(&self) -> &'static str {
        "xfs"
    }

    fn caps(&self) -> FsCaps {
        self.caps
    }

    fn entries(&self, limit: &EntryLimit, cancel: &CancellationToken) -> VolResult<EntryTree> {
        let tree = self.collect(limit, cancel)?;
        tracing::debug!(
            entries = tree.entries.len(),
            truncated = tree.truncated,
            skipped = tree.skipped,
            "XFS 列树完成"
        );
        Ok(tree)
    }

    fn read_at(&self, id: EntryId, offset: u64, buf: &mut [u8]) -> VolResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let (ino, _, _, is_dir) = self.slot_of(id)?;
        if is_dir {
            return Ok(0); // 目录没有可提取的字节（投影层只对文件调它）
        }
        self.with_fs(|fs| {
            let file = fs.open_ino(ino).map_err(map_err)?;
            file.read_at(offset, buf).map_err(map_err)
        })
    }

    fn size_of(&self, id: EntryId) -> VolResult<u64> {
        let (ino, _, _, _) = self.slot_of(id)?;
        self.with_fs(|fs| Ok(fs.open_ino(ino).map_err(map_err)?.len()))
    }

    fn apply(
        &mut self,
        change: &Change,
        local: Option<&Path>,
        cancel: &CancellationToken,
    ) -> VolResult<()> {
        if !self.caps.write {
            return Err(read_only(&self.caps));
        }
        check_cancel(cancel)?;
        match change {
            Change::Replace { entry, src } => {
                let (_, path, name, is_dir) = self.slot_of(*entry)?;
                if is_dir {
                    return Err(VolError::Unsupported(ERR_BAD_TYPE));
                }
                let source = local.unwrap_or(src.as_path()).to_path_buf();
                self.put_file(&path, &source, cancel)?;
                tracing::debug!(name, "XFS 替换条目");
            }
            Change::Delete(id) => {
                let (_, path, name, is_dir) = self.slot_of(*id)?;
                let (parent, base) = split_parent_bytes(&path)?;
                self.with_fs(|fs| {
                    let dir = dir_ino_bytes(fs, parent)?;
                    let done = if is_dir {
                        // 目录非空 ⇒ 上游报 DirectoryNotEmpty ⇒ `vol.err.dir-not-empty`。
                        // **不递归删**：镜像里的删除应当是显式的。
                        fs.remove_directory(dir, base)
                    } else {
                        fs.unlink_file(dir, base)
                    };
                    done.map_err(map_err)?;
                    Ok(())
                })?;
                tracing::debug!(name, "XFS 删除条目");
            }
            Change::Append { src, name } => {
                let full = name.as_str();
                if let Some(dir) = full.strip_suffix('/') {
                    // 名字以 `/` 结尾 ⇒ 只建目录（zip/7z 的「隐式目录」语义）。
                    self.with_fs(|fs| ensure_dir_bytes(fs, dir.as_bytes()))?;
                } else {
                    let source = local.unwrap_or(src.as_path()).to_path_buf();
                    self.put_file(full.as_bytes(), &source, cancel)?;
                }
                tracing::debug!(name = full, "XFS 追加条目");
            }
            Change::Rename { from, to } => {
                let from_name = from.as_str();
                // 快照里有 ⇒ 用它记下的**原始字节**（非 UTF-8 名才定位得准）；
                // 快照里没有 ⇒ 它就是**批内前一条变更刚改出来的名字**（快照是列树那一刻
                // 的，`apply` 不改表）⇒ 按名当路径用，让上游去判「在不在」。
                let from_path = self
                    .path_bytes_of_name(from_name)
                    .unwrap_or_else(|| from_name.as_bytes().to_vec());
                let (from_parent, from_base) = split_parent_bytes(&from_path)?;
                // `to` 不含 `/` ⇒ 以 **`from` 的父目录**为准（`Change::Rename.to` 的契约）；
                // 含 `/` ⇒ 视为卷内完整路径。父目录那一段按**字节**拼：XFS 的名字是裸
                // 字节，拼一次文本就会把非 UTF-8 的名字改坏。
                let to_bytes = if to.as_str().contains('/') {
                    trim_slashes(to.as_str().as_bytes()).to_vec()
                } else {
                    join_bytes(from_parent, to.as_str().as_bytes())
                };
                let (to_parent, to_base) = split_parent_bytes(&to_bytes)?;
                if from_parent != to_parent {
                    // 上游只提供「一个目录内改名」（跨目录要记两个父目录并调整链接数）。
                    return Err(VolError::Unsupported(ERR_DIR_MOVE));
                }
                self.with_fs(|fs| {
                    let dir = dir_ino_bytes(fs, from_parent)?;
                    // 上游的改名只覆盖**短格式**目录（大目录要记一个 buffer item）。
                    // 自己先判形态，好过按上游的错误文本猜键。
                    let parent = fs.open_ino(dir).map_err(map_err)?;
                    if parent.inode().format != Format::Local {
                        return Err(VolError::Unsupported(ERR_DIR_TOO_LARGE));
                    }
                    drop(parent);
                    fs.rename_in_directory(dir, from_base, to_base)
                        .map_err(map_err)?;
                    Ok(())
                })?;
                tracing::debug!(from = from_name, to = %String::from_utf8_lossy(&to_bytes), "XFS 改名条目");
            }
            Change::MakeDir { name } => {
                let dir = name.as_str().trim_end_matches('/');
                self.with_fs(|fs| ensure_dir_bytes(fs, dir.as_bytes()))?;
                tracing::debug!(dir, "XFS 建目录");
            }
            // `Change` 是 `#[non_exhaustive]`（可扩展契约）：明确拒绝，而不是静默忽略
            // 一条我们读不懂的变更。
            _ => return Err(VolError::Unsupported(ERR_UNKNOWN_CHANGE)),
        }
        Ok(())
    }

    fn flush(&mut self) -> VolResult<()> {
        if !self.caps.write {
            return Ok(()); // 只读挂载：没有一个字节是我们写的，也就没什么要落
        }
        // 走上游的 `unmount`：推日志记录 + 写对 superblock 计数 + 收尾 unmount 记录
        // ⇒ 卷干净（否则每次我们写过的卷都带脏日志，见模块文档）。它是 `self` 消费型
        // 接口，故把句柄**拿出去**：下一次访问再惰性重挂。
        //
        // 卸载失败 ⇒ 句柄不还：卷立刻退化成只读（重挂会看到一份未重放的日志，驱动按
        // `JournalPending` 拒写），这是**如实**的结果——比留下一个「以为已经落盘」的可写
        // 句柄安全。
        let taken = { self.lock_fs()?.take() };
        if let Some(fs) = taken {
            fs.unmount().map_err(map_err)?;
        }
        self.dev.flush()?;
        Ok(())
    }

    fn audit(&self) -> VolResult<AuditSummary> {
        // 上游的 `check` **从不写**（连只读挂载也能跑），故这就是真自检——`skipped`
        // 保持 false（`is_clean()` 才不会被误判成「没检查」）。
        let report = self.with_fs(|fs| Ok(fs_xfs::check::check(fs)))?;
        if report.is_clean() {
            return Ok(AuditSummary::clean());
        }
        let mut findings: Vec<String> = report
            .findings
            .iter()
            .map(|f| {
                format!(
                    "{} [ag={:?} agbno={:?} ino={:?} field={:?}]：{}",
                    f.code.as_str(),
                    f.location.ag,
                    f.location.agbno,
                    f.location.ino,
                    f.location.field,
                    f.what
                )
            })
            .collect();
        if report.suppressed > 0 {
            findings.push(format!("另有 {} 处同类问题未逐条列出", report.suppressed));
        }
        if report.scan == fs_xfs::check::Scan::Partial {
            findings.push("扫描不完整：有结构读不到，其下未检查".to_string());
        }
        tracing::debug!(
            findings = report.findings.len(),
            inodes = report.inodes,
            directories = report.directories,
            dirty = report.dirty,
            "XFS 自检发现异常"
        );
        Ok(AuditSummary {
            findings,
            skipped: false,
        })
    }
}

// ---------------------------------------------------------------- 列树

/// 列表累积器：`tree.entries` 与 `map` **同步压入** ⇒ `EntryId` = 下标。
struct Walker<'a> {
    limit: &'a EntryLimit,
    cancel: &'a CancellationToken,
    /// 文件系统块大小（算「盘上占用」）。
    blocksize: u64,
    tree: EntryTree,
    map: EntryMap,
}

impl Walker<'_> {
    /// 显式栈的深度优先（`(目录 inode, 字节路径前缀, 深度)`）。
    ///
    /// 根**不列在树里**：它的孩子就是顶层条目（名字无前导 `./`）。
    fn run(&mut self, fs: &Filesystem, root: u64) -> VolResult<()> {
        let mut stack: Vec<(u64, Vec<u8>, usize)> = vec![(root, Vec::new(), 0)];
        while let Some((dir_ino, prefix, depth)) = stack.pop() {
            check_cancel(self.cancel)?;
            let entries = fs
                .open_ino(dir_ino)
                .and_then(|d| d.entries())
                .map_err(map_err)?;
            let children: Vec<_> = entries
                .iter()
                .filter(|e| e.name != b"." && e.name != b"..")
                .collect();
            if depth >= self.limit.max_depth {
                // 到深度顶且确实还有孩子 ⇒ 树没列全，如实标注（空目录不算截断）。
                if !children.is_empty() {
                    self.tree.truncated = true;
                }
                continue;
            }
            for entry in children {
                check_cancel(self.cancel)?;
                if self.tree.entries.len() >= self.limit.max_entries {
                    self.tree.truncated = true;
                    return Ok(()); // 上限到顶：立刻收手（不再列、不再深入）
                }
                let path = join_bytes(&prefix, &entry.name);
                let Some((name, name_encoding)) = entry_name(&path) else {
                    tracing::debug!(ino = entry.ino, "条目名表示不成 EntryName ⇒ 不列出");
                    self.tree.skipped += 1;
                    continue;
                };
                let view = match inode_view(fs, entry.ino) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!(ino = entry.ino, error = %e, "目录项指向的 inode 读不到 ⇒ 不列出");
                        self.tree.skipped += 1;
                        continue;
                    }
                };
                if !(view.is_dir || view.is_symlink || view.is_regular) {
                    // 设备/管道/套接字：不列出（「内容」不是可提取的字节），计数如实上报。
                    self.tree.skipped += 1;
                    continue;
                }
                let name_str = name.as_str().to_string();
                self.tree.entries.push(EntryInfo {
                    id: self.map.inos.len(),
                    name,
                    is_dir: view.is_dir,
                    is_symlink: view.is_symlink,
                    symlink_target: view.target.map(PathBuf::from),
                    // 目录没有可提取的字节（仓库既有口径：tar/zip/cpio 三处同为 0）；
                    // 文件与符号链接（大小 = 目标路径长度）如实报 inode 的大小。
                    size_raw: if view.is_dir { 0 } else { view.size },
                    // 盘上占用 = `di_nblocks` × 块大小。**单位是文件系统块**：本工具链
                    // 写出来的卷就是这样（上游 `file_write` 用 `ceil(bytes/blocksize)`
                    // 写这个字段；夹具里一个 12288 字节的文件读出 3，不是 24）。
                    // 稀疏文件的这块会小于 `size_raw`，那正是它在盘上实际占的字节数。
                    size_packed: view.nblocks.saturating_mul(self.blocksize),
                    mtime: unix_time(view.mtime_sec),
                    mode: Some(u32::from(view.mode)),
                    attributes: EntryAttrs {
                        read_only: view.mode & 0o200 == 0,
                        executable: Some(view.mode & 0o111 != 0),
                        ..Default::default()
                    },
                    encrypted: false,
                    header_encrypted: false,
                    crc32: None,
                    codec: None, // 卷内文件不经压缩层
                    name_encoding,
                    encryption: None,
                    compression_level: None,
                });
                self.map.inos.push(entry.ino);
                self.map.names.push(name_str);
                self.map.dirs.push(view.is_dir);
                if view.is_dir {
                    stack.push((entry.ino, path.clone(), depth + 1));
                }
                self.map.paths.push(path);
            }
        }
        Ok(())
    }
}

/// inode 里我们真正用到的几项（出了 [`Filesystem`] 借用就只留这些）。
struct InodeView {
    is_dir: bool,
    is_symlink: bool,
    is_regular: bool,
    size: u64,
    nblocks: u64,
    mode: u16,
    mtime_sec: i64,
    target: Option<String>,
}

/// 读一个 inode（符号链接顺带读目标——上游 `read_link` 给的是**原样存储**的目标，
/// 可能是相对路径，故原样转过去，不做任何解析）。
fn inode_view(fs: &Filesystem, ino: u64) -> Result<InodeView, XfsError> {
    let file = fs.open_ino(ino)?;
    let inode = file.inode();
    Ok(InodeView {
        is_dir: inode.is_dir(),
        is_symlink: inode.is_symlink(),
        is_regular: inode.is_regular_file(),
        size: inode.size,
        nblocks: inode.nblocks,
        mode: inode.mode,
        mtime_sec: inode.mtime.sec,
        target: if inode.is_symlink() {
            Some(String::from_utf8_lossy(&file.link_target()?).into_owned())
        } else {
            None
        },
    })
}

// ---------------------------------------------------------------- 名字与路径

/// 字节路径 → 规范化条目名（`/` 分隔、无前导 `./`）。
///
/// `None` = 这个名字表示不成 [`hzip_format::EntryName`]（含 NUL/C0/C1 控制字符、空名）
/// ⇒ 调用方按「不列出」处理。非 UTF-8 名用有损解码（`U+FFFD`）并如实标
/// [`NameEncoding::Unknown`]——上游把名字存成裸字节且格式里没有编码字段，谁来解都是
/// 一次猜测，如实标比假装知道好（写侧仍按原始字节定位，见模块文档）。
fn entry_name(path: &[u8]) -> Option<(hzip_format::EntryName, NameEncoding)> {
    let lossy = String::from_utf8_lossy(path);
    let encoding = if std::str::from_utf8(path).is_ok() {
        NameEncoding::Utf8
    } else {
        NameEncoding::Unknown
    };
    parse_entry_name(&lossy).ok().map(|n| (n, encoding))
}

/// 拼接字节路径（父为空 ⇒ 就是名字；不产生前导 `/`）。
fn join_bytes(prefix: &[u8], name: &[u8]) -> Vec<u8> {
    if prefix.is_empty() {
        return name.to_vec();
    }
    let mut out = Vec::with_capacity(prefix.len() + 1 + name.len());
    out.extend_from_slice(prefix);
    out.push(b'/');
    out.extend_from_slice(name);
    out
}

/// 去掉两侧的 `/`。
fn trim_slashes(path: &[u8]) -> &[u8] {
    let start = path.iter().position(|b| *b != b'/').unwrap_or(path.len());
    let end = path
        .iter()
        .rposition(|b| *b != b'/')
        .map_or(start, |i| i + 1);
    &path[start..end]
}

/// 字节路径切成（父目录，末段）；父目录为空 = 卷根。
fn split_parent_bytes(path: &[u8]) -> VolResult<(&[u8], &[u8])> {
    let path = trim_slashes(path);
    if path.is_empty() {
        return Err(VolError::Unsupported(ERR_UNKNOWN_ENTRY));
    }
    Ok(match path.iter().rposition(|b| *b == b'/') {
        Some(i) if i + 1 < path.len() => (&path[..i], &path[i + 1..]),
        _ => (&path[..0], path),
    })
}

/// 字节路径 → 目录 inode（空 = 根）。
///
/// 上游按**字节**走路径（`open_bytes`），故非 UTF-8 的名字也能定位。
fn dir_ino_bytes(fs: &Filesystem, path: &[u8]) -> VolResult<u64> {
    let path = trim_slashes(path);
    if path.is_empty() {
        return Ok(fs.superblock().rootino);
    }
    let dir = fs.open_bytes(path).map_err(map_err)?;
    if !dir.is_dir() {
        return Err(VolError::Unsupported(ERR_BAD_TYPE));
    }
    Ok(dir.inode().ino)
}

/// 在目录 `dir` 里找 `name`：`(inode, 是否目录, 是否常规文件, 大小)`。
///
/// 上游按**字节**比较目录项名（XFS 的名字是裸字节）；返回取值而非 `File` 句柄——
/// `File` 借的是**打开时那份 inode 快照**，出了目录就没人该拿着它。
fn child_of(fs: &Filesystem, dir: u64, name: &[u8]) -> Result<(u64, bool, bool, u64), XfsError> {
    let parent = fs.open_ino(dir)?;
    let child = parent.open_child(name)?;
    let inode = child.inode();
    Ok((
        inode.ino,
        inode.is_dir(),
        inode.is_regular_file(),
        inode.size,
    ))
}

/// 逐级建目录（已存在 ⇒ 既有目录；既有但不是目录 ⇒ `vol.err.bad-type`）。
///
/// `Append`/`MakeDir` 的「父目录不存在就建」是既有档案语义（zip/7z 会自动建隐式目录）。
fn ensure_dir_bytes(fs: &Filesystem, path: &[u8]) -> VolResult<()> {
    let mut parent = fs.superblock().rootino;
    for segment in path.split(|b| *b == b'/').filter(|s| !s.is_empty()) {
        if segment.len() > NAME_MAX {
            return Err(VolError::Unsupported(ERR_NAME_TOO_LONG));
        }
        parent = match child_of(fs, parent, segment) {
            Ok((ino, is_dir, _, _)) => {
                if !is_dir {
                    return Err(VolError::Unsupported(ERR_BAD_TYPE));
                }
                ino
            }
            Err(XfsError::NotFound) => {
                fs.create_directory(parent, segment, DIR_MODE)
                    .map_err(map_err)?
                    .0
            }
            Err(e) => return Err(map_err(e)),
        };
    }
    Ok(())
}

// ---------------------------------------------------------------- 小工具

/// 只读挂载上的统一拒绝（理由来自 `caps`）。
fn read_only(caps: &FsCaps) -> VolError {
    VolError::ReadOnly(caps.read_only_reason.unwrap_or(ReadOnlyReason::BySetting))
}

/// unix 秒 → `OffsetDateTime`（超出 `time` 的可表示范围 ⇒ `None`，不猜）。
fn unix_time(secs: i64) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::from_unix_timestamp(secs).ok()
}

/// 本地文件的 mtime → `(秒, 纳秒)`；1970 之前的时间取负秒。
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

/// 取消即中断（`entries()`/`apply()` 的每条路径前都问一次）。
fn check_cancel(cancel: &CancellationToken) -> VolResult<()> {
    if cancel.is_cancelled() {
        Err(VolError::Cancelled)
    } else {
        Ok(())
    }
}
