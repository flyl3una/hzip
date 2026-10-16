# T-IMG12：`hzip-fs-fat`（FAT12/16/32 读写，ext4/NTFS 之后）

| 项目 | 内容 |
|---|---|
| 阶段 | P4（用户裁定 ③ 的最后一块；ESP 分区/U 盘镜像的主力格式） |
| 依赖 | T-IMG10（框架就位）、T-IMG11（同一套 `FsDriver` 注册方式） |
| 状态 | 未开工 |
| 目标 | FAT12/16/32 可列、可读、可增删改；**长名（LFN）与 8.3 都支持**；无日志带来的中断风险如实标注 |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 新 | `crates/hzip-fs-fat/src/{lib.rs,driver.rs,volume.rs,io.rs}` | `FatDriver`/`FatVolume`；`io.rs` = `Arc<dyn BlockDevice>` → `fatfs::ReadWriteSeek` 适配器 |
| 改 | `crates/hzip-vol/src/error.rs` | 新键：`vol.err.too-large`（FAT32 单文件 4 GiB 上限）、`vol.warn.no-journal` |
| 改 | `crates/hzip-format-testkit/src/vol/fat.rs`（`vol` feature） | `fat32_image`（已有最小实现）+ `fat16_image` + `fat12_image` + 长名/CJK 目录 |
| 新 | `crates/hzip-fs-fat/tests/{read.rs,write.rs,refuse.rs,conformance.rs}` | 三组 + `TC-CONF-001..010`（testkit `run_conformance`，夹具有 FAT12/16/32 三档） |
| 改 | `crates/hzip-core/Cargo.toml`（`volume-formats`） | 注册 `hzip_fs_fat`；`hzip-fmt-raw` 的 `open_fs` 即可命中 ESP 分区 |

## 步骤 0：前置核实

| 待核 | 依据 |
|---|---|
| `fatfs::FileSystem::new(disk, FsOptions)` 的参数与 `FsOptions` 字段（`update_accessed_date`/`time_provider`） | <https://docs.rs/fatfs/latest/fatfs/struct.FsOptions.html> |
| `DirEntry` 形状（`FileName`/`is_dir`/`metadata().len()/.modified()`） | <https://docs.rs/fatfs/latest/fatfs/struct.DirEntry.html> |
| `File::{read,write,seek,truncate,set_len}` 与 `rename` 的跨目录语义 | <https://docs.rs/fatfs/latest/fatfs/struct.File.html>、`struct.Dir.html` |
| `fatfs::ReadWriteSeek` 的必需方法集 | <https://docs.rs/fatfs/latest/fatfs/trait.ReadWriteSeek.html> |

## 步骤 1：写失败测试

`tests/read.rs`：

```rust
#[test] fn long_and_short_names_both_visible()   { /* "BOOTX64.EFI"（8.3）与 "Some Long Name.txt"（LFN）都在树里 */ }
#[test] fn cjk_names_round_trip()                { /* LFN + UTF-16 → UTF-8 不丢条目 */ }
#[test] fn nested_dirs_and_empty_files()         { /* 多层目录、空文件、0 字节目录项 */ }
#[test] fn modification_time_is_reported()       { /* FAT 时间精度 2 s：允许 ±2 s（写进断言注释） */ }
```

`tests/write.rs`：

```rust
#[test] fn add_replace_delete_rename_mkdir_round_trip_and_survive_remount() {
    /* 与 T-IMG5/T-IMG11 同形；FAT 无日志 ⇒ 额外断言：写后目录项与 FAT 链自洽（轻量自检） */
}

#[test] fn rename_across_directories()            { /* 跨目录改名（FAT 需「拷目录项 + 链搬迁」） */ }

#[test] fn works_on_an_efi_system_partition_shape() {
    /* ESP 形态：/EFI/BOOT/BOOTX64.EFI + /EFI/Microsoft/… ⇒ 增删改都成立（最常见的真实用法） */
}

#[test] fn fat32_four_gib_limit_is_reported_not_truncated() { /* Append 超过 4 GiB-1 ⇒ vol.err.too-large，且不写一半 */ }
```

`tests/refuse.rs`：

```rust
#[test] fn corrupt_bpb_is_an_error_not_a_panic()      { /* 位翻转头扇区若干处 ⇒ Err，无 panic */ }
#[test] fn read_only_device_yields_read_only_caps()    { /* ReadOnlyDevice ⇒ caps.write=false + DeviceReadOnly */ }
#[test] fn fat12_and_fat16_are_supported()             { /* 三种 FAT 宽度各一条（12 位表项是最易错的） */ }
```

## 步骤 2：实现

### 2.1 适配器（`io.rs`）

```rust
/// `fatfs` 只认 `Read + Write + Seek`；我们把块设备包成它要的形状。
/// 注意：fatfs 假设它是**独占**的、并可能对同一区域重复读写（FAT 表/目录项）。
pub struct BlockDeviceSeek { dev: Arc<dyn BlockDevice>, pos: u64 }
impl fatfs::ReadWriteSeek for BlockDeviceSeek {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;      // dev.read_at(pos, …) + pos 推进
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize>;         // dev.write_at(pos, …)
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64>;
    fn flush(&mut self) -> std::io::Result<()>;                        // dev.flush()
    // 若 trait 有 `set_len`/`truncate` 之类：一律 `Err(Unsupported)`（卷容量不可变）
}
```

