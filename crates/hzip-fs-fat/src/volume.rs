//! FAT 卷：挂载、递归列表、惰性读、五类变更、轻量自检。
//!
//! # `Change` → `fatfs` 映射
//!
//! | `Change` | 落地 | 备注 |
//! |---|---|---|
//! | `Replace { entry, src }` | `create_file`（FAT 层「建或覆盖」）→ `truncate` → 写内容 | id 取不到（越界/已删）⇒ 稳定键拒绝，不猜路径。**写前先查 4 GiB 上限** |
//! | `Delete(id)` | `Dir::remove` | 目录**不递归删**；非空 ⇒ `vol.err.dir-not-empty` |
//! | `Append { src, name }` | 逐级 `create_dir` + `create_file` + 写内容 | 同名已存在 ⇒ 覆盖（＝ zip `-a` 的更新语义） |
//! | `Rename { from, to }` | `Dir::rename(from, root, to)` | `to` 不含 `/` ⇒ `from` 的父目录下的新名字（core 契约）；含 `/` ⇒ 卷内完整路径（跨目录搬移） |
//! | `MakeDir { name }` | 逐级 `create_dir` | 已存在 ⇒ 幂等成功 |
//!
//! # 为什么**每次操作现挂**（而不是长期持有 `fatfs::FileSystem`）
//!
//! `fatfs::FileSystem` 不是 `Send`（它内嵌 `FsOptions`，里面是
//! `&'static dyn TimeProvider`/`&'static dyn OemCpConverter`，两个 trait 都没有
//! `Sync` 约束），而 [`hzip_vol::FsVolume`] 要求 `Send + Sync`。诚实的解法不是
//! `unsafe` 假装它是 `Send`，而是：**文件系统句柄只在一次调用内存在**
//! （挂载 → 用 → `unmount`/drop 落盘），跨调用保存的只有设备句柄与 id→路径表。
//! 代价是每次操作多读一个引导扇区（FAT32 再读一个 FSInfo 扇区）——量级与一次
//! 目录项查找相同，且上层通常还压着 `hzip-vol` 的读缓存。
//!
//! `fatfs` 本来就假设设备**独占**（同一设备上挂两个 `FileSystem` 会互相踩 FAT 表），
//! 「一次调用一个句柄」天然满足这条。
//!
//! # 条目 id 与「路径表」
//!
//! `EntryId` 是「[`FsVolume::entries`] 压入顺序的下标」。本驱动把该顺序对应的
//! 相对路径记在 `slots` 里（[`Slot`]），于是 `read_at`/`size_of`/`apply` 能按 id
//! 找回路径。变更只做**定点维护**（改名改前缀、删除打墓碑、新建追加），故 id 跨
//! 变更保持稳定——调用方拿着一张列表连做多条变更时不会串行。
//!
//! # 时间戳
//!
//! FAT 存的是 **本地时间**（无时区，2 s 精度）。这里按 **UTC** 解释（与 ext4 夹具
//! `FIXED_UNIX_SECONDS` 同刻）：两种文件系统的同一份夹具给出同一个时间点，一致性
//! 优先于本地化；界面显示时再按本地时区渲染。
//!
//! # 无日志 ⇒ 风险如实标注
//!
//! 目录项、FAT 链、数据簇之间**没有日志**：中断可能留下「目录项指向未写 FAT 链」
//! 这类不一致。本驱动不做假承诺：`audit()` 返回
//! [`AuditSummary::not_implemented`]（键 `vol.warn.no-journal`），并**额外**做一次
//! 轻量自检（根目录 + 一层子目录的目录项与簇链可达性）——那是抽样，不是 fsck。
//! 写回的安全性由 `hzip-vol` 的默认覆盖写层保证（提交前原档一个字节不动）。

use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use fatfs::{Dir, FileAttributes, FileSystem, FsOptions as FatfsOptions, ReadWriteSeek};
use hzip_format::{Change, EntryAttrs, EntryId, EntryInfo, NameEncoding};
use hzip_progress::CancellationToken;
use hzip_vol::{
    AuditSummary, BlockDevice, EntryLimit, EntryTree, FsCaps, FsOptions, FsVolume, ReadOnlyReason,
    VolError, VolResult,
};
use time::{Month, OffsetDateTime, PrimitiveDateTime, Time as ClockTime};

