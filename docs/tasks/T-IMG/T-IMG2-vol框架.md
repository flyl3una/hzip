# T-IMG2：`hzip-vol` 卷层框架

| 项目 | 内容 |
|---|---|
| 阶段 | P0 地基（所有镜像容器与文件系统驱动都建在它上面） |
| 依赖 | T-IMG1（`VolumeWriteMode`、`supports_in_place_edit()`、`write_in_place`） |
| 状态 | **已实施（7/8 模块）**（2026-10-16）：`error.rs`（错误模型 + 9 个理由键）· `materialize.rs`（spool，`hzip-spool-` 前缀）· `overlay.rs`（覆盖设备：读穿透 + 写时拷贝 + 只回放脏块 + 幂等）· `device.rs`（**三态 Chain**：就地/覆盖/只读 + 就地失败降级 + real/spool 分离）· `partition.rs`（GPT/MBR + 子设备切片 + 标签/类型名）· `registry.rs`（FS 驱动注册表 + 头部复核，经 `VolOptions.fs_registry` 进容器）· `reader.rs`（**分区即顶层目录** + `volume_rows()` 元数据）；**32 条测试全绿**、clippy `-D warnings` 干净。余 `editor.rs`（`ArchiveEditor`：`rewrite`/`write_in_place` 装配，写路径 T-IMG6/T-IMG9 用） |
| 目标 | 用**一个块设备接口**把「镜像容器 → 分区 → 文件系统」三层接起来，并把「写模式 A/B/只读」收敛成设备链末端的一个选择；向上只暴露 `hzip-format` 的既有契约 |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 新 | `crates/hzip-vol/Cargo.toml` | 依赖（`=` 精确锁）：`rust-fs-core = "=0.3.11"`、`rust-disk-partitions = "=0.7.1"`、`hzip-format`、`hzip-error`、`hzip-platform`（TempArea）、`hzip-progress`；dev-dep：`tempfile` |
| 新 | `src/lib.rs` | 契约 `VolumeHandler` / `Volume` / `FsDriver` / `FsVolume` + 数据类型 |
| 新 | `src/error.rs` | `VolError`、`ReadOnlyReason`（**文案键**）、`From<VolError> for HzipError` |
| 新 | `src/overlay.rs` | `OverlayDevice`：读穿透 + 脏块表 + 只回放脏块 + 整流量拷 |
| 新 | `src/device.rs` | `Chain`：源 → 设备（`ReadOnly` / `InPlace` / `Overlay` 三种末端）+ LRU 缓存装配 |
| 新 | `src/materialize.rs` | `Source::Stream` → 临时文件（**按既有 spool 约定**：前缀 `hzip-spool-` + `NamedTempFile` 保活 + 计数超限即 `Quota`；预检由 core 经 `VolOptions.spool` 传入，依据 `crates/hzip-core/src/services/space.rs`） |
| 新 | `src/partition.rs` | GPT/MBR 探测（`disk_partitions::probe`/`sniff`）+ 每卷 `OwnedRwSlice` |
| 新 | `src/registry.rs` | FS 驱动注册表：`FsKind` → `FsDriver`（feature 门控 `vol-fs-ext4` 等） |
| 新 | `src/reader.rs` | `VolumeReader: ArchiveReader`：卷树投影、惰性 `read_entry`、`extract`、`test`(=audit) |
| 新 | `src/editor.rs` | `VolumeEditor: ArchiveEditor`：`rewrite`（整流量拷）/ `write_in_place`（提交回放） |
| 新 | `tests/{overlay.rs,partition.rs,reader.rs}` | 框架自测（全部用 mock/内存设备，不依赖真实镜像） |
| 改 | 根 `Cargo.toml` | `members` 加 `crates/hzip-vol`；`[workspace.dependencies]` 登记上游三件 |
| 改 | `build/checks/deps-direction.py` | 登记边：`hzip-vol → hzip-format/hzip-platform`、`hzip-fmt-* → hzip-vol`、`hzip-fs-* → hzip-vol` |