### 2.2 `FatVolume`

| `FsVolume` | `fatfs` |
|---|---|
| `entries` | `root_dir()` 递归 `read_dir()`；`DirEntry::is_dir()`/`metadata().len()`/`.modified()` |
| `read_at`/`size_of` | `open_file(path)` → `seek` + `read`；尺寸用 `metadata().len()` |
| `apply(Replace)` | `open_file(path).truncate()?` 后写（或 `OpenOptions::new().write(true).create(true).truncate(true)`） |
| `apply(Delete)` | `Root/Dir::remove`；目录 ⇒ 空目录可删（非空 ⇒ `vol.err.dir_not_empty`） |
| `apply(Append)` | 逐级 `create_dir` + `open_file` 写 |
| `apply(Rename)` | `Dir::rename`（跨目录同 API）；LFN 冲突按 `FsOptions` 口径（避免覆盖） |
| `apply(MakeDir)` | `create_dir` |
| `audit` | FAT 无 fsck：返回 `AuditSummary::skipped("vol.warn.no-journal")`，但**额外**做一次轻量自检（FAT 链抽样：读根目录 + 一层子目录的簇链是否可达） |

`caps`：

```rust
FsCaps { write: true, delete: true, rename: true, mkdir: true,
         symlink: false, times: true /* 仅 mtime，2 s 精度 */, mode: false,
         name_max: 255 /* LFN */, read_only_reason: None }
```

### 2.3 无日志的如实标注

FAT 的目录项更新、FAT 表更新、数据簇写入之间**没有日志**：中断可能留下「目录项指向未写 FAT 链」这类不一致。处置（不改语义、只把风险说清）：

- 默认模式 B（覆盖层暂存 + 提交回放）⇒ **提交前原档不变**，用户可在提交前取消；
- 提交期进度窗提示「正在写回镜像，请勿中断」；
- 文案键 `vol.warn.no-journal`（能力矩阵工具提示里出现一次）。

## 步骤 3：跑测试与验收

```bash
cargo test -p hzip-fs-fat -p hzip-format-testkit --features vol
cargo test -p hzip-core --test volume          # ESP 形态的端到端回归
```

### 步骤 4：提交

```bash
git add crates/hzip-fs-fat crates/hzip-format-testkit crates/hzip-vol/src/error.rs
git commit -m "feat(vol): FAT12/16/32 读写驱动（LFN + ESP 形态）"
```

## 验收

- [ ] 三种 FAT 宽度（12/16/32）都能列目录、读、写
- [ ] LFN 与 8.3 并存；CJK 名往返不丢条目
- [ ] 五类变更 + 跨目录改名成立，**重挂**后仍成立
- [ ] ESP 形态（`/EFI/BOOT/BOOTX64.EFI`）端到端可改
- [ ] FAT32 4 GiB 上限：报 `vol.err.too-large`，**不截断、不半写**
- [ ] 位翻转/坏 BPB：报错不 panic；只读设备 ⇒ 只读 + 理由
- [ ] `vol.warn.no-journal` 在能力矩阵/提示里可见（一次即可）

## 风险 / 注意

| 点 | 处置 |
|---|---|
| fatfs 的时间精度（2 s）与「访问日期更新」行为 | 断言用 ±2 s；`FsOptions.update_accessed_date(false)` 避免读操作改写镜像（**读不能改档案**——这条与 hzip 既有红线一致） |
| FAT12 的 12 位表项实现易错 | 上游负责；我们用三种宽度的夹具各跑一遍往返 |
| 超大 U 盘镜像（FAT32 + 数百 GB） | 列表按条目上限截断（T-IMG13）；单文件 >4 GiB 明确拒绝 |
| 与 exFAT 的混淆 | exFAT 是**另一个** FS（`FsKind::ExFat`），本期不接驱动 ⇒ 分区行显示「未知卷」+ 理由键 `vol.readonly.fs-unknown`（**不要**用 FAT 驱动去读 exFAT） |

## §落地记录（2026-10-16）

**已完成**（子代理交付，**35 条**：lib 2 / conformance 10 / read 8 / refuse 6 / write 9；clippy 干净）：`FatDriver`（`supports` = `FsKind::Fat` + **独立的第二份证据**：BPB 几何自洽 + 簇数与类型标签互证，且**显式拒绝** EXFAT/NTFS）+ `FatVolume` + `io.rs`（`Arc<dyn BlockDevice>` → `fatfs::ReadWriteSeek`）。

**关键设计（值得记）**：`fatfs::FileSystem` **不是 `Send`**（内嵌 `&'static dyn TimeProvider`），而 `FsVolume` 要求 `Send + Sync` ⇒ 未用 `unsafe` 假装，而是「**文件系统句柄只在一次调用内存在**」（挂载→用→卸载），跨调用只留设备句柄与 `EntryId→路径` 表（`Mutex<Vec<Slot>>`，删除打墓碑/改名改前缀 ⇒ **EntryId 跨变更稳定**）。

**偏离**：FAT12/16 夹具放本 crate `tests/common/`（testkit 只有 `fat32_image`）；`vol.err.too-large` 等键经 `Unsupported(&'static str)` 承载（键字符串与冻结值一致，UI 按键查值不受影响）；读到 4 GiB 上限时**写前拒绝**（`check_file_size`），不半写。
