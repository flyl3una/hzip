//! `NtfsVolume`：已挂载的 NTFS 视图（列 / 读 / 变更 / 回放 / 自检）。
//!
//! # 读侧映射（计划 §2.1）
//!
//! | `FsVolume` | 上游 |
//! |---|---|
//! | `entries` | `read::read_dir_entries` 递归（一次 open 走完整棵树）+ `read::read_stat` 取名称/大小/时间 |
//! | `read_at` / `size_of` | `read::read_attribute_range` / `read::read_stat` |
//! | `apply` | `write::{create_file,mkdir,remove,rename,link,unlink,write_file_contents}` |
//!
//! # 写侧的三条硬约束
//!
//! 1. **拒写判定序**（§2.3）：`NtfsVolume::open` 里由 `driver::decide` 定死；进了写路径的
//!    卷只有「可写」一种状态，`apply` 再按 `Change` 逐条做目标级拒写（④目录规模、⑤变换内容）；
//! 2. **不半写**：会**增长**目录索引的操作（建文件/建目录/硬链/改名）在动手前查
//!    [`INDEX_BLOCKS_MAX`] 容量；`Append` 若写完失败，回滚刚建出来的空文件；
//! 3. **改动先落在临时镜像上**：`apply` 只改临时镜像文件，[`NtfsVolume::flush`] 才把
//!    与设备不同的块回放到设备——提交前设备一个字节不动（取消即丢）。

use std::collections::VecDeque;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::PathIo as BlockPathIo;
use fs_ntfs::error::{Error as NtfsError, Kind};
use fs_ntfs::{fsck, mft_io, read, reparse_link_target, write};
use hzip_format::{Change, EntryAttrs, EntryId, EntryInfo, EntryName, NameEncoding};
use hzip_progress::CancellationToken;
use hzip_vol::materialize::SpoolFile;
use hzip_vol::{
    AuditSummary, BlockDevice, BlockRead as _, EntryLimit, EntryTree, FsCaps, FsVolume,
    ReadOnlyReason, VolError, VolResult,
};
use time::OffsetDateTime;

use crate::driver::{self, DRIVER_ID, Decision, Probe};
use crate::names::{self, NAME_MAX};

/// 上游「每目录 64 个索引块」的写上限。
///
/// 来源是上游自己写下的常驻 `$Bitmap:$I30`（8 字节 = 64 位，一位一个索引块，
/// `write.rs` 建 `$INDEX_ALLOCATION` 时写 `[1,0,0,0,0,0,0,0]`）；位图本身要求
/// 非常驻（目录再大）时上游直接拒。本驱动把这条上限**提前**成写前拒绝。
pub const INDEX_BLOCKS_MAX: u64 = 64;

/// `$I30` 目录索引的流名（上游 `mkfs::stream::I30` 同值）。
const I30: &str = "$I30";

/// WOF（Windows Overlay Filter / Compact OS）的重解析标签。
///
/// 这种文件的 `$DATA` 是空的（或稀疏的零），真内容在 `WofCompressedData` 流里 ⇒
/// 上游对它的**内容读**会拒（`read::refuse_wof_compressed`），我们必须更早一步拒，
/// 才好给出稳定键 `vol.unsupported.compressed`。
const REPARSE_TAG_WOF: u32 = fs_ntfs::record_build::reparse_tag::WOF;

/// 与设备对账/回放的块大小（1 MiB；与仓库其它桥接循环同量级）。
const SYNC_CHUNK: usize = 1 << 20;

/// 读本地文件（`Replace`/`Append` 的内容）的分块。
const READ_CHUNK: usize = 1 << 20;

/// FILETIME 与 Unix 纪元的秒差（1601-01-01 → 1970-01-01）。
const NT_EPOCH_SECS: u64 = 11_644_473_600;

// ---------------------------------------------------------------- 属性位
// $STANDARD_INFORMATION.file_attributes（MS-FSCC 2.1.1）。只列我们读的那些。

const FA_READONLY: u32 = 0x0000_0001;
const FA_HIDDEN: u32 = 0x0000_0002;
const FA_SYSTEM: u32 = 0x0000_0004;
const FA_REPARSE_POINT: u32 = 0x0000_0400;
const FA_COMPRESSED: u32 = 0x0000_0800;
const FA_ENCRYPTED: u32 = 0x0000_4000;

/// 条目内容的上游分类（§2.3 ⑤：读/写处置不同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    /// 普通内容（含稀疏：上游按零读，写侧拒——见 [`Content::writable`]）。
    Plain,
    /// `$DATA` 带 COMPRESSED（LZNT1）：上游能解，读得出；写侧拒。
    Lznt1,
    /// `$DATA` 带 SPARSE：读得到零，写侧拒。
    Sparse,
    /// `$DATA` 带 ENCRYPTED（EFS）：没有用户密钥，读不出明文。
    Encrypted,
    /// WOF 重解析（Compact OS / WIMBoot）：内容在 WOF 存储里，**不在 `$DATA`**。
    Wof,
}

impl Content {
    /// 内容能不能读（WOF 的 `$DATA` 是空的、EFS 是密文 ⇒ 拒绝而不是给假字节）。
    fn readable(self) -> bool {
        !matches!(self, Self::Encrypted | Self::Wof)
    }