## 步骤 0：上游签名核实（写死进代码前必做，30 分钟）

本文件的代码骨架已按 docs.rs 签名写就，但**以下构造点尚未核实**，落地时先按下面顺序核一遍（核完把结果写回注释）：

| 待核 | 在哪看 |
|---|---|
| `OwnedRwSlice::new(...)` / `SliceReader::new(...)` 的参数与错误 | <https://docs.rs/rust-fs-core/latest/fs_core/struct.OwnedRwSlice.html> |
| `CachingDevice::new/read_only`、`CountingDevice` 的构造与统计读取 | <https://docs.rs/rust-fs-core/latest/fs_core/struct.CachingDevice.html> |
| `fs_core::Error` 变体（`VolError` 的映射目标） | <https://docs.rs/rust-fs-core/latest/fs_core/enum.Error.html> |
| `Partition` 字段名（`start`/`length` 已见 README，余下确认）与 `probe` 的错误类型 | <https://docs.rs/rust-disk-partitions/latest/disk_partitions/struct.Partition.html> |
| `TempArea::new_session` / `materialize` 的确切签名 | `crates/hzip-platform/src/temp_area.rs:89-140` |
| spool 约定（前缀/超龄回收/预检函数）| `crates/hzip-core/src/services/space.rs:1-63`（`SPOOL_PREFIX`、`SPOOL_STALE_AFTER`、`precheck_open_temp_space` 的文档解释了为什么「打开」也要预检） |
| 一个格式 crate 如何 spool（保活模式） | `crates/hzip-fmt-zip/src/reader.rs:99-143`（`Materialized { path, file, keep: Option<NamedTempFile> }`） |

```bash
cargo doc -p rust-fs-core -p rust-disk-partitions --no-deps --open   # 本地生成后逐条核对
```

## 步骤 1：写失败测试

`crates/hzip-vol/tests/overlay.rs`：

```rust
const BLK: u64 = 64 * 1024;

/// 内存设备：既是 base 也是差异对照。
#[derive(Default)]
struct MemDevice(std::sync::Mutex<Vec<u8>>);
impl fs_core::BlockRead for MemDevice {
    fn read_at(&self, off: u64, buf: &mut [u8]) -> fs_core::Result<()> { /* 越界报错，否则拷贝 */ }
    fn size_bytes(&self) -> u64 { self.0.lock().unwrap().len() as u64 }
}
impl fs_core::BlockDevice for MemDevice {
    fn write_at(&self, off: u64, buf: &[u8]) -> fs_core::Result<()> { /* 写入 */ }
    fn is_writable(&self) -> bool { true }
}

#[test]
fn reads_through_until_written() {
    let base = Arc::new(MemDevice::new(vec![0xAA; (3 * BLK) as usize]));
    let ov = OverlayDevice::new(base.clone(), tempfile::tempfile().unwrap(), BLK);
    assert_eq!(read_byte(&ov, 0), 0xAA, "未写块必须穿透到 base");
    ov.write_at(BLK + 5, &[0x11, 0x22]).unwrap();
    assert_eq!(read_bytes(&ov, BLK + 5, 2), vec![0x11, 0x22]);
    assert_eq!(read_byte(&base, BLK + 5), 0xAA, "提交前原设备一字节不动");
    assert_eq!(ov.dirty_blocks(), 1, "只一个脏块（写的块粒度）");
}

#[test]
fn replay_into_touches_only_dirty_blocks() {
    let target = MemDevice::new(vec![0x77; (3 * BLK) as usize]);
    /* 写第 2 块 → replay_into(&target) */
    assert_eq!(read_byte(&target, 0), 0x77, "未脏块不得被回放覆盖");
    assert_eq!(ov.replay_into(&target).unwrap(), 1);
}

#[test]
fn replay_is_idempotent_and_stream_matches_device() {
    /* replay_into ×2 → 目标字节一致（幂等：core 的密码重试会重复调用） */
    /* stream_out 全量 == 直接逐块读 OverlayDevice（含 base 命中部分） */
}
```

