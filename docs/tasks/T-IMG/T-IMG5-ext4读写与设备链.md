# T-IMG5：ext4 写侧 + 设备链（模式 A / B / 只读）

| 项目 | 内容 |
|---|---|
| 阶段 | P2 读写闭环（**里程碑 M2**：卷内文件可增/删/改/改名/建目录） |
| 依赖 | T-IMG4 |
| 状态 | 未开工 |
| 目标 | 把 ext4 的写路径接上（JBD2 日志化由上游负责），并把「写模式开关」的三种设备链末端在**真实镜像上**跑通 |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 改 | `crates/hzip-vol/src/device.rs` | `Chain::{open,commit,reset}` 补全：`ReadOnly`/`InPlace`/`Overlay` 三态 + LRU 缓存装配 + 打开失败降级 |
| 改 | `crates/hzip-vol/src/error.rs` | `VolError::{ReadOnly,Unsupported,Quota}` 与 `fs_ext4`/`fs_core` 错误的映射（含 Inode 无效、目录非空、权限不足三类的可读文案键） |
| 改 | `crates/hzip-fs-ext4/src/volume.rs` | `Ext4Volume::apply`：`Change` → `Filesystem::apply_*` 全量映射；`caps.write = true`；`apply_utimens` 保时间戳 |
| 改 | `crates/hzip-fs-ext4/src/driver.rs` | 可写挂载策略：脏卷重放（`mount_recovering` / `replay_journal_if_dirty`）与「拒写 + 理由」两分支 |
| 改 | `crates/hzip-fmt-raw/src/handler.rs` | `caps.edit = true`、`supports_in_place_edit() = true`、`open_editor` 返回 `VolumeEditor` |
| 新 | `crates/hzip-fs-ext4/tests/write.rs` | 五类变更 × 写后重挂 × audit |
| 新 | `crates/hzip-vol/tests/device_chain.rs` | 三态设备链与降级 |

## 步骤 0：前置核实

| 待核 | 依据（T-IMG3 步骤 0 已核一半） |
|---|---|
| `Filesystem::mount_recovering` 的「backed-up device」含义（是否需要我们提供备份区） | <https://docs.rs/rust-fs-ext4/latest/fs_ext4/struct.Filesystem.html> |
| `apply_rename` 对目录子树的行为 | 同上（`src`/`dst` 全路径版） |
| `apply_create` 的 mode 语义（低 12 位）与默认建议值 | 同上 |

## 步骤 1：写失败测试

`crates/hzip-fs-ext4/tests/write.rs`：