    /// 内容能不能写（上游写路径只接受未变换的 `$DATA`）。
    fn writable(self) -> bool {
        self == Self::Plain
    }

    /// 稳定键（本轮新增；locale 表按它取值）。
    fn key(self) -> &'static str {
        "vol.unsupported.compressed"
    }

    /// `EntryInfo.codec` / `encryption` 的口径。
    fn codec(self) -> Option<&'static str> {
        match self {
            Self::Lznt1 => Some("lznt1"),
            Self::Wof => Some("wof"),
            Self::Encrypted => Some("efs"),
            Self::Plain | Self::Sparse => None,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Plain => "普通内容",
            Self::Lznt1 => "LZNT1 压缩内容",
            Self::Sparse => "稀疏内容",
            Self::Encrypted => "EFS 加密内容",
            Self::Wof => "WOF（Compact OS）压缩内容",
        }
    }
}

/// 一个条目的上游侧结论。
#[derive(Debug, Clone)]
struct EntryMeta {
    is_dir: bool,
    is_symlink: bool,
    symlink_target: Option<PathBuf>,
    size: u64,
    mtime: Option<OffsetDateTime>,
    attrs: EntryAttrs,
    mode: u32,
    content: Content,
}

impl EntryMeta {
    fn bare(is_dir: bool) -> Self {
        Self {
            is_dir,
            is_symlink: false,
            symlink_target: None,
            size: 0,
            mtime: None,
            attrs: EntryAttrs::default(),
            mode: if is_dir { 0o40755 } else { 0o100644 },
            content: Content::Plain,
        }
    }
}

/// 最近一次 `entries()` 的结果（`EntryId` → 路径 + 元数据）。
///
/// `FsVolume` 的 `read_at(id)` / `size_of(id)` / `apply(Change::*{entry})` 都按 id 引用
/// 条目，而 id 只在**最近一次** `entries()` 的结果内有效——契约如此，这里如实缓存，
/// 越界 id 报错而不猜。
#[derive(Default)]
struct Cache {
    slots: Vec<Slot>,
}

struct Slot {
    path: String,
    meta: EntryMeta,
}

/// 已挂载的 NTFS 卷。
pub struct NtfsVolume {
    dev: Arc<dyn BlockDevice>,
    /// 临时镜像（`hzip-spool-` 前缀）；持有着保证文件活到本卷析构，`Drop` 即删除。
    /// 上游所有调用都走 `image.path()`。
    image: SpoolFile,
    writable: bool,
    reason: Option<ReadOnlyReason>,
    probe: Probe,
    cache: Mutex<Cache>,
    /// 名字里带替换字符 `U+FFFD` 的条目数（见 crate 文档的偏离说明）。
    lossy_names: AtomicU64,
}

impl NtfsVolume {
    /// 构造（`driver::mount` 的尾段：只读探测 + 判定 + 写挂载准备）。
    pub(crate) fn open(
        dev: Arc<dyn BlockDevice>,
        image: SpoolFile,
        probe: Probe,
        decision: Decision,
    ) -> VolResult<Self> {
        let path = image.path().to_path_buf();
        let mut writable = decision.writable;
        let mut reason = decision.reason;
        if writable {
            match driver::prepare_rw(&path, probe.journal_pending) {
                Ok(()) => {}
                // 上游在准备阶段拒了（竞态/更坏的状态）：降级只读而不是让整卷打不开。
                Err(e) => {
                    tracing::warn!("NTFS 写挂载降级为只读: {e}");
                    writable = false;
                    reason = Some(match e {
                        VolError::ReadOnly(r) => r,
                        _ => ReadOnlyReason::JournalPending,
                    });
                }
            }
        }
        if probe.dirty {
            tracing::warn!(
                "NTFS 卷带 dirty 标志（未干净卸载）：上游写挂载会拒绝，故以只读挂载；\
                 请在 Windows 上 chkdsk 后重试"
            );
        }
        if let Some(why) = &probe.hibernated {
            tracing::warn!("NTFS 卷疑似休眠残留（已按只读挂载，不改标志）: {why}");
        }
        Ok(Self {
            dev,
            image,
            writable,
            reason,
            probe,
            cache: Mutex::new(Cache::default()),
            lossy_names: AtomicU64::new(0),
        })
    }

    /// 名字里出现替换字符 `U+FFFD` 的条目数（最近一次 `entries()` 统计）。
    #[must_use]
    pub fn lossy_name_count(&self) -> u64 {
        self.lossy_names.load(Ordering::Relaxed)
    }

    // ------------------------------------------------------------ IO 与工具

    fn open_ro(&self) -> VolResult<BlockPathIo> {
        BlockPathIo::open_ro(self.image.path())
            .map_err(|e| VolError::Image(format!("打开临时镜像: {e}")))
    }

    fn open_rw(&self) -> VolResult<BlockPathIo> {
        BlockPathIo::open_rw(self.image.path())
            .map_err(|e| VolError::Image(format!("打开临时镜像: {e}")))
    }