`crates/hzip-vol/tests/partition.rs`：手工构造 512 B MBR（`55 AA` + 两个表项）与 GPT 头（`EFI PART`）⇒ `partitions()` 的卷数/`start`/`len` 正确；全零设备 ⇒ `partitions()` 为空且 `open_fs(None)` 走整卷；损坏表（表项越界）⇒ `VolError::Image`（**不 panic**）。

`crates/hzip-vol/tests/reader.rs`（mock `Volume`/`FsVolume`）：

```rust
#[test]
fn entries_are_a_flat_tree_with_partition_prefix() {
    // 两个分区：p1 = FAT32 两文件，p2 = ext4 一目录一文件
    // ⇒ 名字序列：1. EFI System (FAT32)/EFI/、1. …/EFI/BOOTX64.EFI、2. root (ext4)/etc/、2. …/etc/fstab
    // 且 is_dir 正确、mtime/size 来自 FsVolume
}

#[test]
fn read_entry_is_random_and_repeatable() { /* id 乱序、重复读同一 id，内容一致 */ }

#[test]
fn entry_limit_and_cancel_stop_listing() { /* limit 命中 ⇒ 截断并置部分标记；cancel ⇒ Cancelled */ }

#[test]
fn unsupported_fs_shows_unknown_volume_and_is_read_only() { /* sniff=None ⇒ 分区仍在，但无子条目、caps_edit=false */ }
```

## 步骤 2：实现

### 2.1 契约（`src/lib.rs`，落地即冻结）