```rust
fn mount_rw(img: &Arc<MemDevice>) -> Box<dyn FsVolume> {
    Ext4Driver::new().mount(img.clone(), &FsOptions { writable: true, ..Default::default() }).unwrap()
}

#[test]
fn add_replace_delete_rename_mkdir_round_trip_and_survive_remount() {
    let dev = Arc::new(MemDevice::from_bytes(testkit::vol::ext4_image(&[("/a.txt", b"old")])));
    {
        let mut fs = mount_rw(&dev);
        // ① Append：新文件
        fs.apply(&Change::Append { src: tmp_path("new.txt", b"NEW"), name: "dir/new.txt".try_into().unwrap() }, None, &cancel()).unwrap();
        // ② Replace：覆盖既有
        fs.apply(&Change::Replace { entry: id_of(&fs, "a.txt"), src: tmp_path("a.txt", b"new-content") }, None, &cancel()).unwrap();
        // ③ MakeDir
        fs.apply(&Change::MakeDir { name: "empty-dir".try_into().unwrap() }, None, &cancel()).unwrap();
        // ④ Rename（文件）
        fs.apply(&Change::Rename { from: "a.txt".try_into().unwrap(), to: "b.txt".try_into().unwrap() }, None, &cancel()).unwrap();
        // ⑤ Delete
        fs.apply(&Change::Delete(id_of(&fs, "dir/new.txt")), None, &cancel()).unwrap();
        assert!(fs.audit().unwrap().is_clean(), "写后自检必须干净");
        fs.flush().unwrap();
    }
    // **重挂**后才算真写完（这是本任务最硬的断言）
    let fs = Ext4Driver::new().mount(dev.clone(), &FsOptions::default()).unwrap();
    let tree = fs.entries(&EntryLimit::default(), &cancel()).unwrap();
    assert_eq!(read_all(&fs, &tree, "b.txt"), b"new-content");
    assert!(tree.by_name("dir").is_some(), "空了的目录仍在（Append 时自动建的父目录）");
    assert!(tree.by_name("empty-dir").is_some());
    assert!(tree.by_name("a.txt").is_none() && tree.by_name("dir/new.txt").is_none());
}

#[test]
fn replace_preserves_local_mtime() {
    /* 本地文件 mtime = 固定值 ⇒ 写后 stat_ino.mtime 一致（apply_utimens） */
}

#[test]
fn rename_directory_moves_whole_subtree() { /* from 为目录 ⇒ 子树整体改前缀（core 契约要求） */ }

#[test]
fn dirty_volume_is_refused_without_replay_and_writes_with_replay() {
    let dirty = testkit::vol::ext4_image_dirty(&[("/a.txt", b"x")]);
    let dev = Arc::new(MemDevice::from_bytes(dirty.clone()));
    let fs = Ext4Driver::new().mount(dev.clone(), &FsOptions { writable: true, replay_journal: false, ..Default::default() }).unwrap();
    assert!(!fs.caps().write);
    assert_eq!(fs.caps().read_only_reason, Some(ReadOnlyReason::JournalPending));

    let dev2 = Arc::new(MemDevice::from_bytes(dirty));
    let mut fs2 = Ext4Driver::new().mount(dev2.clone(), &FsOptions { writable: true, replay_journal: true, ..Default::default() }).unwrap();
    assert!(fs2.caps().write, "显式允许重放后可写");
    fs2.apply(&Change::MakeDir { name: "after-replay".try_into().unwrap() }, None, &cancel()).unwrap();
    fs2.flush().unwrap();
}

#[test]
fn full_disk_gives_a_readable_reason_not_a_panic() {
    /* 夹具把 inode/块位图填满 ⇒ apply(Append) ⇒ VolError::Quota/Unsupported("vol.err.no-space")，不是 panic */
}
```

`crates/hzip-vol/tests/device_chain.rs`：

```rust
#[test] fn overlay_mode_leaves_source_untouched_until_commit() { /* 写 → 源字节不变 → commit → 源变 */ }
#[test] fn in_place_mode_writes_immediately() { /* 写 → 源立即可见（`is_writable` 语义） */ }
#[test] fn read_only_mode_rejects_writes_and_reports_reason() { /* write_at ⇒ 只读错误；caps.read_only_reason = UserSetting */ }
#[test] fn in_place_falls_back_to_overlay_when_file_is_not_writable() {
    /* 造只读文件（chmod 0444 / 或 i 位）⇒ Chain::open(InPlace) 得到 Overlay 链 + 理由 DeviceReadOnly */
}
```

## 步骤 2：实现

### 2.1 `Change` → `fs_ext4` 映射（`crates/hzip-fs-ext4/src/volume.rs`）

| `Change` 变体 | ext4 调用 | 备注 |
|---|---|---|
| `Replace { entry, src }` | 存在：`apply_replace_file_content(path, bytes)`；不存在：`apply_create(path, 0o644)` + 同前（`tracing::warn!`） | 之后 `apply_utimens(path, atime, mtime)`（取本地文件 mtime，保真） |
| `Delete(id)` | `apply_rmdir(path)`（目录）/ `apply_unlink(path)`（文件·链接） | 目录非空 ⇒ `apply_rmdir` 报错 ⇒ 映射为 `vol.err.dir-not-empty`（**不递归删**：镜像里的删除应当是显式的） |
| `Append { src, name }` | `apply_create(name, 0o644)` + `apply_replace_file_content(name, bytes)`；父目录不存在 ⇒ 逐级 `apply_mkdir`；`name` 以 `/` 结尾 ⇒ 只 `apply_mkdir` | 建父目录是既有档案语义（zip/7z 自动建隐式目录） |
| `Rename { from, to }` | `apply_rename(from, to, false)` | 目录 ⇒ 上游整子树；`to` 的父目录段以 `from` 的父目录为准（core 契约「不含父目录段」）——**用一条 GUI 改名用例钉死** |
| `MakeDir { name }` | `apply_mkdir(name, 0o755)` | 父目录不存在 ⇒ 逐级建 |

