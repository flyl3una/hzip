# T-IMG13：加固（fuzz / 配额 / 性能基线 / 交叉验证 / 故障注入）

| 项目 | 内容 |
|---|---|
| 阶段 | P5（三个文件系统驱动都到位后） |
| 依赖 | T-IMG11、T-IMG12 |
| 状态 | 未开工 |
| 目标 | 把「能跑」变成「扛得住」：坏镜像不 panic、资源有上限、性能有基线、提交失败有可核验的行为 |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 新 | `crates/hzip-vol/tests/fuzz_smoke.rs` | 截断/零填/位翻转的**不 panic** 扫描（头、分区表、ext4 超级块、qcow2 头、vmdk 头+descriptor） |
| 新 | `crates/hzip-vol/tests/fault_injection.rs` | `FaultyDevice`（第 N 次写失败）→ 提交前后行为断言 |
| 新 | `crates/hzip-vol/tests/limits.rs` | 条目上限/临时盘空间/取消延迟 |
| 新 | `script/vol-bench.sh` + `docs/tasks/T-IMG/perf-baseline.md` | 性能基线（含机器信息）与回归口径 |
| 新 | `script/vol-oracle.sh` | 交叉验证（qemu-img / 7z / e2fsck / chkdsk；缺失即 skip） |
| 改 | `crates/hzip-platform/src/**`（若无） | 临时盘可用空间查询（Windows `GetDiskFreeSpaceExW` / Unix `statvfs`） |
| 改 | `.github/workflows/ci.yml` | 新 crate 测试作业 + release 模式 vol 测试 + feature 裁剪构建 |

## 步骤 1：写失败测试

`tests/fuzz_smoke.rs`：

```rust
/// 每个输入形态三连：截断到若干长度、全零、单字节位翻转（选关键偏移）。
/// 断言只有一条：**不许 panic**；要么 Ok，要么 Err(VolError)。
#[test]
fn handles_are_total() {
    for corpus in [QCOW2_MIN, VMDK_SPARSE_MIN, MBR_IMAGE, GPT_IMAGE, EXT4_MIN] {
        for mutated in mutate_all(corpus) {            // 截断 8 档 + 全零 + 前 64 字节逐位翻转
            let dev = Arc::new(MemDevice::from_bytes(mutated));
            let _ = probe_and_open_all(dev);           // 结果丢弃，只求不炸
        }
    }
}

#[test]
fn descriptor_parser_is_total() { /* vmdk descriptor：CRLF/LF、大小写、空文本、超长行、非法 UTF-8 */ }
```

`tests/fault_injection.rs`：

```rust
#[test]
fn failure_before_commit_leaves_source_untouched() {
    /* 第 N 次写失败 ⇒ apply 返回 Err；源 sha256 不变（含 N 取遍 1..=8） */
}

#[test]
fn failure_during_commit_is_reported_as_partial() {
    /* 回放第 k 块时失败 ⇒ 返回 Err(VolError::Io) 且日志含 "partial_commit"；
       **不**返回成功（宁可报错也不假装写完了） */
}

#[test]
fn audit_failure_never_writes() { /* audit Err ⇒ 一次写都没发生（用 MemDevice 计数断言） */ }
```

`tests/limits.rs`：

```rust
#[test] fn entry_limit_truncates_and_reports()   { /* 限 1000 ⇒ 正好 1000 + truncated 标记 */ }
#[test] fn cancel_latency_is_bounded()           { /* 列表/读取/回放三处取消：从发信号到返回 < 1 个块的处理时间 */ }
#[test] fn temp_space_is_checked_before_materializing() { /* 可用空间 < 镜像大小 ⇒ 立即 Quota，不开始拷 */ }
```

## 步骤 2：实现与执行

### 2.1 资源上限（写进 `hzip-vol`，值进配置）

| 项 | 默认 | 说明 |
|---|---|---|
| `EntryLimit.max_entries` | 100_000 | `entries()` 上限；超限置 `truncated`（UI 提示「部分列出」） |
| 列表内存预算 | ≈ 20 MiB | 由 `EntryInfo` 大小 × 上限反推；超预算时下调上限并记日志 |
| `EntryLimit.max_depth` | 64 | 防递归爆栈（恶意/损坏镜像的循环目录） |
| 临时盘预检 | 需要 ≥ 镜像物化大小 + 溢出预估 | 不足 ⇒ `QuotaKind` 明确错误（**不半途而废**） |
| `VolOptions.cache_blocks` | 8192 | LRU 读缓存上限（≈512 MiB 视块大小；随 `04` 内存口径复核） |

