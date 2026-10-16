# T-IMG3：`hzip-fmt-raw` 容器 + `hzip-fs-ext4` 只读适配 + 镜像夹具

| 项目 | 内容 |
|---|---|
| 阶段 | P1 只读闭环（**内部里程碑 M1 的前半**；用户要求「一起发」⇒ M1 不单独发版） |
| 依赖 | T-IMG2 |
| 状态 | **夹具部分已实施**（`hzip-format-testkit/src/vol/**` + `tests/vol_fixtures.rs` 11 条自证全绿）；容器与 ext4 只读驱动待做 |
| 目标 | 打通「最简镜像（raw）+ 首发文件系统（ext4）」的**读**侧：能列出分区、列出 ext4 目录树、按条目读出字节；夹具不依赖任何外部工具 |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 新 | `crates/hzip-fmt-raw/src/{lib.rs,handler.rs,probe.rs,volume.rs}` | `RawHandler`：探针（无魔数）+ `Volume`（无表/有表两态）+ `ArchiveReader` 交给 `hzip-vol::reader` 组装 |
| 新 | `crates/hzip-fs-ext4/src/{lib.rs,driver.rs,volume.rs}` | `Ext4Driver: FsDriver` + `Ext4Volume: FsVolume`（**本任务只读**；写侧 T-IMG5） |
| 新 | `crates/hzip-format-testkit/src/vol/{mod.rs,mbr.rs,gpt.rs,mem.rs,ext4.rs,fat.rs}` | 内存块设备 + MBR/GPT 构造 + ext4/FAT 小镜像（`vol` feature 门控，默认关） |
| 改 | `crates/hzip-format-testkit/Cargo.toml` | 新增 `[features] vol = ["dep:rust-fs-core","dep:rust-fs-ext4","dep:fatfs"]` |
| 改 | 根 `Cargo.toml` | members 加两个新 crate；`[workspace.dependencies]` 登记 `rust-fs-ext4`、`fatfs` |

## 步骤 0：前置核实

| 待核 | 依据 |
|---|---|
| `fs_ext4::mkfs` 的调用形状（能否直接对内存设备格式化） | <https://docs.rs/rust-fs-ext4/latest/fs_ext4/mkfs/index.html> |
| `fs_ext4::dir::DirEntry` 字段名（inode/name/file_type） | <https://docs.rs/rust-fs-ext4/latest/fs_ext4/dir/struct.DirEntry.html> |
| `fs_ext4::Inode` 字段名（size/mode/mtime/links） | <https://docs.rs/rust-fs-ext4/latest/fs_ext4/struct.Inode.html> |
| `fs_ext4::Filesystem::mount` 是否对**只读设备**自动只读挂载 | `mount` 文档：「Read-only unless the device reports …」 |
| `fatfs::format_volume` 与 `Cursor<Vec<u8>>` 配合（`ReadWriteSeek`） | <https://docs.rs/fatfs/latest/fatfs/fn.format_volume.html> |

## 步骤 1：写失败测试

`crates/hzip-fmt-raw/tests/probe.rs`：

```rust
#[test]
fn empty_and_random_heads_are_not_raw() {
    assert_eq!(raw_probe(&[0u8; 2048]), None, "全零不得认（宁可打不开也不误判）");
    assert_eq!(raw_probe(b"this is just text, no table"), None);
}

#[test]
fn mbr_and_gpt_and_bare_fs_are_recognised() {
    let mut mbr = [0u8; 2048];
    mbr[510..512].copy_from_slice(&[0x55, 0xAA]);
    mbr[446 + 4] = 0x0C;                                   // 类型：FAT32 LBA
    mbr[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
    mbr[446 + 12..446 + 16].copy_from_slice(&8192u32.to_le_bytes());
    assert_eq!(raw_probe(&mbr), Some(Confidence::High));

    let mut gpt = [0u8; 2048];
    gpt[510..512].copy_from_slice(&[0x55, 0xAA]);          // 保护性 MBR
    gpt[446 + 4] = 0xEE;
    gpt[512..520].copy_from_slice(b"EFI PART");
    assert_eq!(raw_probe(&gpt), Some(Confidence::High));

    let mut ext = [0u8; 2048];
    ext[1024..1028].copy_from_slice(&8192u32.to_le_bytes()); // s_inodes_count ≠ 0
    ext[1048..1052].copy_from_slice(&2u32.to_le_bytes());    // s_log_block_size = 2 ⇒ 4 KiB
    ext[1080..1082].copy_from_slice(&[0x53, 0xEF]);          // s_magic = 0xEF53 @ 1024+56
    assert_eq!(raw_probe(&ext), Some(Confidence::High));
}

#[test]
fn head_shorter_than_needed_is_not_raw() {
    assert_eq!(raw_probe(&[0x55, 0xAA]), None, "头长不足 ⇒ 不认（探针不得越界）");
}
```