use crate::io::BlockDeviceSeek;

// ---------------------------------------------------------------- 常量与稳定键

/// FAT 单文件的**硬**上限：目录项里的 size 是 `u32`，故最大 4 GiB - 1。
pub const FAT_MAX_FILE_BYTES: u64 = u32::MAX as u64;

/// 拷贝缓冲（64 KiB：与仓库其它桥接循环同量级）。
const COPY_CHUNK: usize = 64 * 1024;

/// LFN 名字上限（**字节**；上游 `fatfs` 按 UTF-8 字节数判，非 ASCII 名于是更短）。
const NAME_MAX_BYTES: usize = 255;

/// 自检最多扫多少条目（防坏镜像把 `hzip t` 拖死）。
const AUDIT_MAX_ENTRIES: usize = 4096;

/// 单文件超 FAT 上限（`vol.err.too-large`）。
///
/// 注意：`hzip-vol` 的 [`VolError`] 本期没有「文件过大」这一变体（该 crate 不在本
/// 波次的改动范围），故稳定键经 `Unsupported(&'static str)` 承载——键本身仍是原文
/// 冻结的那一个，UI 按键查 locale 不受影响。
pub const ERR_TOO_LARGE: &str = "vol.err.too-large";
/// 目录非空（不递归删：镜像里的删除应当是显式的）。
pub const ERR_DIR_NOT_EMPTY: &str = "vol.err.dir-not-empty";
/// 改名/新建的目标已存在。
pub const ERR_EXISTS: &str = "vol.err.exists";
/// 名字超 LFN 上限。
pub const ERR_NAME_TOO_LONG: &str = "vol.err.name-too-long";
/// 源不是常规文件（目录/FIFO/设备）。
pub const ERR_SOURCE_NOT_FILE: &str = "vol.err.source-not-file";
/// `EntryId` 不在本次列表里（调用方没先 `entries()`，或 id 越界）。
pub const ERR_UNKNOWN_ENTRY: &str = "vol.err.unknown-entry";
/// 该 id 指向的条目已被本次变更删掉。
pub const ERR_ENTRY_GONE: &str = "vol.err.entry-gone";
/// 本驱动不认识的 `Change` 变体（`Change` 是 `#[non_exhaustive]`）。
pub const ERR_UNKNOWN_CHANGE: &str = "vol.err.unknown-change";

// ---------------------------------------------------------------- 数据类型

/// 一个条目的卷内相对路径（`/` 分隔、无前导 `/`）。
///
/// `slots` 的下标即 `EntryId`；`deleted` 是墓碑（删除只标记，不改下标 —— id 稳定）。
#[derive(Debug, Clone)]
struct Slot {
    name: String,
    is_dir: bool,
    deleted: bool,
}

/// 已挂载的 FAT 卷（[`hzip_vol::FsVolume`] 实现）。
pub struct FatVolume {
    /// 设备（每次操作现挂文件系统；见模块文档）。
    dev: Arc<dyn BlockDevice>,
    /// id → 路径表（`entries()` 重建；变更定点维护）。
    slots: Mutex<Vec<Slot>>,
    /// 能力（挂载时定死：设备只读/按设置只读 ⇒ 写侧全关）。
    caps: FsCaps,
}