    /// 条目 id → 卷内路径（`EntryName` 的规范化形态：`a/b`，相对且无前导 `/`）。
    fn path_of(&self, id: EntryId) -> VolResult<String> {
        let cache = self
            .cache
            .lock()
            .map_err(|_| VolError::Image("条目缓存中毒".into()))?;
        cache
            .slots
            .get(id)
            .map(|s| s.path.clone())
            .ok_or_else(|| VolError::Image(format!("条目 id {id} 不在最近一次 entries() 的结果内")))
    }

    /// 把本地文件的 mtime 保真到卷内（`FsCaps::times` 的落点；失败只 warn——
    /// 时间戳不是用户要的那份数据）。
    fn stamp_local_mtime(&self, io: &mut BlockPathIo, path: &str, local: &Path) {
        let Ok(modified) = std::fs::metadata(local).and_then(|m| m.modified()) else {
            return;
        };
        let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH) else {
            return;
        };
        let nt =
            (since.as_secs() + NT_EPOCH_SECS) * 10_000_000 + u64::from(since.subsec_nanos()) / 100;
        let times = write::FileTimes {
            modification: Some(nt),
            ..Default::default()
        };
        if let Err(e) = write::set_times_io(io, path, times) {
            tracing::warn!(path, "设置 mtime 失败: {e}");
        }
    }

    // ------------------------------------------------------------ 条目探测

    /// 读一个条目的元数据（`record_number` 来自目录索引；陈旧/不可读 ⇒ `None`，
    /// **条目照样列出**，只是元数据缺省）。
    fn meta_of(
        &self,
        io: &mut BlockPathIo,
        path: &str,
        record_number: u64,
        is_dir_hint: bool,
    ) -> Option<EntryMeta> {
        let stat = match read::read_stat(io, record_number) {
            Ok(stat) => stat,
            Err(e) => {
                tracing::debug!(path, "读条目元数据失败（索引项陈旧？）: {e}");
                return None;
            }
        };
        let mut meta = EntryMeta::bare(stat.is_dir);
        meta.size = if stat.is_dir { 0 } else { stat.size };
        meta.mtime = OffsetDateTime::from_unix_timestamp(read::nt_to_unix(stat.modified_nt)).ok();
        meta.attrs = entry_attrs(stat.file_attributes);
        if stat.is_dir {
            meta.mode = 0o40755;
        }
        if stat.file_attributes & (FA_REPARSE_POINT | FA_COMPRESSED | FA_ENCRYPTED) != 0 {
            meta.content = self
                .content_of(io, path, record_number)
                .unwrap_or(Content::Plain);
            if stat.file_attributes & FA_REPARSE_POINT != 0 {
                self.decode_link(io, path, &mut meta);
            }
        }
        if !meta.content.writable() {
            // 「条目可见但只读」（§2.3 ⑤）：写侧会拒，列表里如实标出来。
            meta.attrs.read_only = true;
        }
        let _ = is_dir_hint; // 索引项的类型只是提示，`read_stat` 是权威
        Some(meta)
    }

    /// 上游对内容的分类（`$DATA` 标志 + WOF 重解析）。
    fn content_of(&self, io: &mut BlockPathIo, path: &str, record_number: u64) -> Option<Content> {
        if let Ok(Some(rp)) = write::read_reparse_point_io(io, path)
            && rp.reparse_tag == REPARSE_TAG_WOF
        {
            return Some(Content::Wof);
        }
        let (_, record) = mft_io::read_mft_record_io(io, record_number).ok()?;
        let loc = attr_io::find_attribute(&record, AttrType::Data, None)?;
        let flags = attr_io::attribute_flags(&record, &loc).unwrap_or(0);
        Some(classify(flags))
    }

    /// 链接目标（符号链接/联接）：`is_symlink` + 目标。
    fn decode_link(&self, io: &mut BlockPathIo, path: &str, meta: &mut EntryMeta) {
        let Ok(Some(rp)) = write::read_reparse_point_io(io, path) else {
            return;
        };
        let tag = rp.reparse_tag;
        const SYMLINK: u32 = fs_ntfs::record_build::reparse_tag::SYMLINK;
        const MOUNT_POINT: u32 = fs_ntfs::record_build::reparse_tag::MOUNT_POINT;
        const LX_SYMLINK: u32 = fs_ntfs::record_build::reparse_tag::LX_SYMLINK;
        if !matches!(tag, SYMLINK | MOUNT_POINT | LX_SYMLINK) {
            return;
        }
        // hzip 的条目模型只有「符号链接 + 目标」一种链接形态，联接（目录链接）
        // 与 WSL 符号链接都归到这里。
        meta.is_symlink = true;
        meta.mode = 0o120777;
        // 上游 `reparse_link_target` 吃的是带 8 字节头的 `$REPARSE_POINT` 值，
        // 而 `ReparsePoint` 给的是头之后的 DataBuffer ⇒ 按 MS-FSCC 补回头部。
        let mut value = Vec::with_capacity(8 + rp.data.len());
        value.extend_from_slice(&tag.to_le_bytes());
        if let Ok(len) = u16::try_from(rp.data.len()) {
            value.extend_from_slice(&len.to_le_bytes());
        } else {
            return;
        }
        value.extend_from_slice(&[0, 0]);
        value.extend_from_slice(&rp.data);
        meta.symlink_target = reparse_link_target(&value).map(PathBuf::from);
    }

    // ------------------------------------------------------------ 写路径工具

    /// 目录索引的规模：已映射块数、已占用块数、容量块数。
    ///
    /// 容量取常驻 `$Bitmap:$I30` 的位数（上游口径：8 字节 ⇒ 64 块）；位图非常驻时
    /// 上游**不增长**（`IndexBlockAllocator::new` 的 `bitmap_blocks = None`）⇒ 容量记 0。
    fn index_scale(io: &mut BlockPathIo, dir_path: &str) -> VolResult<IndexScale> {
        let Some(dir_rec) = resolve_opt(io, dir_path)? else {
            return Err(VolError::Image(format!("父目录 {dir_path:?} 不存在")));
        };
        let (params, record) = up("读目录记录", mft_io::read_mft_record_io(io, dir_rec))?;
        let Some(ia) = attr_io::find_attribute(&record, AttrType::IndexAllocation, Some(I30))
        else {
            // 常驻 `$INDEX_ROOT` 够装 ⇒ 上游在记录内长大，不受这条上限约束。
            return Ok(IndexScale {
                mapped: 0,
                used: 0,
                capacity: u64::MAX,
            });
        };
        let block_bytes = u64::from(params.index_block_size).max(1);
        let mapped = ia.non_resident_value_length.unwrap_or(0) / block_bytes;
        let (used, capacity) = match attr_io::find_attribute(&record, AttrType::Bitmap, Some(I30)) {
            Some(bitmap) if bitmap.is_resident => {
                let len = bitmap.resident_value_length.unwrap_or(0) as usize;
                let capacity = len as u64 * 8;
                let start = bitmap.attr_offset + bitmap.resident_value_offset.unwrap_or(0) as usize;
                // 记录里读不到位图值 ⇒ 当作「满」（宁可拒，不半写）。
                let used = record
                    .get(start..start + len)
                    .map_or(capacity, |bytes| count_bits(bytes, capacity));
                (used, capacity)
            }
            // 非常驻位图（目录更大）⇒ 上游不增长；位图缺失 ⇒ 同样不敢写。
            Some(_) | None => (0, 0),
        };
        Ok(IndexScale {
            mapped,
            used,
            capacity,
        })
    }

    /// ④ 目录规模：**会增长**该目录索引的操作，写前拒绝（不半写）。
    ///
    /// 判据与上游一致：位图里**没有空位**（`used == capacity`）时，下一次插入若需要
    /// 新块，上游必然拒（`block >= capacity`）。比「映射块数≥上限」更准：删掉条目腾出
    /// 的空位仍可用（那时上游会复用，不必长大）。
    fn check_dir_growth(io: &mut BlockPathIo, dir_path: &str) -> VolResult<()> {
        let scale = Self::index_scale(io, dir_path)?;
        if scale.used >= scale.capacity {
            tracing::warn!(
                dir = dir_path,
                mapped = scale.mapped,
                used = scale.used,
                capacity = scale.capacity,
                "目录索引已无空位（上游每目录 {INDEX_BLOCKS_MAX} 块）⇒ 拒写"
            );
            return Err(VolError::Unsupported("vol.unsupported.dir-too-large"));
        }
        Ok(())
    }

    /// ⑤ 变换过内容（压缩/稀疏/加密/WOF）：**写前拒**。
    fn refuse_transformed(
        io: &mut BlockPathIo,
        path: &str,
        record_number: Option<u64>,
    ) -> VolResult<()> {
        let Some(rec) = (match record_number {
            Some(rec) => Some(rec),
            None => resolve_opt(io, path)?,
        }) else {
            return Ok(());
        };
        let content = Self::classify_path(io, path, rec)?;
        if content.writable() {
            return Ok(());
        }
        tracing::warn!(path, kind = content.describe(), "变换过的内容 ⇒ 拒写");
        Err(VolError::Unsupported(content.key()))
    }

    /// 由路径判内容类别（记录在 base record 里；溢出记录按 `Plain` 保守放行，
    /// 由上游自己拒）。
    fn classify_path(io: &mut BlockPathIo, path: &str, record_number: u64) -> VolResult<Content> {
        if let Ok(Some(rp)) = write::read_reparse_point_io(io, path)
            && rp.reparse_tag == REPARSE_TAG_WOF
        {
            return Ok(Content::Wof);
        }
        let (_, record) = up("读记录", mft_io::read_mft_record_io(io, record_number))?;
        let Some(loc) = attr_io::find_attribute(&record, AttrType::Data, None) else {
            return Ok(Content::Plain);
        };
        Ok(classify(
            attr_io::attribute_flags(&record, &loc).unwrap_or(0),
        ))
    }

    /// 逐级建目录（`a/b/c`）——父目录缺失是既有档案语义（zip/7z 的隐式目录）。
    fn create_dirs(&self, io: &mut BlockPathIo, dir: &str) -> VolResult<()> {
        let mut current = String::new();
        for segment in dir.split('/') {
            if segment.is_empty() {
                continue;
            }
            names::validate_new_name(segment)?;
            let child = join_path(&current, segment);
            if resolve_opt(io, &child)?.is_some() {
                current = child;
                continue;
            }
            Self::check_dir_growth(io, &current)?;
            up("建目录", write::mkdir_io(io, &current, segment))?;
            current = child;
        }
        Ok(())
    }

    /// 读 `Change::Replace`/`Append` 的本地内容（分块读，中途可取消）。
    ///
    /// 上游的整段内容写（`write_file_contents`）只吃 `&[u8]`，故这里必然在内存里
    /// 放一份——大文件的内存开销记在 crate 文档的能力矩阵里。
    fn read_local(path: &Path, cancel: &CancellationToken) -> VolResult<Vec<u8>> {
        let mut file = std::fs::File::open(path).map_err(VolError::Io)?;
        let len = file.metadata().map_err(VolError::Io)?.len();
        let mut out = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            check_cancel(cancel)?;
            let n = file.read(&mut buf).map_err(VolError::Io)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        Ok(out)
    }

    // ------------------------------------------------------------ 五类变更

    fn apply_replace(
        &self,
        io: &mut BlockPathIo,
        path: &str,
        local: &Path,
        cancel: &CancellationToken,
    ) -> VolResult<()> {
        let (parent, base) = split_parent(path);
        let existing = resolve_opt(io, path)?;
        match existing {
            Some(rec) => Self::refuse_transformed(io, path, Some(rec))?,
            None => {
                names::validate_new_name(base)?;
                self.create_dirs(io, parent)?;
                Self::check_dir_growth(io, parent)?;
                up("建文件", write::create_file_io(io, parent, base))?;
                tracing::warn!(path, "Replace 的目标不存在：已先建空文件再写内容");
            }
        }
        let data = Self::read_local(local, cancel)?;
        up("写文件内容", write::write_file_contents_io(io, path, &data))?;
        self.stamp_local_mtime(io, path, local);
        Ok(())
    }

    fn apply_append(
        &self,
        io: &mut BlockPathIo,
        name: &str,
        local: &Path,
        cancel: &CancellationToken,
    ) -> VolResult<()> {
        // 以 `/` 结尾 = 只要目录（与 zip/7z 的隐式目录语义一致）。
        if let Some(dir) = name.strip_suffix('/') {
            return self.create_dirs(io, dir);
        }
        let (parent, base) = split_parent(name);
        names::validate_new_name(base)?;
        self.create_dirs(io, parent)?;
        Self::check_dir_growth(io, parent)?;
        let data = Self::read_local(local, cancel)?;
        up("建文件", write::create_file_io(io, parent, base))?;
        if let Err(e) = up("写文件内容", write::write_file_contents_io(io, name, &data)) {
            // 刚建出来的文件是我们的，写失败就回滚，不给用户留半个空文件。
            match write::unlink_io(io, name) {
                Ok(()) => tracing::warn!(name, "写内容失败，已回滚新建的空文件"),
                Err(undo) => tracing::warn!(name, "写内容失败且回滚失败: {undo}"),
            }
            return Err(e);
        }
        self.stamp_local_mtime(io, name, local);
        Ok(())
    }

    fn apply_delete(&self, io: &mut BlockPathIo, path: &str) -> VolResult<()> {
        // 非空目录 ⇒ 上游 `Kind::NotEmpty` ⇒ `vol.err.dir-not-empty`（不递归删：
        // 镜像里的删除应当是显式的，与 T-IMG5 的 ext4 口径一致）。
        up("删除条目", write::remove_io(io, path))
    }

    fn apply_rename(&self, io: &mut BlockPathIo, from: &str, to: &str) -> VolResult<()> {
        let (old_parent, _) = split_parent(from);
        let (to_parent, to_base) = split_parent(to);
        names::validate_new_name(to_base)?;
        // core 契约：`to` 不含父目录段 ⇒ 空 = 与原目录同一个父目录。
        let new_parent = if to_parent.is_empty() {
            old_parent
        } else {
            to_parent
        };
        if new_parent == old_parent {
            // 同目录改名也可能需要往该目录里插新条目 ⇒ 增长的容量同样要写前查。
            // （上游 `rename` 的文档说「改长度要求常驻 `$INDEX_ROOT`」，那是**过期的
            // 文档**：`write.rs` 的 `#474` 注释与实现都表明溢出目录已走 index 路径，
            // 故这里不按那条旧限制预拒。）
            Self::check_dir_growth(io, old_parent)?;
            if resolve_opt(io, from)?.is_none() {
                return Err(VolError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("改名源不存在: {from}"),
                )));
            }
            return up("改名", write::rename_io(io, from, to_base));
        }
        // 跨目录：NTFS 不能硬链目录 ⇒ 目录搬迁如实拒；文件走 link + unlink
        // （同一条记录改挂名字，内容/时间/记录号都保住）。
        let Some(rec) = resolve_opt(io, from)? else {
            return Err(VolError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("改名源不存在: {from}"),
            )));
        };
        let is_dir = up("读改名源记录", mft_io::read_mft_record_io(io, rec))
            .map(|(_, record)| mft_io::record_flags(&record) & mft_io::MFT_FLAG_DIRECTORY != 0)?;
        if is_dir {
            tracing::warn!(from, to, "跨目录搬目录：NTFS 不能硬链目录 ⇒ 拒");
            return Err(VolError::Unsupported("vol.err.dir-move"));
        }
        Self::refuse_transformed(io, from, Some(rec))?;
        self.create_dirs(io, new_parent)?;
        Self::check_dir_growth(io, new_parent)?;
        up(
            "跨目录改名的硬链",
            write::link_io(io, from, new_parent, to_base),
        )?;
        if let Err(e) = up("跨目录改名的旧名解链", write::unlink_io(io, from)) {
            // 新名已挂上：旧名留着只是多一个硬链，不是损坏 ⇒ 如实回报，不假装成功。
            tracing::warn!(from, "跨目录改名：新名已建，旧名解链失败: {e}");
            return Err(e);
        }
        Ok(())
    }

    fn apply_make_dir(&self, io: &mut BlockPathIo, name: &str) -> VolResult<()> {
        self.create_dirs(io, name)
    }

    // ------------------------------------------------------------ 回放

    /// 把临时镜像里与设备不同的块回放到设备（**只写不同块**：覆盖层的脏块表
    /// 只为真改动记账，不至于整镜像重写）。
    fn write_back(&self) -> VolResult<()> {
        let mut file = std::fs::File::open(self.image.path()).map_err(VolError::Io)?;
        let file_len = file.metadata().map_err(VolError::Io)?.len();
        let len = file_len.min(self.dev.size_bytes());
        let mut mine = vec![0u8; SYNC_CHUNK];
        let mut theirs = vec![0u8; SYNC_CHUNK];
        let mut offset = 0u64;
        while offset < len {
            let want = usize::try_from((len - offset).min(SYNC_CHUNK as u64)).unwrap_or(SYNC_CHUNK);
            file.read_exact(&mut mine[..want]).map_err(VolError::Io)?;
            self.dev
                .read_at(offset, &mut theirs[..want])
                .map_err(VolError::from)?;
            if mine[..want] != theirs[..want] {
                self.dev
                    .write_at(offset, &mine[..want])
                    .map_err(VolError::from)?;
            }
            offset += want as u64;
        }
        Ok(())
    }
}

