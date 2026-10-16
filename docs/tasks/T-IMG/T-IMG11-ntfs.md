# T-IMG11：`hzip-fs-ntfs`（NTFS 读写，ext4 测过之后）

| 项目 | 内容 |
|---|---|
| 阶段 | P4（用户裁定 ③：ext4 测过再补 NTFS） |
| 依赖 | T-IMG10（框架、界面、文案单源化就位） |
| 状态 | 未开工 |
| 目标 | Windows 虚拟机磁盘（绝大多数是 NTFS）可列、可读、可增删改；**边界与拒写口径如实**（日志/休眠/目录规模），不越界硬写 |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 新 | `crates/hzip-fs-ntfs/src/{lib.rs,driver.rs,volume.rs,names.rs}` | `NtfsDriver: FsDriver` + `NtfsVolume: FsVolume`（走上游 **Rust facade**，不用 C ABI） |
| 改 | `crates/hzip-vol/src/error.rs` | 新理由键：`vol.readonly.hibernated`、`vol.unsupported.dir-too-large`、`vol.unsupported.compressed` |
| 改 | `crates/hzip-format-testkit/src/vol/ntfs.rs`（`vol` feature） | NTFS 夹具（`mkfs` 或手写最小卷）+ dirty 卷 + 休眠卷标记 |
| 新 | `crates/hzip-fs-ntfs/tests/{read.rs,write.rs,refuse.rs,conformance.rs}` | 三组 + `TC-CONF-001..010`（testkit `run_conformance`，夹具 = NTFS 小卷） |
| 改 | `crates/hzip-core/Cargo.toml`（`volume-formats` 组合） | 注册 `hzip_fs_ntfs` |

## 步骤 0：前置核实

| 待核 | 依据 |
|---|---|
| `facade::Filesystem::mount_rw` 与 `mount_rw_replaying_log` 的差别与错误类型 | <https://docs.rs/rust-fs-ntfs/latest/fs_ntfs/facade/struct.Filesystem.html> |
| `facade::DirEntry` 字段（名字/类型/大小/时间） | <https://docs.rs/rust-fs-ntfs/latest/fs_ntfs/facade/struct.DirEntry.html> |
| facade 是「无状态、每次重新打开镜像」——对**上层 `Arc<dyn BlockDevice>`（可能是覆盖层）**是否适用 | 若 facade 只吃文件路径 ⇒ 必须**先物化**到临时文件再用 facade（见 §2.4 决策） |

## 步骤 1：写失败测试

`tests/read.rs`：

```rust
#[test] fn lists_and_reads_a_fresh_volume() { /* 夹具 mkfs 后建 /dir/file.txt ⇒ 树/内容/大小/时间正确 */ }
#[test] fn unicode_names_round_trip()      { /* 中文/emoji 名：UTF-16 → UTF-8 不丢条目、名字逐字符一致 */ }
#[test] fn resident_and_nonresident_files() { /* 小（常驻）与大（非驻留、跨簇）各一条 */ }
#[test] fn deep_directory_tree()           { /* 8 层目录；`entries()` 递归深度与条目上限正常 */ }
```

`tests/write.rs`：

```rust
#[test] fn add_replace_delete_rename_mkdir_round_trip_and_survive_remount() {
    /* 与 T-IMG5 的 ext4 用例同形：五类变更 → 写后 `audit()` → **重挂**再验 */
}

#[test] fn rename_preserves_content_and_times() { /* rename 后内容/basic info 时间戳合理（上游口径） */ }

#[test] fn full_volume_gives_readable_reason() { /* 空间不足 ⇒ vol.err.no_space，不是 panic */ }
```

`tests/refuse.rs`：