impl FatVolume {
    /// 挂载一个 FAT12/16/32 卷。
    ///
    /// # 只读判定序
    ///
    /// ① 设备本身不可写（只读介质/权限/`ReadOnlyDevice`）⇒ [`ReadOnlyReason::DeviceReadOnly`]；
    /// ② 调用方要求只读（[`FsOptions::writable`] 为假）⇒ [`ReadOnlyReason::BySetting`]。
    ///
    /// # 读不能改档案
    ///
    /// 挂载期显式 `update_accessed_date(false)`：不读改写镜像（hzip 的红线；
    /// `fatfs` 的默认值也是 false，但这条我们不复用默认值——它太重要）。
    pub fn mount(dev: Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<Self> {
        let writable = dev.is_writable();
        let fs = mount_fs(dev.clone())?;
        tracing::debug!(
            fat = ?fs.fat_type(),
            label = %fs.volume_label(),
            cluster = fs.cluster_size(),
            device_writable = writable,
            "FAT 卷已挂载"
        );
        // 挂载期的活就到此为止：句柄用完即走（`Drop` 的 unmount 对只读访问是空操作）。
        drop(fs);
        let caps = if !writable {
            FsCaps::read_only(ReadOnlyReason::DeviceReadOnly)
        } else if !opts.writable {
            FsCaps::read_only(ReadOnlyReason::BySetting)
        } else {
            FsCaps {
                write: true,
                delete: true,
                rename: true,
                mkdir: true,
                // FAT 无符号链接/权限位：能力如实为假，不假装能写。
                symlink: false,
                // 只有 mtime（2 s 精度）；`Change` 里也没有「改时间」这一类。
                times: true,
                mode: false,
                name_max: NAME_MAX_BYTES,
                read_only_reason: None,
            }
        };
        if !caps.write {
            tracing::debug!(
                reason = ?caps.read_only_reason,
                "FAT 卷以只读挂载（写侧能力全关）"
            );
        }
        Ok(Self {
            dev,
            slots: Mutex::new(Vec::new()),
            caps,
        })
    }

    /// 现挂一个文件系统句柄（每次操作一个；见模块文档）。
    fn fs(&self) -> VolResult<FileSystem<BlockDeviceSeek>> {
        mount_fs(self.dev.clone())
    }

    /// 取 id → 路径表锁。
    ///
    /// 中毒（持锁期间发生过 panic）不是「继续用」的信号：报错，让用户重开镜像。
    fn slots(&self) -> VolResult<MutexGuard<'_, Vec<Slot>>> {
        self.slots
            .lock()
            .map_err(|_| VolError::Image("FAT 卷状态已中毒（前次操作发生 panic）".to_string()))
    }
}

impl std::fmt::Debug for FatVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FatVolume")
            .field("caps", &self.caps)
            .field("size", &self.dev.size_bytes())
            .finish()
    }
}

/// `fatfs` 挂载选项：**必须**关掉访问日期更新（读不能改档案）。
fn fat_options() -> FatfsOptions {
    FatfsOptions::new().update_accessed_date(false)
}

/// 现挂一个 `fatfs` 文件系统（挂载失败 ⇒ 结构错误，不是 IO，也不是 panic）。
fn mount_fs(dev: Arc<dyn BlockDevice>) -> VolResult<FileSystem<BlockDeviceSeek>> {
    FileSystem::new(BlockDeviceSeek::new(dev), fat_options()).map_err(mount_err)
}

/// 挂载失败：BPB/FSInfo 结构不合法 ⇒ `vol.err` 结构错误。
fn mount_err(e: std::io::Error) -> VolError {
    if let Some(inner) = e.get_ref().and_then(|r| r.downcast_ref::<VolError>()) {
        return match inner {
            VolError::ReadOnly(r) => VolError::ReadOnly(*r),
            VolError::Image(m) => VolError::Image(m.clone()),
            other => VolError::Image(format!("块设备错误：{other}")),
        };
    }
    VolError::Image(format!("FAT 引导扇区/FSInfo 不合法：{e}"))
}

/// 写在**只读挂载**上的统一拒绝（理由来自 `caps`）。
fn read_only(caps: &FsCaps) -> VolError {
    VolError::ReadOnly(caps.read_only_reason.unwrap_or(ReadOnlyReason::BySetting))
}

/// 写前硬门（`Replace`/`Append` 共用）：落盘后的文件长度超 FAT 上限 ⇒ `vol.err.too-large`。
///
/// **必须在截断/分配任何簇之前调用**：`fatfs` 到达 4 GiB 时只是把 `write` 返回 0
/// （`write_all` 于是报 `WriteZero`）——那意味着已经写了 4 GiB 才失败，也就是半写。
pub fn check_file_size(final_len: u64) -> VolResult<()> {
    if final_len > FAT_MAX_FILE_BYTES {
        return Err(VolError::Unsupported(ERR_TOO_LARGE));
    }
    Ok(())
}

// ---------------------------------------------------------------- FsVolume