impl FsVolume for NtfsVolume {
    fn fs_id(&self) -> &'static str {
        DRIVER_ID
    }

    fn caps(&self) -> FsCaps {
        if self.writable {
            FsCaps {
                write: true,
                delete: true,
                rename: true,
                mkdir: true,
                symlink: true,
                times: true,
                // NTFS 的权限模型是 ACL/安全描述符，不是 POSIX mode。
                mode: false,
                name_max: NAME_MAX,
                read_only_reason: None,
            }
        } else {
            let mut caps = FsCaps::read_only(self.reason.unwrap_or(ReadOnlyReason::BySetting));
            caps.name_max = NAME_MAX;
            caps
        }
    }

    fn entries(&self, limit: &EntryLimit, cancel: &CancellationToken) -> VolResult<EntryTree> {
        let max_depth = limit.max_depth.max(1);
        let mut io = self.open_ro()?;
        let mut slots: Vec<Slot> = Vec::new();
        let mut tree: Vec<EntryInfo> = Vec::new();
        let mut lossy = 0u64;
        let mut truncated = false;
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((String::new(), 0));

        'walk: while let Some((dir, depth)) = queue.pop_front() {
            check_cancel(cancel)?;
            let dir_rec = resolve_opt(&mut io, &dir)?
                .ok_or_else(|| VolError::Image(format!("目录 {dir:?} 不存在")))?;
            let listing = up("列目录", read::read_dir_entries(&mut io, dir_rec))?;
            for entry in listing.into_iter().filter(read::listed_to_hosts) {
                check_cancel(cancel)?;
                if tree.len() >= limit.max_entries {
                    truncated = true;
                    break 'walk;
                }
                let full = join_path(&dir, &entry.name);
                if names::is_lossy_name(&full) {
                    lossy += 1;
                }
                let meta = self
                    .meta_of(&mut io, &full, entry.record_number, entry.is_dir)
                    .unwrap_or_else(|| EntryMeta::bare(entry.is_dir));
                let id = tree.len();
                tree.push(entry_info(id, &full, &meta));
                if meta.is_dir && depth + 1 < max_depth {
                    queue.push_back((full.clone(), depth + 1));
                }
                slots.push(Slot { path: full, meta });
            }
        }
        self.lossy_names.store(lossy, Ordering::Relaxed);
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| VolError::Image("条目缓存中毒".into()))?;
        cache.slots = slots;
        Ok(EntryTree {
            entries: tree,
            truncated,
            // NTFS 没有设备/管道/套接字类条目，故「跳过」恒为 0（不是没统计）。
            skipped: 0,
        })
    }

    fn read_at(&self, id: EntryId, offset: u64, buf: &mut [u8]) -> VolResult<usize> {
        let path = self.path_of(id)?;
        let meta = {
            let cache = self
                .cache
                .lock()
                .map_err(|_| VolError::Image("条目缓存中毒".into()))?;
            cache.slots.get(id).map(|s| s.meta.clone())
        };
        let content = meta.as_ref().map_or(Content::Plain, |m| m.content);
        if !content.readable() {
            tracing::warn!(
                path,
                kind = content.describe(),
                "内容不可读 ⇒ 拒（不返回假字节）"
            );
            return Err(VolError::Unsupported(content.key()));
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let mut io = self.open_ro()?;
        let rec = resolve_opt(&mut io, &path)?.ok_or_else(|| {
            VolError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("条目已不在卷内: {path}"),
            ))
        })?;
        let data = up(
            "读内容",
            read::read_attribute_range(&mut io, rec, AttrType::Data, None, offset, buf.len()),
        )?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }

    fn size_of(&self, id: EntryId) -> VolResult<u64> {
        let cached = {
            let cache = self
                .cache
                .lock()
                .map_err(|_| VolError::Image("条目缓存中毒".into()))?;
            cache
                .slots
                .get(id)
                .map(|s| (s.path.clone(), s.meta.clone()))
        };
        let Some((path, meta)) = cached else {
            return Err(VolError::Image(format!(
                "条目 id {id} 不在最近一次 entries() 的结果内"
            )));
        };
        if meta.size != 0 || meta.is_dir {
            return Ok(meta.size);
        }
        // 尺寸为 0 的普通文件：可能是「索引项陈旧」——如实重读记录，读不到就是 0。
        let mut io = self.open_ro()?;
        let Some(rec) = resolve_opt(&mut io, &path)? else {
            return Ok(0);
        };
        match read::read_stat(&mut io, rec) {
            Ok(stat) => Ok(if stat.is_dir { 0 } else { stat.size }),
            Err(e) => {
                tracing::debug!(path, "size_of 重读失败: {e}");
                Ok(0)
            }
        }
    }

    fn apply(
        &mut self,
        change: &Change,
        local: Option<&Path>,
        cancel: &CancellationToken,
    ) -> VolResult<()> {
        if !self.writable {
            return Err(VolError::ReadOnly(
                self.reason.unwrap_or(ReadOnlyReason::DeviceReadOnly),
            ));
        }
        check_cancel(cancel)?;
        let mut io = self.open_rw()?;
        match change {
            Change::Replace { entry, src } => {
                let path = self.path_of(*entry)?;
                // `local` 是上层解析好的本地文件；没有就用 `Change` 自带的 `src`。
                let local = local.unwrap_or(src.as_path());
                self.apply_replace(&mut io, &path, local, cancel)?;
                tracing::debug!(path, "Replace 完成");
            }
            Change::Delete(entry) => {
                let path = self.path_of(*entry)?;
                self.apply_delete(&mut io, &path)?;
                tracing::debug!(path, "Delete 完成");
            }
            Change::Append { src, name } => {
                let local = local.unwrap_or(src.as_path());
                self.apply_append(&mut io, name.as_str(), local, cancel)?;
                tracing::debug!(name = name.as_str(), "Append 完成");
            }
            Change::Rename { from, to } => {
                self.apply_rename(&mut io, from.as_str(), to.as_str())?;
                tracing::debug!(from = from.as_str(), to = to.as_str(), "Rename 完成");
            }
            Change::MakeDir { name } => {
                self.apply_make_dir(&mut io, name.as_str())?;
                tracing::debug!(name = name.as_str(), "MakeDir 完成");
            }
            // `Change` 是 `#[non_exhaustive]`：上游加变体时这里如实说「不认识」，
            // 而不是猜一个动作。
            _ => return Err(VolError::Unsupported("vol.err.unsupported-change")),
        }
        Ok(())
    }

    fn flush(&mut self) -> VolResult<()> {
        if !self.writable {
            // 只读卷没有改动可回放（临时镜像里只有上游只读挂载读过的东西）。
            return Ok(());
        }
        self.write_back()?;
        self.dev.flush().map_err(VolError::from)
    }

    fn audit(&self) -> VolResult<AuditSummary> {
        let mut summary = AuditSummary::default();
        let mut findings: Vec<String> = Vec::new();
        if let Some(why) = &self.probe.hibernated {
            findings.push(format!("{}: {why}", ReadOnlyReason::Hibernated.key()));
        }
        if self.probe.dirty {
            findings.push(format!(
                "{}: 卷 dirty 标志置位（上游要求先在 Windows 上 chkdsk）",
                ReadOnlyReason::Dirty.key()
            ));
        }
        if self.probe.journal_pending {
            findings.push(format!(
                "{}: {}",
                ReadOnlyReason::JournalPending.key(),
                self.probe.journal_state
            ));
        }
        let lossy = self.lossy_name_count();
        if lossy > 0 {
            findings.push(format!(
                "{lossy} 个条目名含 U+FFFD（非法代理对，已按替换字符列出、未丢条目）"
            ));
        }
        // 上游 fsck 的**只读**子集（`check_io` 不写一个字节）：$MFTMirr 对照、
        // 每条在用记录的属性链、$Bitmap 与 run list 对账。不是 Windows chkdsk。
        let mut io = match BlockPathIo::open_ro(self.image.path()) {
            Ok(io) => io,
            Err(e) => {
                return Ok(AuditSummary::not_implemented(&format!(
                    "打开临时镜像失败，自检未跑: {e}"
                )));
            }
        };
        match fsck::check_io(&mut io) {
            Ok(report) => {
                for finding in &report.findings {
                    findings.push(format!("{finding:?}"));
                }
            }
            Err(e) => {
                return Ok(AuditSummary::not_implemented(&format!(
                    "上游 fsck 只读子集未跑完: {e}"
                )));
            }
        }
        summary.findings = findings;
        summary.skipped = false;
        Ok(summary)
    }
}