```rust
impl FsVolume for Ext4Volume {
    fn apply(&mut self, change: &Change, local: Option<&Path>, cancel: &CancellationToken) -> VolResult<()> {
        if !self.caps.write { return Err(VolError::ReadOnly(self.caps.read_only_reason.unwrap_or(ReadOnlyReason::UserSetting))); }
        check_cancel(cancel)?;
        match change { /* 上表逐条；每条前 `check_cancel`，每条后 `debug!(…)` */ }
        Ok(())
    }
    fn audit(&self) -> VolResult<AuditSummary> { Ok(AuditSummary::from(self.fs.audit(AUDIT_MAX_DIRS, AUDIT_MAX_ENTRIES)?)) }
}
```

### 2.2 可写挂载策略（`crates/hzip-fs-ext4/src/driver.rs`）

```rust
pub struct FsOptions {
    pub writable: bool,
    /// 卷内有未重放日志时，是否允许「先重放再写」（默认 false ⇒ 只读 + 理由）
    pub replay_journal: bool,
    pub cache_blocks: usize,
}

fn mount(&self, dev: Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<Box<dyn FsVolume>> {
    let dirty = probe_needs_recovery(&dev)?;                       // 读超级块 s_state / 日志超级块
    if opts.writable && !dirty {
        let fs = fs_ext4::Filesystem::mount_recovering(dev.clone())?   // 带检查的日志恢复挂载
        return Ok(Box::new(Ext4Volume::new(fs, /* write */ true, /* reason */ None)));
    }
    if opts.writable && dirty && opts.replay_journal {
        let mut fs = fs_ext4::Filesystem::mount(dev.clone())?;
        fs.replay_journal_if_dirty()?;                              // 显式重放（用户已同意）
        return Ok(Box::new(Ext4Volume::new(fs, true, None)));
    }
    let fs = fs_ext4::Filesystem::mount_lazy(dev.clone())?;         // 只读：不做挂载期重放
    Ok(Box::new(Ext4Volume::new(fs, false,
        Some(if dirty { ReadOnlyReason::JournalPending } else { ReadOnlyReason::UserSetting }))))
}
```

> 只读挂载用 `mount_lazy` 的理由：读侧不应因重放而**改写设备**；`mount`/`mount_recovering` 在可写设备上会重放，故只在「允许写」分支使用。

> **落地口径（2026-10-16，子代理实测后修正）**：最终**一律 `mount_lazy`**，重放改为**显式**调用
> `replay_journal_if_dirty()`（必要时 `recover_orphans()`）：
> - 计划里的 `mount_recovering` 在**无内部日志**的卷上直接 `Unsupported`（我们格式化的夹具卷就是无日志的），会把「能读的卷」误判成打不开；
> - 计划里的可写分支 `mount` 会在设备可写时**顺手重放脏日志**——等于背着用户改档案，与「默认不静默改盘」的口径冲突；
> - 故：**读**永不重放；**写**且用户显式同意时才重放，且重放是独立的一步（进日志与提示）。

### 2.3 设备链补全（`crates/hzip-vol/src/device.rs`）

```rust
pub fn open(src: Source, mode: WriteMode, opts: &VolOptions) -> VolResult<Chain> {
    let (path, tmp) = resolve_path(src)?;                  // File 原样；Stream/Volumes ⇒ 物化（T-IMG8）
    let mut reason = None;
    let device: Arc<dyn BlockDevice> = match mode {
        WriteMode::ReadOnly => Arc::new(ReadOnlyDevice::new(FileDevice::open(&path)?)),
        WriteMode::InPlace => match FileDevice::open_rw(&path) {
            Ok(d)  => Arc::new(d),
            // 打不开 r/w（只读介质、权限、被占用）⇒ 降级 Overlay 并**带理由**（不静默）
            Err(_) => { reason = Some(ReadOnlyReason::DeviceReadOnly); overlay_chain(&path)? }
        },
        WriteMode::Overlay => overlay_chain(&path)?,
    };
    let device = if opts.cache_blocks > 0 { Arc::new(CachingDevice::new(device, opts.cache_blocks)) } else { device };
    Ok(Chain { mode, device, path, tmp, reason, overlay, base })
}

impl Chain {
    /// Overlay ⇒ 只回放脏块（源是常规文件）／InPlace ⇒ flush／ReadOnly ⇒ 不做事
    pub fn commit(&self, dst: Option<&dyn BlockDevice>) -> VolResult<CommitStats>;
    pub fn reset(&mut self) -> VolResult<()>;     // 丢弃覆盖层，回到「源 + 空覆盖」（core 重试语义）
}
```