impl FsVolume for FatVolume {
    fn fs_id(&self) -> &'static str {
        "fat"
    }

    fn caps(&self) -> FsCaps {
        self.caps
    }

    fn entries(&self, limit: &EntryLimit, cancel: &CancellationToken) -> VolResult<EntryTree> {
        let mut slots = self.slots()?;
        let fs = self.fs()?;
        let mut walker = Walker {
            limit,
            cancel,
            cluster: fs.cluster_size(),
            tree: EntryTree::default(),
            slots: Vec::new(),
        };
        let root = fs.root_dir();
        walker.walk_level(&root, "", 0)?;
        tracing::debug!(
            entries = walker.tree.entries.len(),
            truncated = walker.tree.truncated,
            skipped = walker.tree.skipped,
            "FAT 列目录"
        );
        *slots = walker.slots;
        Ok(walker.tree)
    }

    fn read_at(&self, id: EntryId, offset: u64, buf: &mut [u8]) -> VolResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let slots = self.slots()?;
        let slot = slot_of(&slots, id)?;
        if slot.is_dir {
            return Ok(0); // 目录没有文件内容（投影层只对文件调它）
        }
        // 任何 FAT 文件都短于 4 GiB：越过即 EOF，**也避开** `fatfs` 内部对 u32 的夹取。
        if offset > FAT_MAX_FILE_BYTES {
            return Ok(0);
        }
        let name = slot.name.clone();
        drop(slots);
        let fs = self.fs()?;
        let (parent, base) = split_parent(&name)?;
        let dir = open_dir_or_root(&fs, parent)?;
        let mut file = dir.open_file(base).map_err(from_io)?;
        file.seek(SeekFrom::Start(offset)).map_err(from_io)?;
        file.read(buf).map_err(from_io)
    }

    fn size_of(&self, id: EntryId) -> VolResult<u64> {
        let slots = self.slots()?;
        let slot = slot_of(&slots, id)?;
        if slot.is_dir {
            return Ok(0);
        }
        let name = slot.name.clone();
        drop(slots);
        let fs = self.fs()?;
        Ok(stat_entry(&fs, &name)?.1)
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
        let mut slots = self.slots()?;
        let fs = self.fs()?;
        match change {
            Change::Replace { entry, src } => {
                let source = local.unwrap_or(src.as_path());
                let name = slot_of(&slots, *entry)?.name.clone();
                write_local(&fs, &name, source, cancel)?;
                tracing::debug!(name, source = %source.display(), "FAT Replace");
            }
            Change::Delete(entry) => {
                let slot = slot_of(&slots, *entry)?.clone();
                if slot.is_dir && dir_has_children(&fs, &slot.name)? {
                    // 目录不递归删（与 ext4 侧同口径）：镜像里的删除必须是显式的。
                    return Err(VolError::Unsupported(ERR_DIR_NOT_EMPTY));
                }
                fs.root_dir().remove(&slot.name).map_err(from_io)?;
                slot_of_mut(&mut slots, *entry)?.deleted = true;
                tracing::debug!(name = %slot.name, "FAT Delete");
            }
            Change::Append { src, name } => {
                let full = name.as_str();
                if full.ends_with('/') {
                    // `Change` 契约（与 ext4 侧同口径）：名字以 `/` 结尾 ⇒ 只建目录
                    // （zip/7z 的「隐式目录」语义）。
                    make_dir(&fs, full, &mut slots)?;
                    tracing::debug!(name = full, "FAT Append（只建目录）");
                } else {
                    let source = local.unwrap_or(src.as_path());
                    // 逐级建父目录（既有档案语义：zip/7z 自动建隐式目录）。
                    ensure_dirs(&fs, full)?;
                    write_local(&fs, full, source, cancel)?;
                    if !slots.iter().any(|s| !s.deleted && s.name == full) {
                        slots.push(Slot {
                            name: full.to_string(),
                            is_dir: false,
                            deleted: false,
                        });
                    }
                    tracing::debug!(name = full, source = %source.display(), "FAT Append");
                }
            }
            Change::Rename { from, to } => {
                let src_name = from.as_str();
                let dst_name = destination_name(src_name, to.as_str());
                if dst_name == src_name {
                    return Ok(()); // 改到同一个名字：没事发生（也不是错误）
                }
                // 同一个根 Dir 作 src/dst：fatfs 的 `rename` 支持目标带路径 ⇒ 跨目录搬移
                // 走的是同一条 API（它会「拷目录项 + 链搬迁」）。
                let root = fs.root_dir();
                root.rename(src_name, &root, &dst_name).map_err(from_io)?;
                rename_slots(&mut slots, src_name, &dst_name);
                tracing::debug!(from = src_name, to = %dst_name, "FAT Rename");
            }
            Change::MakeDir { name } => {
                let full = name.as_str();
                make_dir(&fs, full, &mut slots)?;
                tracing::debug!(name = full, "FAT MakeDir");
            }
            _ => return Err(VolError::Unsupported(ERR_UNKNOWN_CHANGE)),
        }
        // 显式卸载：写路径上要**报得出** FSInfo/dirty 位的落盘失败（drop 会吞掉它）。
        fs.unmount().map_err(from_io)?;
        self.dev.flush()?;
        Ok(())
    }

    fn flush(&mut self) -> VolResult<()> {
        if !self.caps.write {
            return Ok(()); // 只读挂载：没有一个字节是我们写的，也就没什么要落
        }
        // 写路径每次都已 unmount（FSInfo + 清 dirty 位），这里再做一次是**幂等**的
        // 保险：调用方可能在别处（例如直接经设备）改过东西。
        let fs = self.fs()?;
        fs.unmount().map_err(from_io)?;
        self.dev.flush()?;
        Ok(())
    }

    fn audit(&self) -> VolResult<AuditSummary> {
        // FAT 没有 fsck 可跑 ⇒ 如实标注「未做一致性检查」，**不冒充检查过了**。
        // 键 `vol.warn.no-journal`：无日志 ⇒ 中断可能留下不一致（见 crate 文档）。
        let mut summary = AuditSummary::not_implemented("vol.warn.no-journal");
        let fs = self.fs()?;
        light_check(&fs, &mut summary);
        Ok(summary)
    }
}