`crates/hzip-fs-ext4/tests/read_only.rs`（夹具来自 testkit）：

```rust
#[test]
fn lists_and_reads_the_whole_tree() {
    let img = testkit::vol::ext4_image(&[("/etc/fstab", b"LABEL=root / ext4 defaults 0 1\n"),
                                         ("/boot/BOOTX64.EFI", b"EFI-BYTES"),
                                         ("/empty.txt", b"")]);
    let dev = Arc::new(testkit::vol::MemDevice::from_bytes(img));   // 夹具有效性见 testkit::tests::vol_fixtures
    let fs = Ext4Driver::new().mount(dev, &FsOptions::default()).unwrap();
    let tree = fs.entries(&EntryLimit::default(), &CancellationToken::new()).unwrap();
    let names: Vec<_> = tree.entries.iter().map(|e| e.info.name.as_str().to_string()).collect();
    assert!(names.contains(&"etc".to_string()) && names.contains(&"etc/fstab".to_string()));
    assert!(names.iter().all(|n| !n.starts_with("./")), "名字必须已规范化");

    // 读：乱序 + 重复
    let fstab = tree.by_name("etc/fstab").unwrap();
    let mut buf = vec![0u8; 64];
    let n = fs.read_at(fstab.id, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"LABEL=root / ext4 defaults 0 1\n");
    assert_eq!(fs.size_of(fstab.id).unwrap(), 31);

    // 空文件与更深一层目录
    assert_eq!(fs.size_of(tree.by_name("empty.txt").unwrap().id).unwrap(), 0);
}

#[test]
fn special_files_are_not_listed_and_symlinks_carry_targets() {
    let img = testkit::vol::ext4_image_with(&[("dev", Dir), ("dev/null", CharDev), ("link", SymlinkTo("/etc/fstab"))]);
    let tree = ...;
    assert!(tree.by_name("dev/null").is_none(), "设备/管道/套接字不列出（避免误导）");
    assert!(tree.by_name("dev").is_some(), "目录本身要列出");
    let l = tree.by_name("link").unwrap();
    assert!(l.info.is_symlink);
    assert_eq!(l.info.symlink_target.as_deref(), Some(Path::new("/etc/fstab")));
}

#[test]
fn audit_reports_clean_image() {
    let fs = ...;
    assert!(fs.audit().unwrap().is_clean());
}
```

`crates/hzip-fmt-raw/tests/reader.rs`：

```rust
#[test]
fn bare_ext4_image_has_no_partition_layer() { /* 无分区表 ⇒ 顶层直接是 ext4 树，无 "1. …" 前缀行 */ }

#[test]
fn mbr_image_exposes_two_partitions() { /* 名字形如 "1. EFI System (FAT32)/"；未识别卷仍在列表里且不可编辑 */ }
```

## 步骤 2：实现

### 2.1 探针（`hzip-fmt-raw/src/probe.rs`）

```rust
/// raw 无魔数：只认**内容结构**，认不出就不认（不猜）。
/// 与 `FormatHandler::probe_head_bytes() = 2048` 配套（GPT 头在 512，ext4 超级块在 1024）。
pub fn raw_probe(head: &[u8]) -> Option<Confidence> {
    if head.len() < 512 { return None; }
    let mbr_sig = head[510..512] == [0x55, 0xAA];
    if mbr_sig && mbr_entries_plausible(&head[446..510]) { return Some(Confidence::High); }
    if head.len() >= 1024 && &head[512..520] == b"EFI PART" { return Some(Confidence::High); }
    if fs_magic_at_0(head) || ext_superblock_at(head) { return Some(Confidence::High); }
    None
}

/// 表项plausible：至少一项 type != 0 且 (start|len) != 0（不查盘长——探针只看头）
fn mbr_entries_plausible(t: &[u8]) -> bool;
/// FAT12/16/32（偏移 54/82 的 "FAT.."）、NTFS（偏移 3 的 "NTFS    "）、exFAT（"EXFAT   "）
fn fs_magic_at_0(head: &[u8]) -> bool;
/// ext2/3/4：超级块**固定**起于偏移 1024（与块大小无关），`s_magic`（0xEF53 LE = `53 EF`）
/// 在超级块内偏移 56 ⇒ **绝对 1080**；再核 `s_inodes_count != 0`（1024..1028）与
/// `s_log_block_size`（1048..1052，u32 且 ≤ 6 ⇒ 块大小 1 KiB..64 KiB）做强度复核。
///
/// ⚠ 别按「`53 EF` @1024」去认 ext4：1024 处是 `s_inodes_count`。
/// （实施时踩过：夹具自测把它写错，`cargo test` 立刻报 `left: [0, 32]`。）
fn ext_superblock_at(head: &[u8]) -> bool;
```