// ---------------------------------------------------------------- 自由函数

/// 上游结果 + 上下文 → [`VolResult`]（驱动里唯一的错误映射点）。
fn up<T>(what: &str, result: Result<T, NtfsError>) -> VolResult<T> {
    result.map_err(|e| driver::map_fs_error(what, e))
}

/// 解析路径；`Kind::NotFound` 折成 `Ok(None)`（「不存在」是正常分支）。
fn resolve_opt(io: &mut BlockPathIo, path: &str) -> VolResult<Option<u64>> {
    match write::resolve_path_to_record_number_io(io, path) {
        Ok(rec) => Ok(Some(rec)),
        Err(e) if e.kind() == Kind::NotFound => Ok(None),
        Err(e) => Err(driver::map_fs_error(&format!("解析路径 {path:?}"), e)),
    }
}

/// 卷内路径拼接（条目名形态：**相对、无前导 `/`**，与 `hzip-vol` 的条目名口径和
/// T-IMG5 的 ext4 驱动一致；根下的条目就是 `file.txt`）。
fn join_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// `a/b` → (`a`, `b`)；`b` → (`""`, `b`)。
fn split_parent(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(at) => (&path[..at], &path[at + 1..]),
        None => ("", path),
    }
}

/// 目录索引的规模（[`NtfsVolume::index_scale`] 的返回值）。
#[derive(Debug, Clone, Copy)]
struct IndexScale {
    /// `$INDEX_ALLOCATION:$I30` 已映射的索引块数。
    mapped: u64,
    /// `$Bitmap:$I30` 里置位的块数。
    used: u64,
    /// `$Bitmap:$I30` 能描述的块数（常驻位数；非常驻时 0 = 上游不增长）。
    capacity: u64,
}