// ---------------------------------------------------------------- 列表

/// 列表累积器：`tree.entries` 与 `slots` **同步压入** ⇒ `EntryId` = 下标。
struct Walker<'a> {
    limit: &'a EntryLimit,
    cancel: &'a CancellationToken,
    /// 簇大小（算「盘上占用」）。
    cluster: u32,
    tree: EntryTree,
    slots: Vec<Slot>,
}

impl Walker<'_> {
    /// 列 `dir` 一层，再逐个子目录递归（前缀 = 卷内相对路径）。
    fn walk_level<T: ReadWriteSeek>(
        &mut self,
        dir: &Dir<'_, T>,
        prefix: &str,
        depth: usize,
    ) -> VolResult<()> {
        if depth > self.limit.max_depth {
            self.tree.truncated = true; // 深度到顶：如实标「部分列出」，不硬闯
            return Ok(());
        }
        let mut subdirs: Vec<(String, String)> = Vec::new();
        for item in dir.iter() {
            if self.tree.entries.len() >= self.limit.max_entries {
                self.tree.truncated = true;
                return Ok(());
            }
            check_cancel(self.cancel)?;
            let entry = item.map_err(from_io)?;
            let short = entry.short_file_name();
            if short == "." || short == ".." {
                continue; // FAT 的目录自指项：列出来只会成环
            }
            let name = entry.file_name();
            let full = join(prefix, &name);
            let Ok(entry_name) = hzip_format::parse_entry_name(&full) else {
                // 名字不可规范化（坏 LFN 里的控制字符等）：跳过并**计数**。
                self.tree.skipped += 1;
                continue;
            };
            let is_dir = entry.is_dir();
            let size = if is_dir { 0 } else { entry.len() };
            self.tree.entries.push(EntryInfo {
                id: self.slots.len(),
                name: entry_name,
                is_dir,
                is_symlink: false,
                symlink_target: None,
                size_raw: size,
                size_packed: pack_size(size, self.cluster),
                mtime: offset_from_fat(entry.modified()),
                mode: None, // FAT 无权限位（`caps.mode == false`）
                attributes: attrs_of(entry.attributes()),
                encrypted: false,
                header_encrypted: false,
                crc32: None,
                codec: None, // 卷内文件不经压缩层
                name_encoding: NameEncoding::Utf8,
                encryption: None,
                compression_level: None,
            });
            self.slots.push(Slot {
                name: full.clone(),
                is_dir,
                deleted: false,
            });
            if is_dir {
                subdirs.push((name, full));
            }
        }
        for (name, full) in subdirs {
            if self.tree.entries.len() >= self.limit.max_entries {
                self.tree.truncated = true;
                break;
            }
            check_cancel(self.cancel)?;
            let Ok(child) = dir.open_dir(&name) else {
                // 目录项在、簇链坏 ⇒ 子树列不出来：计为「跳过」，不让整棵树失败。
                self.tree.skipped += 1;
                continue;
            };
            self.walk_level(&child, &full, depth + 1)?;
        }
        Ok(())
    }
}