```rust
pub use error::{ReadOnlyReason, VolError, VolResult};
pub use fs_core::{BlockDevice, BlockRead};

/// 卷容器：镜像文件 → 一个可随机读写的虚拟块设备。
pub trait VolumeHandler: Send + Sync {
    fn id(&self) -> &'static str;                                   // "qcow2" / "vmdk" / "raw"
    fn open(&self, dev: Arc<dyn BlockDevice>, opts: &VolOptions) -> VolResult<Box<dyn Volume>>;
}

#[derive(Debug, Clone, Default)]
pub struct VolOptions {
    /// 允许向上游要 r/w（false ⇒ 一律只读打开，写模式开关为「只读」时用）
    pub writable: bool,
    /// LRU 读缓存块数（0 = 不装；CachingDevice）
    pub cache_blocks: usize,
    /// spool 策略：目录 + 文件名前缀（`hzip-spool-`）+ 允许的最大字节数。
    /// 由 core 填（依据 `services::space::{SPOOL_PREFIX, SPOOL_STALE_AFTER, precheck_open_temp_space}`）；
    /// `hzip-vol` 自己不依赖 `hzip-core`，只执行并对超限报 `VolError::Quota`。
    pub spool: SpoolPolicy,
}

#[derive(Debug, Clone)]
pub struct SpoolPolicy {
    /// 临时目录（默认 `hzip_platform::dirs::temp_root()`）
    pub dir: PathBuf,
    /// 文件名前缀（**必须** `hzip-spool-`，否则启动期孤儿回收认不出）
    pub prefix: &'static str,
    /// 允许的最大 spool 字节（`None` = 不设上限，仅受磁盘可用空间约束）
    pub max_bytes: Option<u64>,
}

pub trait Volume: Send {
    fn geometry(&self) -> Geometry;
    /// MBR/GPT 探测结果；**空 = 无分区表**（整卷即一个文件系统卷）
    fn partitions(&self) -> VolResult<Vec<PartitionView>>;
    /// `None` = 无分区表时用整卷；`Ok(None)` = 该卷文件系统不认识
    fn open_fs(&self, part: Option<PartitionId>) -> VolResult<Option<Box<dyn FsVolume>>>;
    fn caps(&self) -> VolCaps;
    fn flush(&mut self) -> VolResult<()>;
}

/// 文件系统驱动（ext4 / ntfs / fat 各一个实现，`registry.rs` 按 `FsKind` 挑）。
pub trait FsDriver: Send + Sync {
    fn id(&self) -> &'static str;
    fn supports(&self, kind: FsKind, head: &[u8]) -> bool;           // 嗅探结论 + 头部复核
    fn mount(&self, dev: Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<Box<dyn FsVolume>>;
}

pub trait FsVolume: Send + Sync {
    fn fs_id(&self) -> &'static str;
    fn caps(&self) -> FsCaps;
    /// 全树（只读元数据）。实现在内部做条目上限与取消检查，超限置 `truncated`。
    fn entries(&self, limit: &EntryLimit, cancel: &CancellationToken) -> VolResult<EntryTree>;
    /// 惰性读：任意偏移、乱序、重复可用（对应 `ArchiveReader` 红线 2）
    fn read_at(&self, id: EntryId, offset: u64, buf: &mut [u8]) -> VolResult<usize>;
    fn size_of(&self, id: EntryId) -> VolResult<u64>;
    fn apply(&mut self, change: &Change, local: Option<&Path>, cancel: &CancellationToken) -> VolResult<()>;
    fn flush(&mut self) -> VolResult<()>;
    /// 提交前一致性自检（ext4 = `Filesystem::audit`；未实现者返回 `Ok(AuditSummary::skipped())`）
    fn audit(&self) -> VolResult<AuditSummary>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsCaps {
    pub write: bool, pub delete: bool, pub rename: bool, pub mkdir: bool,
    pub symlink: bool, pub times: bool, pub mode: bool,
    pub name_max: usize,
    pub read_only_reason: Option<ReadOnlyReason>,
}

#[derive(Debug, Clone)]
pub struct VolCaps {
    pub writable: bool,
    /// 提交可**只回放脏块**（源是常规文件；流源只能整流量拷）
    pub in_place: bool,
    pub read_only_reason: Option<ReadOnlyReason>,
}

#[derive(Debug, Clone)]
pub struct PartitionView {
    pub id: PartitionId,          // = 序号（usize）
    pub display: String,          // "1. EFI System (FAT32)"
    pub kind: PartitionKind,      // 复用 disk_partitions::PartitionKind
    pub fs: Option<FsKind>,       // None = 未识别卷
    pub start: u64,
    pub len: u64,
}
```

`src/error.rs`（**拒绝理由即文案键**，T-IMG10 校验键齐）：

```rust
#[derive(Debug, thiserror::Error)]
pub enum VolError {
    #[error("块设备 IO: {0}")] Io(#[from] std::io::Error),
    #[error("镜像格式: {0}")] Image(String),
    #[error("只读: {0}")] ReadOnly(ReadOnlyReason),
    #[error("不支持: {0}")] Unsupported(&'static str),
    #[error("配额: {0}")] Quota(&'static str),
    #[error("已取消")] Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyReason {
    Dirty,            // vol.readonly.dirty
    JournalPending,   // vol.readonly.journal
    Hibernated,       // vol.readonly.hibernated
    Encrypted,        // vol.readonly.encrypted
    Layout,           // vol.readonly.layout（vmdk 非 sparse）
    FsUnknown,        // vol.readonly.fs-unknown
    DeviceReadOnly,   // vol.readonly.device
    UserSetting,      // vol.readonly.by-setting（写模式开关选了只读）
}
impl ReadOnlyReason { pub fn key(self) -> &'static str { /* 上表右列 */ } }

impl From<VolError> for HzipError {
    fn from(e: VolError) -> Self {
        match e {
            VolError::Io(source) => HzipError::Io { source, path: None },
            VolError::Cancelled => HzipError::Cancelled,
            VolError::Quota(k) => HzipError::Quota { quota: QuotaKind::MaxTotalBytes },
            VolError::ReadOnly(r) => HzipError::Unsupported { what: r.key().into() },
            VolError::Unsupported(k) => HzipError::Unsupported { what: k.into() },
            VolError::Image(m) => HzipError::Format {
                format: FormatId::from("vol"),
                kind: FormatErrorKind::BadHeader,
            },
        }
    }
}
```