```rust
#[test] fn hibernated_volume_is_read_only()          { /* 夹具置休眠位 ⇒ 只读 + Hibernated；写 ⇒ 报错 */ }
#[test] fn log_holding_work_requires_explicit_replay() { /* $LogFile 含事务 ⇒ 默认拒写（JournalPending）；允许后 mount_rw_replaying_log 成功 */ }
#[test] fn dirty_flag_is_reported()                  { /* is_dirty ⇒ 只读理由/提示语正确 */ }
#[test] fn directory_beyond_index_block_limit_is_refused() { /* 造一个大目录（>64 索引块）⇒ 写判为 vol.unsupported.dir-too-large（不半途写坏） */ }
#[test] fn compressed_or_windows_filtered_files_are_read_only_entries() { /* 上游拒读/拒写 ⇒ 该条目只读 + 理由，不整卷失败 */ }
```

## 步骤 2：实现

### 2.1 映射（`NtfsVolume`）

| `FsVolume` | 上游 facade |
|---|---|
| `entries` | `read_dir(path)` 递归；`FileType` → `is_dir`；尺寸取 `stat(path)` |
| `read_at` / `size_of` | `read_file(path, offset, buf)` / `stat(path)` |
| `apply(Replace)` | `write_file_contents(path, data)`（不存在 ⇒ `create_file(parent, basename)` 后写） |
| `apply(Delete)` | 目录 ⇒ `rmdir`；文件 ⇒ `unlink` |
| `apply(Append)` | `create_file(parent, basename)` + `write_file_contents`；父目录缺失 ⇒ 逐级 `mkdir` |
| `apply(Rename)` | `rename(old_path, new_basename)`（**只换 basename**；跨目录改名 ⇒ `link` + `unlink` 组合，落地时按 facade 能力择一并在测试里钉死） |
| `apply(MakeDir)` | `mkdir(parent, basename)` |
| `audit` | 上游 `fsck` 子集（facade 若未暴露 ⇒ 用 `volume_stats()` 做轻量自检 + 记 `AuditSummary::skipped()`） |

### 2.2 名字（`names.rs`）

```rust
/// NTFS 名字是 UTF-16。hzip 的 `EntryName` 是 UTF-8 字符串：
/// 用 `String::from_utf16_lossy`（**不丢条目**；替换字符只出现在非法代理对上）
/// 并把异常名计数进 `EntryTree.warnings`，供 UI 提示。
pub fn utf16_to_entry_name(raw: &[u16]) -> String;
/// 反向：写侧把 UTF-8 名字转 UTF-16（非法字符按 U+FFFD）
pub fn entry_name_to_utf16(name: &str) -> Vec<u16>;
```

### 2.3 拒写判定序

```text
① 休眠卷（Windows 快速启动残留） ⇒ 只读 + Hibernated（**绝不清标志**）
② $LogFile 含事务 ⇒ 默认只读 + JournalPending；用户显式确认 ⇒ mount_rw_replaying_log
③ dirty 标志（且日志空） ⇒ 可写 + 提示（上游允许；我们沿用上游口径）
④ 目录 > 64 索引块 ⇒ 该目录下的写操作拒绝（vol.unsupported.dir-too-large）
⑤ WOF 压缩/加密文件：读拒绝该条目（条目仍列出、标记只读），写拒绝
```

### 2.4 关键决策：facade 与块设备层的关系

facade 是**基于路径、无状态**的（每次操作重新打开镜像）。我们的设备链末端可能是**覆盖层**（模式 B）而不是真文件——两条解法：

- **A（推荐，先做）**：`Chain` 在 `Overlay` 模式下把「源」物化/绑定到一个**临时镜像文件**（`FileDevice::open_rw(tmp)` 直接当末端，不再叠覆盖层），提交时把临时文件的**脏块**回放到原档。语义与覆盖层等价（提交前原档不变），且 facade 拿到的永远是真实路径 ⇒ 零上游改造。
- **B（后做）**：若上游提供 `mount_with_callback`/`FsCoreDevice` 变体（C ABI 可见 `fs_ntfs_mount_with_fs_core_device`），改走设备直连 + 覆盖层。

> 选 A 的理由：NTFS 走 facade 时**每次操作重开镜像**，指向真文件最省事；代价是需要一份完整镜像的临时副本（与 `Sink::File` 路径同量级）。ext4 走设备直连（T-IMG5）不受影响。此决策写进 `NtfsVolume` 的 crate 文档。

