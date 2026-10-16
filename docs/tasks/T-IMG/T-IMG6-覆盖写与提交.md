# T-IMG6：覆盖写、提交与原子性

| 项目 | 内容 |
|---|---|
| 阶段 | P2 读写闭环（用户裁定 ② 的落点：默认 B = 覆盖写，提交时只回放脏块） |
| 依赖 | T-IMG5（设备链）；`Sink::Stream` 交付路径还需 **T-IMG0**（组合/嵌套写回） |
| 状态 | 未开工 |
| 目标 | 把「改了但还没提交」这一段做成**可取消、可重试、原档不变**；提交时能只写脏块（源是文件）或整量重写（源是流） |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 改 | `crates/hzip-vol/src/overlay.rs` | 定稿：脏块表、`replay_into`、`stream_out`、`dirty_bytes`、幂等 |
| 改 | `crates/hzip-vol/src/device.rs` | `Chain::{commit,reset,stream_into}` 定稿 + 提交期内进度 |
| 改 | `crates/hzip-vol/src/editor.rs` | `VolumeEditor::{rewrite,write_in_place}` 定稿（两条交付路径 + 提交前 audit + 可重入） |
| 新 | `crates/hzip-vol/tests/overlay_commit.rs` | 覆盖层提交面（幂等/只脏块/取消不动原档/流拷等价） |
| 新 | `crates/hzip-vol/tests/editor.rs` | 编辑器两路径 + 可重入 + audit 失败中止（mock 卷） |

## 设计要点（为什么是自持覆盖层，而不是别的）

| 备选 | 否决理由 |
|---|---|
| 每次编辑都 `fs::copy` 整个镜像再改 | 40 GB 镜像不可接受；且崩溃留下半个副本 |
| 直接就地写（模式 A）为默认 | 用户裁定默认 B：提交前原档必须一字节不动 |
| 用 `Qcow2Reader` 当覆盖层（qcow2 天生就是 overlay 格式） | 会把 qcow2 的写依赖塞进**所有**格式（raw 也得背）；且 qcow2 层是「文件级容器」，我们的覆盖必须是「设备级装饰」。自持 ~300 行、可差分测试，更简单 |
| 用「稀疏拷贝 + 改原副本」 | 需要预知哪些块会被改；覆盖率靠猜。覆盖层天然按需只留**真正写过的块** |

## 步骤 1：写失败测试

`crates/hzip-vol/tests/overlay_commit.rs`：

```rust
#[test]
fn replay_touches_only_dirty_blocks() {
    // base = 3 块 0xAA；写第 2 块；replay_into(target=3 块 0x77)
    // ⇒ 仅第 2 块变化，其余仍 0x77
}

#[test]
fn commit_is_idempotent() {
    // 同一覆盖层 replay 两次 ⇒ 目标字节一致（core 的重试语义会重复调用）
}

#[test]
fn cancel_before_commit_leaves_source_byte_identical() {
    let before = sha256(&base);
    { let mut chain = Chain::open(Source::File(p), WriteMode::Overlay, &opts)?;
      apply_something(&mut chain)?;          // 写了若干块
      return; /* 模拟取消：drop chain，不 commit */ }
    assert_eq!(sha256(&base), before, "取消后原档必须一字节不变");
    // 并断言临时溢出文件已清理（TempArea 会话目录为空）
}

#[test]
fn stream_out_equals_device_bytes_and_reports_progress() {
    // stream_out 收集的字节 == 逐块读 OverlayDevice；进度事件数和字节数合理（≈ 镜像长度）
}
```

`crates/hzip-vol/tests/editor.rs`（mock `Volume`/`FsVolume`，记录 apply 调用与设备写入）：