/// 数位图前 `cap` 位里置位了多少。
fn count_bits(bytes: &[u8], cap: u64) -> u64 {
    let mut used = 0;
    for (index, byte) in bytes.iter().enumerate() {
        for bit in 0..8u32 {
            if (index as u64) * 8 + u64::from(bit) >= cap {
                return used;
            }
            if byte & (1 << bit) != 0 {
                used += 1;
            }
        }
    }
    used
}

/// `$DATA` 的属性标志 → [`Content`]（顺序：加密 > 压缩 > 稀疏）。
fn classify(flags: u16) -> Content {
    if flags & attr_io::attr_flags::ENCRYPTED != 0 {
        Content::Encrypted
    } else if flags & attr_io::attr_flags::COMPRESSION_MASK != 0 {
        Content::Lznt1
    } else if flags & attr_io::attr_flags::SPARSE != 0 {
        Content::Sparse
    } else {
        Content::Plain
    }
}

/// `$STANDARD_INFORMATION.file_attributes` → `EntryAttrs`。
fn entry_attrs(file_attributes: u32) -> EntryAttrs {
    EntryAttrs {
        read_only: file_attributes & FA_READONLY != 0,
        hidden: file_attributes & FA_HIDDEN != 0,
        system: file_attributes & FA_SYSTEM != 0,
        // Windows 按扩展名推断可执行性 ⇒ 这里如实给「未知」，归 core。
        executable: None,
    }
}