## 步骤 3：跑测试与验收

```bash
cargo test -p hzip-fs-ntfs
# Windows 开发机（可选，不进 CI）：chkdsk /scan 复核（上游的做法，我们沿用）
```

### 步骤 4：提交

```bash
git add crates/hzip-fs-ntfs crates/hzip-format-testkit crates/hzip-vol/src/error.rs
git commit -m "feat(vol): NTFS 读写驱动（facade 直连）+ 边界拒写口径"
```

## 验收

- [ ] 五类变更往返 + **重挂**复核；Unicode 名不丢条目
- [ ] 休眠卷/日志含事务/脏标志三态行为与文案键逐条对上
- [ ] 目录 > 64 索引块：**写前**拒绝（不半写）
- [ ] WOF 压缩/加密文件：条目可见但只读，写被拒且有理由
- [ ] 大目录（万级条目）列表在条目上限内可完成、可取消（时延记入 T-IMG13 基线）

## 风险 / 注意

| 点 | 处置 |
|---|---|
| 上游自陈「目录最多 64 索引块」 | 明确拒绝而非尝试；文案指出路（「请先在 Windows 里整理该目录」） |
| facade 的无状态重开 ⇒ 每次操作 O(重解析) | 批量变更在**同一批**里合并（`ChangeSet` 顺序执行已是批语义）；必要时改用 C ABI 的 `mount_rw_with_fs_core_device`（B 方案） |
| 临时副本占盘（A 方案） | 走 `QuotaKind` 临时盘空间检查；镜像大小提示进进度窗 |
| NTFS 写路径的成熟度低于读路径 | 提交前 `fsck` 子集/`volume_stats` 自检；开发机以 `chkdsk` 复核；发现问题**先退只读**（能力位可单点关闭） |

## §落地记录（2026-10-16）

**已完成**（子代理交付，**40 条**：lib 7 / conformance 10 / read 6 / refuse 9 / write 8；clippy+fmt 干净）：`NtfsDriver`/`NtfsVolume` + `names.rs`（UTF-16↔UTF-8、非法代理对、长度上限）+ 夹具四态（干净/dirty/日志待重放/休眠，testkit `vol/ntfs.rs`，含自检）。

**实现要点**：设备物化到 **`hzip-spool-` 前缀**的临时镜像（孤儿回收可见）；`apply` 只改临时镜像，`flush()` 以 1 MiB 粒度与设备**逐块比对只写不同块**，`Drop` 不回放 ⇒ **提交前设备字节不变**（取消即丢）。

**偏离（均有依据）**：
1. 走上游 `read::`/`write::`/`fsck::` 的公开路径函数而非 `facade::Filesystem`：facade 的 `Error` 只有 `String`（丢 `Kind`），ENOSPC/ENOTEMPTY 只能猜消息文本，而 UI 要**稳定键**。
2. **dirty ⇒ 只读 + 理由**（不是计划 §2.3 ③ 的「可写 + 提示」）：实测上游 `prepare_rw_mount` 对 dirty 卷直接拒（dirty 意味着要先做一次全卷检查），沿用上游口径、不绕过。
3. 目录索引容量判据用「`$Bitmap:$I30` 无空位」（= 上游自己的拒绝条件），而非硬编码 `> 64` 块；删条目腾出的空位仍可用；`INDEX_BLOCKS_MAX=64` 作为公开常量记录上限。
4. 异常名计数改由 `NtfsVolume::lossy_name_count()` 暴露（`EntryTree` 无 `warnings` 字段，且本轮不改 `hzip-vol`）⇒ 进 `audit()` findings；「不丢条目」红线照旧。
5. 计划里「显式允许后 `mount_rw_replaying_log` 成功」半句**无法用程序化夹具满足**（上游没有造「可完整重放日志」的 API）⇒ 如实断言等价行为：允许重放后日志解析不出来 ⇒ 降级只读 + 同一理由键 + 设备字节未变。