> `VolError::Image(m)` 的 `m` 落地时经 `tracing::debug!` 落日志（`HzipError::Format` 不带自由文本）。

### 2.2 `OverlayDevice`（`src/overlay.rs`）

```rust
pub const OVERLAY_BLOCK: u64 = 64 * 1024;      // ≥ qcow2/vmdk grain 上限，减少回放碎片

pub struct OverlayDevice {
    base: Arc<dyn BlockRead>,
    spill: std::fs::File,                       // 与虚拟长度等长的稀疏溢出文件
    dirty: Mutex<BTreeSet<u64>>,                // 脏块号
    block: u64,
}

impl OverlayDevice {
    pub fn new(base: Arc<dyn BlockRead>, spill: std::fs::File, block: u64) -> Self;
    pub fn dirty_blocks(&self) -> u64;
    /// 只把脏块写回 `target`（源是常规文件时用；**幂等**）
    pub fn replay_into(&self, target: &dyn BlockDevice) -> VolResult<u64>;
    /// 整流量拷（源是流/需重写外层时用；`cancel` 每块检查）
    pub fn stream_out(&self, w: &mut dyn Write, cancel: &CancellationToken) -> VolResult<u64>;
}

impl BlockRead for OverlayDevice {
    /// 按块分流：脏块 → spill；未脏块 → base（**不做写时拷贝**：不必先把整块抄进 spill）
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()>;
    fn size_bytes(&self) -> u64 { self.base.size_bytes() }
}

impl BlockDevice for OverlayDevice {
    fn write_at(&self, offset: u64, buf: &[u8]) -> fs_core::Result<()>;   // 写 spill + 标记脏块
    fn flush(&self) -> fs_core::Result<()>;                              // spill.sync_all()
    fn is_writable(&self) -> bool { true }
    fn set_len(&self, _n: u64) -> fs_core::Result<()> { /* Err：本期不做容量变更 */ }
    fn can_grow(&self) -> bool { false }
}
```

### 2.3 设备链（`src/device.rs`）——**写模式开关的全部实现就在这**