### 2.2 性能基线（`script/vol-bench.sh`）

```bash
# 生成夹具（不依赖外部工具；稀疏创建 + 我们的夹具构造器）
cargo run -q -p hzip-format-testkit --features vol --bin vol_fixtures -- out/
# 场景（每次跑 5 遍取中位数）：
#   ① 打开 → 列表完成：raw(8 MiB) / raw(256 MiB, 5k 文件) / qcow2(256 MiB) / vmdk(256 MiB)
#   ② 读 10 MiB 单文件（含预览取前 64 KiB）
#   ③ 改一个文件 + 提交（模式 B：只回放脏块）
#   ④ 组合源 a.raw.gz（物化 + 列表）
# 输出：docs/tasks/T-IMG/perf-baseline.md（追加"机器 + 日期 + 中位数"一节）
```

**回归口径**：与上一节基线比 **±30%** 视为噪声内；超出需在提交信息里说明原因。CI 不做硬门（避免抖动误红），改为 nightly 手动跑。

### 2.3 交叉验证（`script/vol-oracle.sh`，开发机/可选）

```bash
# 存在才跑，缺失 skip（与上游 crate 对 qemu-img 的做法一致）
qemu-img check  <qcow2|vmdk>          # 结构合法性
qemu-img info   <镜像>                 # virtual size / cluster / backing
7z l            <镜像>                 # 条目对照（只读侧可比的子集）
e2fsck -fn      <ext4 分区镜像>        # 文件系统一致性
chkdsk /scan    <NTFS 卷>（Windows）    # 上游的权威口径
```

输出 `report.json`（每项：命令、退出码、摘要），进提交信息或任务卡附注。

### 2.4 故障注入设备（测试用）

```rust
/// 第 N 次 `write_at` 起返回 Io 错误；`next_fail` 可动态改。
struct FaultyDevice { inner: Arc<dyn BlockDevice>, writes: AtomicUsize, fail_at: AtomicUsize }
impl fs_core::BlockDevice for FaultyDevice { /* 透传 + 计数 + 注入 */ }
```

### 2.5 CI 与裁剪

```yaml
# .github/workflows/ci.yml 新增
- name: vol tests (debug + release)
  run: |
    cargo test -p hzip-vol -p hzip-fmt-raw -p hzip-fmt-qcow2 -p hzip-fmt-vmdk \
               -p hzip-fs-ext4 -p hzip-fs-ntfs -p hzip-fs-fat
    cargo test --release -p hzip-vol
- name: feature 裁剪构建（发行版可裁体积）
  run: cargo build --no-default-features -p hzip-core   # volume-formats 关掉也要能编
```

## 步骤 3：跑并记录

```bash
cargo test -p hzip-vol --release
python build/checks/deps-direction.py
cargo clippy --workspace --all-targets -- -D warnings
bash script/vol-bench.sh            # 追加基线
bash script/vol-oracle.sh           # 开发机（有工具时）
```

### 步骤 4：提交

```bash
git add crates/hzip-vol/tests script docs/tasks/T-IMG/perf-baseline.md .github/workflows/ci.yml
git commit -m "test(vol): fuzz 扫描、故障注入、资源上限、性能基线与交叉验证脚本"
```

## 验收

- [ ] fuzz 扫描（截断/零填/位翻转 × 五类头）**零 panic**
- [ ] 提交前任意一步失败 ⇒ 原档 sha256 不变；提交中失败 ⇒ 报错并记 `partial_commit`
- [ ] audit 失败路径**一次写都没发生**（计数断言）
- [ ] 条目上限/深度上限/临时盘预检三条有专测，且错误信息给出可操作提示
- [ ] 取消延迟有界（三处：列表 / 读取 / 回放）
- [ ] 性能基线入档（含机器信息），四个场景中位数可复现
- [ ] 交叉验证脚本在有工具时全绿；无工具时 skip 且**不误报成功**
- [ ] `cargo build --no-default-features -p hzip-core` 通过（feature 裁剪安全）

## 风险 / 注意

| 点 | 处置 |
|---|---|
| 上游 crate 有 panic 逃逸（第三方代码不受我们控） | fuzz 扫描发现的 panic：① 先在我们这层用 `catch_unwind` 兜住并在 report 里标注「上游缺陷」；② 提 issue/固定版本规避（不 fork 不魔改） |
| 基线受机器影响 | 只做「相对上一节基线 ±30%」的同机比较；机器信息写进基线文件 |
| 交叉验证工具版本漂移 | 脚本记录工具版本；报告中带版本号，避免"昨天绿今天红"无从判断 |