/// 拼接卷内路径（父为空 ⇒ 就是名字；不产生前导 `/`）。
fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

/// 盘上占用（FAT 按簇分配；0 字节文件不占簇）。
fn pack_size(size: u64, cluster: u32) -> u64 {
    let cluster = u64::from(cluster);
    if size == 0 || cluster == 0 {
        return 0;
    }
    size.div_ceil(cluster) * cluster
}

/// FAT 属性位 → [`EntryAttrs`]（FAT 只有三个属性位与我们相关）。
fn attrs_of(attrs: FileAttributes) -> EntryAttrs {
    EntryAttrs {
        read_only: attrs.contains(FileAttributes::READ_ONLY),
        hidden: attrs.contains(FileAttributes::HIDDEN),
        system: attrs.contains(FileAttributes::SYSTEM),
        executable: None, // FAT 不表达可执行（core 按扩展名推断）
    }
}

/// FAT 的 DOS 日期/时间 → [`OffsetDateTime`]（按 UTC 解释，见模块文档）。
fn offset_from_fat(dt: fatfs::DateTime) -> Option<OffsetDateTime> {
    let month = Month::try_from(u8::try_from(dt.date.month).ok()?).ok()?;
    let date = time::Date::from_calendar_date(
        i32::from(dt.date.year),
        month,
        u8::try_from(dt.date.day).ok()?,
    )
    .ok()?;
    let clock = ClockTime::from_hms_milli(
        u8::try_from(dt.time.hour).ok()?,
        u8::try_from(dt.time.min).ok()?,
        u8::try_from(dt.time.sec).ok()?,
        dt.time.millis,
    )
    .ok()?;
    Some(PrimitiveDateTime::new(date, clock).assume_utc())
}

// ---------------------------------------------------------------- id 表

/// 取 id → 路径（墓碑/越界分别给稳定键）。
fn slot_of(slots: &[Slot], id: EntryId) -> VolResult<&Slot> {
    match slots.get(id) {
        Some(s) if !s.deleted => Ok(s),
        Some(_) => Err(VolError::Unsupported(ERR_ENTRY_GONE)),
        None => Err(VolError::Unsupported(ERR_UNKNOWN_ENTRY)),
    }
}

/// 取 id → 路径（可变；墓碑/越界同 [`slot_of`]）。
fn slot_of_mut(slots: &mut [Slot], id: EntryId) -> VolResult<&mut Slot> {
    match slots.get_mut(id) {
        Some(s) if !s.deleted => Ok(s),
        Some(_) => Err(VolError::Unsupported(ERR_ENTRY_GONE)),
        None => Err(VolError::Unsupported(ERR_UNKNOWN_ENTRY)),
    }
}

/// 改名后同步路径表：本条目 + （目录则）整棵子树改前缀。
fn rename_slots(slots: &mut [Slot], from: &str, to: &str) {
    let prefix = format!("{from}/");
    for slot in slots.iter_mut() {
        if slot.deleted {
            continue;
        }
        if slot.name == from {
            slot.name = to.to_string();
        } else if slot.name.starts_with(&prefix) {
            let rest = slot.name[prefix.len()..].to_string();
            slot.name = format!("{to}/{rest}");
        }
    }
}

// ---------------------------------------------------------------- 路径与写入

/// 路径切成（父目录，末段）；父目录为空串表示卷根。
fn split_parent(name: &str) -> VolResult<(&str, &str)> {
    let name = name.trim_matches('/');
    if name.is_empty() {
        return Err(VolError::Unsupported(ERR_UNKNOWN_ENTRY));
    }
    Ok(match name.rsplit_once('/') {
        Some((parent, base)) if !base.is_empty() => (parent, base),
        _ => ("", name),
    })
}