`handler.rs`：`RawHandler { id: "raw", caps: read+write+edit }` + `supports_in_place_edit() -> true`（T-IMG1 的 trait 方法，源是常规文件时 core 才走就地写）；`probe` 转发 `raw_probe`；`probe_head_bytes = 2048`；`open_reader` = `hzip_vol::reader::open(dev, RawVolume::new(dev), ...)`。

```rust
impl FormatHandler for RawHandler {
    fn id(&self) -> FormatId { FormatId::from("raw") }
    fn signature(&self) -> &'static Signature {
        static S: Signature = Signature { magic: b"", offset: 0, display: "raw disk image", extensions: &["raw", "img", "dd"] };
        &S
    }
    fn probe(&self, head: &[u8]) -> Option<Confidence> { raw_probe(head) }
    fn probe_head_bytes(&self) -> usize { 2048 }
    /* create_writer → Unsupported（不产出镜像）；open_editor → VolumeEditor；open_reader → VolumeReader */
}
```

### 2.2 `RawVolume`（`hzip-fmt-raw/src/volume.rs`）

```rust
/// raw = 整文件就是一个块设备。分区表/文件系统全靠下游探测。
pub struct RawVolume { dev: Arc<dyn BlockDevice>, table: Option<TableKind>, parts: Vec<PartitionView> }

impl Volume for RawVolume {
    fn geometry(&self) -> Geometry { Geometry { virtual_size: self.dev.size_bytes(), sector_size: 512, table: self.table, is_sparse: false, has_backing: false } }
    fn partitions(&self) -> VolResult<Vec<PartitionView>> { Ok(self.parts.clone()) }   // 空 = 无表
    fn open_fs(&self, part: Option<PartitionId>) -> VolResult<Option<Box<dyn FsVolume>>> {
        hzip_vol::registry::mount_best(&self.dev, part)      // 按 sniff 的 FsKind 挑驱动；不认识 ⇒ Ok(None)
    }
    fn caps(&self) -> VolCaps { VolCaps { writable: self.dev.is_writable(), in_place: true, read_only_reason: None } }
    fn flush(&mut self) -> VolResult<()> { self.dev.flush().map_err(Into::into) }
}
```

### 2.3 ext4 只读适配（`hzip-fs-ext4/src/volume.rs`）

映射表（一张就够，写侧 T-IMG5 复用）：

| `FsVolume` | `fs_ext4` 入口 |
|---|---|
| `entries` | 从 `lookup_path_bytes(b"/")`（root=2）起递归 `read_dir_ino(ino)`；跳过 `.`/`..`、跳过 INCOMPAT 未支持项；设备/管道/套接字**不列出**（计数进 `EntryTree.skipped`） |
| `read_at` | `Filesystem::read_ino(ino, offset, out)` |
| `size_of` | `Filesystem::stat_ino(ino).size` |
| 条目元数据 | `stat_ino` → `EntryInfo { size_raw, mtime, mode: Some(i_mode), is_symlink, symlink_target: read_link_ino }` |
| `audit` | `Filesystem::audit(max_dirs, max_entries)` |
| `apply`（本任务） | `Err(VolError::ReadOnly(ReadOnlyReason::UserSetting))` → T-IMG5 换成真实现 |

```rust
pub struct Ext4Driver;
impl FsDriver for Ext4Driver {
    fn id(&self) -> &'static str { "ext4" }
    fn supports(&self, kind: FsKind, head: &[u8]) -> bool {
        // 只认 s_magic @ 1024+56（0xEF53）；`53 EF`@1024 是常见误记，别写
        matches!(kind, FsKind::Ext2 | FsKind::Ext3 | FsKind::Ext4)
            && head.len() >= 1082
            && head[1080..1082] == [0x53, 0xEF]
    }
    fn mount(&self, dev: Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<Box<dyn FsVolume>> {
        let fs = fs_ext4::Filesystem::mount(dev)?;      // 只读设备 ⇒ 上游自动只读挂载；脏卷读侧会重放到内存
        Ok(Box::new(Ext4Volume { fs, inodes: RwLock<Vec<u32>>, caps: /* write: false（T-IMG5 打开） */ }))
    }
}

pub struct Ext4Volume { fs: fs_ext4::Filesystem, inodes: RwLock<Vec<u32>> /* EntryId → inode */, caps: FsCaps }
```