```rust
#[test]
fn write_in_place_returns_report_and_commits_dirty_blocks() { /* CommitStats.dirty_blocks > 0；源已变 */ }

#[test]
fn two_calls_are_equivalent_to_one() {
    // 第 1 次调用（模拟密码错重试后的第二次）后 reset ⇒ 脏块集合不累积：
    // 两次分别提交到两个独立目标，结果字节一致
}

#[test]
fn audit_failure_aborts_before_touching_the_source() {
    // FsVolume::audit() → Err ⇒ write_in_place 返回错误，源 sha256 不变
}

#[test]
fn rewrite_to_file_copies_base_then_replays_dirty() { /* Sink::File ⇒ 目标 == base + 脏块 */ }

#[test]
fn rewrite_to_stream_emits_the_whole_image() {
    // Sink::Stream(Vec<u8>) ⇒ 收到镜像全量（长度 == virtual_size），供外层 gz 编码
}

#[test]
fn changes_are_applied_through_the_fs_layer_only() {
    // mock FsVolume 收到 Appends/Deletes/Renames 的顺序与 ChangeSet 一致（不跳步、不改序）
}
```

## 步骤 2：实现

### 2.1 覆盖层（`src/overlay.rs`）

```rust
/// 读穿透 + 写暂存。**未 commit 前原设备一字节不动**。
/// 读路径不做写时拷贝：脏块从 spill 读，未脏块从 base 读（省一次整块抄写）。
pub struct OverlayDevice { base: Arc<dyn BlockRead>, spill: File, dirty: Mutex<BTreeSet<u64>>, block: u64 }

impl OverlayDevice {
    pub fn new(base: Arc<dyn BlockRead>, spill: File, block: u64) -> Self;
    pub fn dirty_blocks(&self) -> u64;
    pub fn dirty_bytes(&self) -> u64 { self.dirty_blocks() * self.block }
    pub fn replay_into(&self, target: &dyn BlockDevice, progress: Option<&ProgressBus>) -> VolResult<u64>;
    pub fn stream_out(&self, w: &mut dyn Write, cancel: &CancellationToken) -> VolResult<u64>;
    /// 写入必须整块覆盖 spill 对应区间（部分写：`spill.write_all_at`），脏块标记按**触及的块**记
    fn touch(&self, offset: u64, len: u64) -> VolResult<()>;
}
```

### 2.2 提交的两条交付路径（`src/device.rs` + `src/editor.rs`）

| 交付目标 | 实现 | 何时用 |
|---|---|---|
| **源本身**（`write_in_place`，模式 B） | `FileDevice::open_rw(path)` → `replay_into(&target)` → `flush()` | core 判定 `handler.supports_in_place_edit() && 源是常规文件` |
| **`Sink::File(p)`**（`rewrite`） | `std::fs::copy(base, p)`（OS 级拷贝，不解码）→ `FileDevice::open_rw(p)` → `replay_into` | core 走「重写 + 原子替换」（例如源不可就地写） |
| **`Sink::Stream(w)`**（`rewrite`） | `stream_out(w)`（全量经过外层编码器） | 源在组合链/档案条目里（`a.qcow2.gz`、`v.zip` 内 `disk.vmdk`）——**外层接线由 T-IMG0 提供**（`CompositeEditor` 的编码链 / 逐层冒泡），本任务只负责 `stream_out` 与报告字段 |

```rust
impl ArchiveEditor for VolumeEditor {
    fn write_in_place(&self, changes: ChangeSet, ctx: &WriteCtx<'_>) -> Result<WriteReport, HzipError> {
        let mut chain = self.chain.lock().unwrap();
        chain.reset()?;                                       // ← core 重试语义：每次从干净中间态开始
        let mut fs = self.open_writable_fs(&chain)?;          // 挂载（含脏卷策略）
        for c in changes.changes.iter() {                     // 顺序即契约（core 已定序）
            check_cancel(ctx.cancel)?;
            fs.apply(c, local_of(c), ctx.cancel)?;
        }
        if !fs.audit()?.is_clean() { chain.reset()?; return Err(VolError::Unsupported("vol.err.audit").into()); }
        fs.flush()?;
        let stats = chain.commit(None)?;                      // Overlay ⇒ 只回放脏块；InPlace ⇒ flush
        Ok(WriteReport { entries_written: changes.changes.len(), bytes_out: stats.dirty_bytes, ..Default::default() })
    }

    fn rewrite(&self, changes: ChangeSet, dst: Sink, ctx: &WriteCtx<'_>) -> Result<WriteReport, HzipError> { /* 上表后两行 */ }
}
```