/// 打开父目录（空 = 根）。
fn open_dir_or_root<'f, T: ReadWriteSeek>(
    fs: &'f FileSystem<T>,
    parent: &str,
) -> VolResult<Dir<'f, T>> {
    let root = fs.root_dir();
    if parent.is_empty() {
        Ok(root)
    } else {
        root.open_dir(parent).map_err(from_io)
    }
}

/// 逐级建目录（已存在 ⇒ 既有目录；`fatfs::Dir::create_dir` 天然幂等）。
fn ensure_dirs<T: ReadWriteSeek>(fs: &FileSystem<T>, name: &str) -> VolResult<()> {
    let segments: Vec<&str> = name.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() <= 1 {
        return Ok(());
    }
    let mut dir = fs.root_dir();
    for segment in &segments[..segments.len() - 1] {
        dir = dir.create_dir(segment).map_err(from_io)?;
    }
    Ok(())
}

/// 建目录（逐级建父目录；已存在 ⇒ 幂等成功），并把新目录登记进 id 表。
fn make_dir<T: ReadWriteSeek>(
    fs: &FileSystem<T>,
    full: &str,
    slots: &mut Vec<Slot>,
) -> VolResult<()> {
    let (parent, base) = split_parent(full)?;
    if base.len() > NAME_MAX_BYTES {
        return Err(VolError::Unsupported(ERR_NAME_TOO_LONG));
    }
    ensure_dirs(fs, full)?;
    open_dir_or_root(fs, parent)?
        .create_dir(base)
        .map_err(from_io)?;
    if !slots.iter().any(|s| !s.deleted && s.name == full) {
        slots.push(Slot {
            name: full.to_string(),
            is_dir: true,
            deleted: false,
        });
    }
    Ok(())
}

/// 把本地文件内容写进 `name`（不存在则新建，存在则**截断重写**）。
///
/// 顺序很关键：先查上限（[`check_file_size`]）→ 再截断 → 再写。
///
/// 写途中的失败（盘满、设备报错）会留下「部分内容」——FAT 没有日志，写一半只能
/// 是一半。**原档的安全性不靠这里**：改动先落 `hzip-vol` 的覆盖层，提交前原文件
/// 一个字节不动，中止即丢。
fn write_local<T: ReadWriteSeek>(
    fs: &FileSystem<T>,
    name: &str,
    src: &Path,
    cancel: &CancellationToken,
) -> VolResult<()> {
    let (parent, base) = split_parent(name)?;
    if base.len() > NAME_MAX_BYTES {
        return Err(VolError::Unsupported(ERR_NAME_TOO_LONG));
    }
    let meta = std::fs::metadata(src)?;
    if !meta.is_file() {
        return Err(VolError::Unsupported(ERR_SOURCE_NOT_FILE));
    }
    let expected = meta.len();
    check_file_size(expected)?; // ← 写前拒绝：不截断、不分配簇、不半写
    let mut input = std::fs::File::open(src)?;

    let dir = open_dir_or_root(fs, parent)?;
    let mut out = dir.create_file(base).map_err(from_io)?;
    out.seek(SeekFrom::Start(0)).map_err(from_io)?;
    out.truncate().map_err(from_io)?; // 等价 `OpenOptions::new().truncate(true)`
    let mut buf = vec![0u8; COPY_CHUNK];
    let mut written = 0u64;
    loop {
        check_cancel(cancel)?;
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(from_io)?;
        written += n as u64;
    }
    out.flush().map_err(from_io)?; // 落目录项（size/mtime）
    tracing::debug!(name, written, expected, "FAT 写内容");
    Ok(())
}

/// 目标路径：`to` 不含 `/` ⇒ **`from` 的父目录**下的新名字（core 契约「不含父目录段」）；
/// `to` 含 `/` ⇒ 视为卷内完整路径（跨目录搬移）。
fn destination_name(from: &str, to: &str) -> String {
    if to.contains('/') {
        return to.trim_matches('/').to_string();
    }
    match from.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{to}"),
        None => to.to_string(),
    }
}