```rust
pub enum WriteMode { ReadOnly, InPlace, Overlay }
impl From<hzip_format::VolumeWriteMode> for WriteMode { /* 直通 */ }

pub struct Chain {
    mode: WriteMode,
    device: Arc<dyn BlockDevice>,
    base: Option<fs_core::FileDevice>,         // InPlace 的真文件 / Overlay 的 base
    overlay: Option<Arc<OverlayDevice>>,
    real_path: Option<PathBuf>,                // 有值 ⇒ 提交可只回放脏块
    _tmp: Option<hzip_platform::TempArea>,     // 物化临时件生命周期守卫（drop 即清理）
}

impl Chain {
    pub fn open(src: Source, mode: WriteMode, opts: &VolOptions) -> VolResult<Chain> {
        let (real_path, tmp) = match src {
            Source::File(p) => (Some(p), None),
            // 流/卷组源：先物化（与 hzip-fmt-zip/src/reader.rs:111 同款）
            other => { let t = materialize::stream_to_temp(other, ...)?; (Some(t.path()), Some(t)) }
        };
        let p = real_path.clone().expect("物化后必有路径");
        let device: Arc<dyn BlockDevice> = match mode {
            WriteMode::ReadOnly => Arc::new(ReadOnlyDevice::new(fs_core::FileDevice::open(&p)?)),
            // 打不开 r/w（只读介质/权限）⇒ 降级为 Overlay 并记理由
            WriteMode::InPlace => match fs_core::FileDevice::open_rw(&p) { Ok(d) => Arc::new(d), Err(_) => /* → Overlay */ },
            WriteMode::Overlay => {
                let base = Arc::new(fs_core::FileDevice::open(&p)?);
                let spill = tempfile::tempfile_in(hzip_platform::dirs::temp_root())?;
                Arc::new(OverlayDevice::new(base.clone(), spill, OVERLAY_BLOCK))
            }
        };
        let device = if opts.cache_blocks > 0 { Arc::new(CachingDevice::new(device, opts.cache_blocks)) } else { device };
        Ok(Chain { mode, device, .. })
    }

    /// 提交：Overlay ⇒ 只回放脏块到真文件（或调用方给的 `dst`）；InPlace ⇒ flush；ReadOnly ⇒ 不做事
    pub fn commit(&self, dst: Option<&dyn BlockDevice>) -> VolResult<CommitStats>;

    /// 源是流（组合链/档案内条目）：把整卷写进外层 `Sink`（外层重写由 core 收尾）
    pub fn stream_into(&self, w: &mut dyn Write, cancel: &CancellationToken) -> VolResult<u64>;

    /// 重置到干净中间态（core 的密码重试会重复调用 `write_in_place`）
    pub fn reset(&mut self) -> VolResult<()>;
}
```

### 2.4 分区层（`src/partition.rs`）

```rust
pub fn probe_partitions(dev: &Arc<dyn BlockDevice>) -> VolResult<(Option<TableKind>, Vec<PartitionView>)> {
    match disk_partitions::probe(&**dev) {
        Ok((table, parts)) => { /* 每卷 sniff 一次 → FsKind；display = "{i+1}. {label_or_kind} ({fs})" */ }
        Err(disk_partitions::Error::NoTable) => Ok((None, Vec::new())),   // 变体名以 docs.rs 为准
        Err(e) => Err(VolError::Image(format!("partition-table: {e}"))),
    }
}

/// 每卷一个可写子设备（分区切片）；无分区表 ⇒ 整卷。
pub fn slice(dev: &Arc<dyn BlockDevice>, view: &PartitionView) -> Arc<dyn BlockDevice> { /* OwnedRwSlice */ }
```

### 2.5 `VolumeReader`（`src/reader.rs`）——条目投影与惰性读

```rust
impl ArchiveReader for VolumeReader {
    fn caps(&self) -> FormatCaps {
        FormatCaps {
            read: true,
            edit: self.fs_caps.write,
            // 「可就地编辑」不在 FormatCaps 里：由 handler 的 supports_in_place_edit() 声明
            // （见 T-IMG1 §0 落地记录），reader 只报 read/edit
            ..Default::default()
        }
    }
    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> { Ok(self.recs.iter().map(|r| r.info.clone()).collect()) }

    /// 惰性：不落临时文件，按 1 MiB 块拉取（预览只读头部 ⇒ 代价极小）
    fn read_entry(&self, id: EntryId, _pw: PasswordRef<'_>) -> Result<Box<dyn Read + Send>, HzipError> {
        let rec = self.recs.get(id).ok_or_else(|| HzipError::Unsupported { what: format!("hz-5003 no-entry-{id}").into() })?;
        Ok(Box::new(FsEntryReader::new(self.fs_for(rec.part)?, id, self.fs.size_of(id)?)))
    }
    fn extract(&self, sel: &Selection, ctx: &ExtractCtx<'_>, dest: &Path) -> Result<ExtractReport, HzipError> { /* 逐条 read_entry → 净化落盘（复用既有 fmt 惯例与 ctx 的冲突/配额/净化） */ }
    /// 对卷而言「测试」= 一致性自检（`fsck` 子集）
    fn test(&self, _ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> { /* FsVolume::audit */ }
}
```