`EntriesIdMap`：`entries()` 建 `Vec<u32>`（EntryId 即下标），`read_at/size_of` 用它反查 inode；`apply`（T-IMG5）以 inode 为准做路径操作（ext4 的 `apply_*` 都是**路径版**，故 `Ext4Volume` 同时保留 `Vec<String>` 路径表）。

### 2.4 夹具（`hzip-format-testkit/src/vol/`，`vol` feature）

```rust
/// 内存块设备：可作 base，也可作差异对照（所有 vol 测试共用）
pub struct MemDevice { buf: Mutex<Vec<u8>> }
impl MemDevice {
    pub fn new(len: u64) -> Self;
    pub fn from_bytes(v: Vec<u8>) -> Self;
    pub fn to_bytes(&self) -> Vec<u8>;
}
impl fs_core::BlockRead for MemDevice { … }
impl fs_core::BlockDevice for MemDevice { … }

/// 512 B MBR：`(type, start_lba, len_lba)` 直接落表项 + 55AA
pub fn mbr(early: &[(u8, u32, u32)]) -> [u8; 512];
/// GPT：保护性 MBR + 主头（CRC32 正确）+ 条目数组 + 备份头
pub fn gpt(parts: &[(PartitionTypeId, u64, u64)]) -> Vec<u8>;

/// ext4 小镜像（mkfs 自造，含给定文件树；支持制造脏卷 `s_state` 以便测只读理由）
pub fn ext4_image(files: &[(&str, &[u8])]) -> Vec<u8>;
/// FAT32 小镜像（fatfs::format_volume 自造）——T-IMG12 用，本任务先落最小实现
pub fn fat32_image(files: &[(&str, &[u8])]) -> Vec<u8>;

/// 组装：`mbr+分区+文件系统` 与 `gpt+分区+文件系统`
pub fn raw_with_mbr(fs: FsKind, files: &[(&str, &[u8])]) -> Vec<u8>;
```

## 步骤 3：跑测试

```bash
cargo test -p hzip-format-testkit --features vol
cargo test -p hzip-fmt-raw -p hzip-fs-ext4
```

### 步骤 4：提交

```bash
git add crates/hzip-fmt-raw crates/hzip-fs-ext4 crates/hzip-format-testkit Cargo.toml
git commit -m "feat(vol): raw 容器探针 + ext4 只读驱动 + 内存镜像夹具"
```

## 验收

- [ ] `raw_probe` 四类正例 + 三类反例（全零 / 纯文本 / 头不足）全绿；**无 panic**
- [ ] ext4 只读：全树列出（含中文名）、乱序/重复读、空文件、符号链接目标、特殊文件不列出
- [ ] `audit()` 对干净镜像返回 clean；`EntryLimit` 截断与取消各有专测
- [ ] 夹具**零外部工具依赖**（`cargo test` 在干净机器上直接过）
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`

## 风险 / 注意

| 点 | 处置 |
|---|---|
| `raw_probe` 误判（把普通文件当镜像） | 只认**结构证据**；全零/纯文本/短头一律不认；扩展名回退（Low）仍由 `hzip-format` 统一处理 |
| `MemDevice` 与生产 `FileDevice` 行为差异（越界/短读语义） | 测试里对越界读写各写一条断言，与 `fs_core` 的返回口径对齐（不吞错） |
| ext4 遍历大镜像的时延 | `entries()` 必带 `EntryLimit` + 每 N 条检查取消；`Filesystem::mount_with_cache` 可调缓存块数（T-IMG13 定档） |
| 夹具体积 | 小镜像 < 8 MiB；`vol` feature 默认关，不拖累其它 fmt crate 的测试构建 |

## §落地记录（2026-10-16）

**已完成**：raw 容器（`crates/hzip-fmt-raw`，11 条端到端用例含**真实 ext4 驱动**）+ ext4 只读侧（见 T-IMG5）+ 夹具（`hzip-format-testkit/src/vol/**`，11 条自证含**真挂载**）+ core 注册与扩展名表（T-IMG4）。

**实测踩坑（已修）**：
1. **ext4 魔数偏移**：`s_magic` 在超级块内偏移 56 ⇒ 绝对 **1080**（1024 处是 `s_inodes_count`）。原先写错成 1024，夹具自测当场炸；探针/`supports()`/文档三处已统一。
2. **脏卷夹具**：改 `s_state` 必须**重算超级块校验和**（卷开着 `metadata_csum`；不重算则上游 `BadChecksum` 根本挂不上，脏卷策略压根测不到）。夹具已修，自证测试升级为「脏卷也必须可挂载」。
3. **投影 id 必须重编号**：文件系统的本地 id 与分区行 id 会撞车（都从 0 起），直接沿用会让 `read_entry(info.id)` 读到**目录**并返回空流。已改为下标即 id，并加「`info.id == 下标`」不变量断言。