**进度与取消**：`apply` 逐条上报（`progress.emit(JobProgress::Step)` 级别即可）；交付期按 `replay_into`/`stream_out` 的块数上报 `Bytes`；`cancel` 命中 ⇒ 立即返回 `Cancelled`，**不 commit**（原档不变，覆盖层随 `Chain` drop 进 TempArea 清理）。

**提交失败**：`replay_into` 中途失败（磁盘满/权限）⇒ 已写的脏块无法自动回滚 ⇒ 记 `tracing::error!` + 返回 `VolError::Io`，并在报告里附「原档可能处于部分提交状态」标记（**如实告知**，不假装原子）。这一点写进 `WriteReport` 的扩展字段或日志，不允许静默。

## 步骤 3：跑测试与验收

```bash
cargo test -p hzip-vol
cargo test -p hzip-fs-ext4 -p hzip-fmt-raw        # 回归：写路径未破
python build/checks/deps-direction.py
```

### 步骤 4：提交

```bash
git add crates/hzip-vol
git commit -m "feat(vol): 覆盖层提交语义——只回放脏块、可重入、取消不动原档"
```

## 验收

- [ ] **取消后原档 sha256 不变**（专测；且临时溢出文件被清理）
- [ ] **回放幂等**：`commit` 两次结果一致（重试安全）
- [ ] **只回放脏块**：目标未脏字节不变（专测）
- [ ] `write_in_place` 连续两次调用等价于一次（脏块不累积）
- [ ] `rewrite` 两条交付路径各自成立：`Sink::File` 得到 base+脏块；`Sink::Stream` 收到镜像全量
- [ ] audit 失败 ⇒ 中止且源不变；提交中途失败 ⇒ **如实报告**部分提交状态
- [ ] 进度事件覆盖「应用变更 → 交付」两段；取消在任一阶段都生效

## 风险 / 注意

| 点 | 处置 |
|---|---|
| 提交中途失败无法回滚 | 明确「非整档原子」的边界（只承诺：提交前原档不变 + FS 层单操作日志化）；日志与文案如实说明 |
| 溢出文件占盘 | `dirty_bytes` 计入进度与日志；`QuotaKind` 卡「临时盘剩余空间」下限（写前检查 `temp_root` 可用空间 ≥ 预估） |
| `fs::copy` 走 OS 级拷贝仍是一次全量 | 只在 `Sink::File` 路径发生（core 已选择重写语义）；模式 B + 常规文件源优先走 `write_in_place` ⇒ 用户日常路径**不拷全量** |
| 大镜像 `stream_out` 的取消粒度 | 每块检查一次 `cancel`（64 KiB ⇒ 取消延迟可忽略） |

## §已定边界（2026-10-16 实测追加，vmdk 代理上报）

**需要「容器扩容」的写入在默认覆盖写模式下会干净失败**（`vol.unsupported.resize`）：

- 现象：`VmdkReader` 分配新 grain 时靠**加长宿主文件**（`set_len`），而 `OverlayDevice::set_len`
  按设计恒错（本期不做容量变更）⇒ 覆盖写链上「需要新分配」的改动失败；`write_in_place`（就地写）正常。
- 为什么不让覆盖层支持扩容：`fs_core` 允许 `set_len` 移动 `size_bytes()`（它是唯一的门），
  实现上「溢出文件长大 + 逻辑长度跟着长」可行，但要连带处理**回放时目标文件也要长大**与
  稀疏语义——属于独立一步，不在本轮。
- 处置：**如实失败 + 明确键**（不静默降级）；用户可切「就地写」或走「另存为」（`rewrite` 整量交割）。
  已记入 `hzip-fmt-vmdk/tests/write.rs` 的模块文档与 `OverlayDevice::set_len` 的 doc。
- 后续（若要支持）：`OverlayDevice` 支持增长 + `Chain::commit` 对「目标比源短」时先 `set_len`。