**树形 → 扁平投影**（§4.3 的实现要点）：

```text
1. EFI System (FAT32)/                 ← 分区层：is_dir = true，容器类型列 = FAT32
1. EFI System (FAT32)/EFI/             ← 文件系统目录
1. EFI System (FAT32)/EFI/BOOTX64.EFI  ← 文件（size/mtime 来自 FsVolume）
2. root (ext4)/etc/fstab
```

### 2.6 `VolumeEditor`（`src/editor.rs`）——形状先立，语义在 T-IMG6 补满

```rust
pub struct VolumeEditor { chain: Mutex<Chain>, handler: Box<dyn VolumeHandler>, mode: WriteMode, src_is_file: bool }

impl ArchiveEditor for VolumeEditor {
    fn rewrite(&self, changes: ChangeSet, dst: Sink, ctx: &WriteCtx<'_>) -> Result<WriteReport, HzipError> {
        // ① chain.reset() ② 应用 changes（经 FsVolume::apply）③ 交付：
        //    Sink::File(p)  ⇒ fs::copy(base, p) + 回放脏块（OS 级拷贝，仍不解码）
        //    Sink::Stream(w)⇒ chain.stream_into(w)（整量走外层编码链）
    }
    fn write_in_place(&self, changes: ChangeSet, ctx: &WriteCtx<'_>) -> Result<WriteReport, HzipError> {
        // ① chain.reset() ② 应用 changes ③ audit（失败 ⇒ 丢弃覆盖层，原档不变）④ chain.commit(None)
    }
}
```

## 步骤 3：跑测试

```bash
cargo test -p hzip-vol
python build/checks/deps-direction.py        # violations=0
cargo clippy --workspace --all-targets -- -D warnings
```

### 步骤 4：提交

```bash
git add crates/hzip-vol Cargo.toml build/checks/deps-direction.py
git commit -m "feat(vol): 卷层框架——块设备契约、写模式设备链、覆盖设备、分区层、条目投影"
```

## 验收

- [ ] `cargo test -p hzip-vol` 全绿（overlay / partition / reader 三组）
- [ ] 覆盖层三条不变量有专测：**提交前原设备不变**、**只回放脏块**、**回放幂等**
- [ ] `VolumeReader::entries()` 对 mock 卷给出 §4.3 的扁平树（含分区前缀与 `is_dir`）
- [ ] `read_entry` 乱序/重复/取消可用；`test()` 走 audit
- [ ] `python build/checks/deps-direction.py` violations=0；`cargo clippy` 0 警告

## 风险 / 注意

| 点 | 处置 |
|---|---|
| `BlockRead/BlockDevice` 方法都是 `&self` ⇒ 内部可变性 | 覆盖设备用 `Mutex<BTreeSet<u64>>`；spill 文件用 `File::write_all_at`/`seek+write`（Windows 与 Unix 都用既有 `FileDevice` 的位置写模式，避免自带平台分支） |
| 覆盖层块粒度与上游簇/grain 不一致会放大回放 | 统一 64 KiB（≥ qcow2 默认 64 KiB / vmdk grain 64 KiB），回放按块合并 |
| 内存：`CachingDevice` 块数默认值 | `VolOptions.cache_blocks` 由 core 按配置给（默认 8192 块 ≈ 512 MiB 上限，按 `04` 体积/内存口径复核） |
| 惰性 `read_entry` 返回的流与 `FsVolume` 生命周期 | `FsEntryReader` 持 `Arc<dyn FsVolume>`；`ArchiveReader` 的 `&self` 语义下不得持有 `&self.vol` 借用 |
| `Source::Volumes`（分卷） | 本期不支持：`materialize` 阶段直接 `VolError::Unsupported("vol.reject.multi-volume")`（与 `registry.rs:171` 既有「组合装配拒绝流源」口径一致） |