/// 条目元数据 → `EntryInfo`（`EntryInfo` 没有 `Default`，逐字段给全）。
fn entry_info(id: EntryId, path: &str, meta: &EntryMeta) -> EntryInfo {
    EntryInfo {
        id,
        name: EntryName::new(path),
        is_dir: meta.is_dir,
        is_symlink: meta.is_symlink,
        symlink_target: meta.symlink_target.clone(),
        size_raw: if meta.is_dir { 0 } else { meta.size },
        // 卷内内容是原样存放的（除上面标记的变换内容）⇒ 占用 = 原始长度。
        size_packed: if meta.is_dir { 0 } else { meta.size },
        mtime: meta.mtime,
        mode: Some(meta.mode),
        attributes: meta.attrs,
        encrypted: meta.content == Content::Encrypted,
        header_encrypted: false,
        crc32: None,
        codec: meta.content.codec().map(Into::into),
        name_encoding: if names::is_lossy_name(path) {
            // 名字里有替换字符 ⇒ 编码状态「可疑」，别宣称是干净的 UTF-8。
            NameEncoding::Unknown
        } else {
            NameEncoding::Utf8
        },
        encryption: if meta.content == Content::Encrypted {
            Some("EFS")
        } else {
            None
        },
        compression_level: None,
    }
}

/// `CancellationToken` → 取消错误（每个可中断点都查）。
fn check_cancel(cancel: &CancellationToken) -> VolResult<()> {
    if cancel.is_cancelled() {
        Err(VolError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