## 步骤 3：跑测试与验收

```bash
cargo test -p hzip-fs-ext4 -p hzip-vol -p hzip-fmt-raw
python build/checks/deps-direction.py
# 手工（M2 观感）：用 ext4 镜像跑 GUI/CLI 的增删改，再用上游 fsck 复核
```

### 步骤 4：提交

```bash
git add crates/hzip-vol/src/device.rs crates/hzip-fs-ext4 crates/hzip-vol/tests
git commit -m "feat(vol): ext4 写侧与三态设备链（模式 A/B/只读）"
```

## 验收

- [ ] 五类变更（Append/Replace/Delete/Rename/MakeDir）在真实 ext4 夹具上成立，且**写后重挂仍成立**
- [ ] 写后 `audit()` 干净；`fsck.ext4`（开发机存在时）复核无错
- [ ] 脏卷：默认拒写且 `read_only_reason == JournalPending`；显式允许后重放可写
- [ ] 设备链三态各自成立：B 提交前源不变、A 立即生效、只读拒写并有理由
- [ ] `InPlace` 打不开 r/w 时**降级 + 理由**（不静默、不改用户文件）
- [ ] 盘满/非空目录删除/无效 inode 三类错误都是可读 `VolError`，无 panic

## 风险 / 注意

| 点 | 处置 |
|---|---|
| 写坏用户数据 | 默认模式 B（T-IMG6 的覆盖层）；A 模式必须显式开启；两条路径都在提交前 `audit()` |
| `apply_replace_file_content` 的内存占用（整文件读进内存） | 上限用 `QuotaKind::MaxFileBytes` 卡（既有配额），超限走 `apply_pwrite` 分块路径（同源 API，T-IMG13 视需要切换） |
| 上游对「磁盘空间不足」的行为 | 夹具专测一条；我们只负责把错误映射成可读文案，不尝试自动腾空间 |
| 时间戳保真 | `Replace` 后必须 `apply_utimens`（本地 mtime），否则「编辑保存后时间变了」会被用户当 bug 报 |

## §落地记录（2026-10-16）

**已完成**（子代理交付，19 条用例：`tests/read_only.rs` 8 + `tests/write.rs` 11；clippy `-D warnings` 干净）：
`Ext4Driver`（`supports` 只认 `s_magic`@1080）+ `Ext4Volume`（全树列出/惰性读/五类 `Change`/flush/audit），挂载策略**一律 `mount_lazy` + 用户显式同意后 `replay_journal_if_dirty()`**。

**实测得出的上游事实（已写进代码注释）**：
1. 所有 ext4 **路径版 API 需要前导 `/`**（`split_parent_and_base` 对相对路径直接 `InvalidArgument`）⇒ 调用侧统一 `abs()`；
2. `mount_recovering` 要求卷自带内部日志（无日志卷直接 `Unsupported`）；`mount` 在**可写设备上会静默重放**脏日志 ⇒ 与「默认不静默改盘」冲突，故不用；
3. 超大 payload 上游报 `Corrupt("no group has a contiguous free run of this size")` 而非 ENOSPC ⇒ 用例断言「不 panic + 可读 `VolError` + 既有数据不受损」；
4. `FsOptions::cache_blocks` 有意不生效（上游无按块数挂载的入口；读缓存归 `hzip-vol` 的 `CachingDevice`）——**不留假实现**。

**偏离**：`Delete` 非空目录报 `vol.err.dir-not-empty`（计划原写 `vol.err.dir-not-empty` ✔ 一致）；未覆盖「未知 `Change` 变体」（无法构造，实现已就位）。