/// 目录是否还有条目（忽略 `.`/`..`）：非空 ⇒ 不删（不递归）。
fn dir_has_children<T: ReadWriteSeek>(fs: &FileSystem<T>, name: &str) -> VolResult<bool> {
    let dir = fs.root_dir().open_dir(name).map_err(from_io)?;
    for item in dir.iter() {
        let entry = item.map_err(from_io)?;
        let short = entry.short_file_name();
        if short != "." && short != ".." {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 从目录项读（is_dir, size）——**不读文件数据**，也不走簇链。
fn stat_entry<T: ReadWriteSeek>(fs: &FileSystem<T>, name: &str) -> VolResult<(bool, u64)> {
    let (parent, base) = split_parent(name)?;
    let dir = open_dir_or_root(fs, parent)?;
    for item in dir.iter() {
        let entry = item.map_err(from_io)?;
        if entry.file_name() == base || entry.short_file_name() == base {
            return Ok((entry.is_dir(), entry.len()));
        }
    }
    Err(VolError::Image(format!("FAT 目录项缺失：{name}")))
}

// ---------------------------------------------------------------- 自检与错误

/// 轻量自检（**不是 fsck**）：根目录 + 一层子目录的目录项可读、簇链可达。
///
/// 发现的每一条都进 `findings`（`skipped` 仍为真：我们确实没做一致性检查）。
fn light_check(fs: &FileSystem<BlockDeviceSeek>, summary: &mut AuditSummary) {
    let root = fs.root_dir();
    if let Err(msg) = scan_dir(&root) {
        summary.findings.push(format!("根目录：{msg}"));
    }
    let mut names = Vec::new();
    for item in root.iter() {
        match item {
            Ok(entry) => {
                let short = entry.short_file_name();
                if entry.is_dir() && short != "." && short != ".." {
                    names.push(entry.file_name());
                }
            }
            Err(e) => {
                summary.findings.push(format!("根目录：{e}"));
                return;
            }
        }
        if names.len() >= AUDIT_MAX_ENTRIES {
            break;
        }
    }
    for name in names {
        match root.open_dir(&name) {
            Ok(dir) => {
                if let Err(msg) = scan_dir(&dir) {
                    summary.findings.push(format!("目录 {name}：{msg}"));
                }
            }
            Err(e) => summary.findings.push(format!("目录 {name} 打不开：{e}")),
        }
    }
}

/// 扫一层的目录项（触发簇链遍历与名字解码）；返回第一条问题。
fn scan_dir<T: ReadWriteSeek>(dir: &Dir<'_, T>) -> Result<(), String> {
    let mut seen = 0usize;
    for item in dir.iter() {
        let entry = item.map_err(|e| format!("目录项不可读：{e}"))?;
        let _ = entry.file_name();
        seen += 1;
        if seen >= AUDIT_MAX_ENTRIES {
            break;
        }
    }
    Ok(())
}

/// `fatfs` 的 `io::Error` → [`VolError`]（语义类别尽量保住）。
fn from_io(e: std::io::Error) -> VolError {
    // 适配器把设备层错误包成 `io::Error::other(VolError)`（见 `io.rs`）：解回来保真。
    if let Some(inner) = e.get_ref().and_then(|r| r.downcast_ref::<VolError>()) {
        return match inner {
            VolError::ReadOnly(r) => VolError::ReadOnly(*r),
            VolError::Cancelled => VolError::Cancelled,
            VolError::Unsupported(k) => VolError::Unsupported(k),
            VolError::Quota(k) => VolError::Quota(k),
            VolError::Image(m) => VolError::Image(m.clone()),
            other => VolError::Image(format!("块设备错误：{other}")),
        };
    }
    match e.kind() {
        std::io::ErrorKind::AlreadyExists => VolError::Unsupported(ERR_EXISTS),
        std::io::ErrorKind::PermissionDenied => VolError::ReadOnly(ReadOnlyReason::DeviceReadOnly),
        // 目录项/簇链与预期不符（按名字找不到、目录自相矛盾）⇒ 结构问题，不是 IO 故障。
        std::io::ErrorKind::NotFound => VolError::Image(format!("FAT 结构与预期不符：{e}")),
        _ => VolError::Io(e),
    }
}

/// 取消检查（每条变更、每个目录项一次）。
fn check_cancel(cancel: &CancellationToken) -> VolResult<()> {
    if cancel.is_cancelled() {
        Err(VolError::Cancelled)
    } else {
        Ok(())
    }
}
