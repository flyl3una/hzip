# T-IMG1：契约扩展（`hzip-format`）

| 项目 | 内容 |
|---|---|
| 阶段 | P0 地基（全链路的唯一契约改动，后续任务都建在它上面） |
| 依赖 | 无 |
| 状态 | **已实施**（2026-10-16；证据见 §0 落地记录） |
| 目标 | 让既有格式契约为「块设备型容器」多出三件它现在表达不了的东西：① 可就地写的能力声明；② 无魔数格式的内容探针；③ 中间强度的置信度 |

## §0 落地记录（2026-10-16）

**实施**：子代理起手完成 `hzip-format` 六个文件与 `tests/probe_contract.rs`，随后因上游模型配额中断（403），由主链路收口消费侧连锁 + 验收。

**与计划的唯一偏离（已定稿为新口径）**：「可就地编辑」**不做 `FormatCaps` 字段，改为 `FormatHandler::supports_in_place_edit()` 默认方法**。

| 原因 | 事实 |
|---|---|
| 字段方案的连锁面大到不划算 | 全仓 `FormatCaps { … }` 字面量 **60 处**（8 个 fmt crate + testkit mock/stub + GUI `op_window` 全被打穿），且部分站点以 `..SINGLE_CAPS` 之类的**基结构体**收尾，机械补位极易出错（实测补出 3 处语法错误 + 8 个文件误伤，已全部回退） |
| 本质是**行为声明**而非数据 | 「有没有 `write_in_place` 的实现」与 `probe` 同性质 ⇒ 与 `probe` 一样用**默认方法**表达，既有格式**零改动、零重编译波及** |
| 对比：`CreateOptions.volume_write`（同批新增字段） | 全仓 30 处 `CreateOptions { … }` 字面量里**只有 1 处**（`hzip-core/src/services/compress.rs` 的 `create_options`）真的需要补位，其余早已 `..Default::default()` ⇒ 字段方案在该类型上代价可接受，故保留 |

**验收证据**：

```bash
cargo test -p hzip-format              # 17 lib + 11 probe_contract + 8 sniff_registry 全绿
cargo check --workspace --all-targets  # 通过（既有 8 个 handler 一行未改）
cargo fmt --check ; cargo clippy --workspace --all-targets -- -D warnings
```

## 交付物

| 动作 | 文件 | 改什么 |
|---|---|---|
| 改 | `crates/hzip-format/src/handler.rs` | `FormatHandler::supports_in_place_edit()`（默认 `false`；**不放 `FormatCaps` 字段**，原因见 §0） |
| 改 | `crates/hzip-format/src/handler.rs` | `FormatHandler::probe/probe_head_bytes`（默认实现，**老 handler 零改动**）；`CreateOptions.volume_write: VolumeWriteMode` |
| 改 | `crates/hzip-format/src/caps.rs` | 新增 `pub enum VolumeWriteMode { Overlay, InPlace, ReadOnly }`（`Default = Overlay`） |
| 改 | `crates/hzip-format/src/sniffer.rs` | `Confidence::Medium`；`sniff_impl` 增 probe 档（序：魔数 → ustar → **probe** → 扩展名回退） |
| 改 | `crates/hzip-format/src/registry.rs` | `sniff()` 把 registry 自身传进 `sniff_impl`（probe 要问 handler） |
| 改 | `crates/hzip-format/src/editor.rs` | `ArchiveEditor::write_in_place`（默认 `Unsupported`） |
| 改 | `crates/hzip-format/src/lib.rs` | 导出 `VolumeWriteMode` |
| 新 | `crates/hzip-format/tests/probe_contract.rs` | 探针/置信度/默认实现的契约测试 |

## 步骤

### 1. 写失败测试

`crates/hzip-format/tests/probe_contract.rs`：

```rust
use hzip_format::caps::FormatCaps;
use hzip_format::{Confidence, FormatHandler, FormatId, FormatRegistry, OpenOptions, Signature, Sniffed, Source};

struct FakeRaw;                                   // 无魔数、只有探针的假格式
impl FormatHandler for FakeRaw {
    fn id(&self) -> FormatId { FormatId::from("fakeraw") }
    fn signature(&self) -> &'static Signature {
        static S: Signature = Signature { magic: b"", offset: 0, display: "fake raw", extensions: &["fakeraw"] };
        &S
    }
    fn caps(&self) -> FormatCaps { FormatCaps { read: true, ..Default::default() } }
    fn probe(&self, head: &[u8]) -> Option<Confidence> {
        (head.len() >= 2048 && head[..2] == [0x53, 0xEF]).then_some(Confidence::Medium)
    }
    fn probe_head_bytes(&self) -> usize { 2048 }
    fn open_reader(&self, _s: Source, _o: &OpenOptions) -> Result<Box<dyn hzip_format::ArchiveReader>, hzip_error::HzipError> {
        Err(hzip_error::HzipError::Unsupported { what: "fake".into() })
    }
    fn create_writer(&self, _d: hzip_format::Sink, _o: &hzip_format::CreateOptions) -> Result<Box<dyn hzip_format::ArchiveWriter>, hzip_error::HzipError> {
        Err(hzip_error::HzipError::Unsupported { what: "fake".into() })
    }
    fn open_editor(&self, _s: Source, _o: &OpenOptions) -> Result<Box<dyn hzip_format::ArchiveEditor>, hzip_error::HzipError> {
        Err(hzip_error::HzipError::Unsupported { what: "fake".into() })
    }
}

fn reg() -> FormatRegistry {
    let mut r = FormatRegistry::new();
    r.register_handler(Box::new(FakeRaw)).unwrap();
    r
}

#[test]
fn probe_hits_with_medium_confidence() {
    let mut head = vec![0u8; 2048];
    head[..2].copy_from_slice(&[0x53, 0xEF]);            // ext 魔数（故意不是 fakeraw 的扩展名）
    let p = write_tmp("blob.bin", &head);
    assert_eq!(reg().sniff(&Source::File(p)), Ok(Sniffed::Single(FormatId::from("fakeraw"), Confidence::Medium)));
}

#[test]
fn probe_miss_falls_back_to_extension_low() {
    let p = write_tmp("a.fakeraw", b"nothing-to-see");   // 探针不认 ⇒ 扩展名回退
    assert_eq!(reg().sniff(&Source::File(p)), Ok(Sniffed::Single(FormatId::from("fakeraw"), Confidence::Low)));
}

#[test]
fn magic_beats_probe() {
    let p = write_tmp("p.zip", b"PK\x03\x04rest");       // 真 zip：魔数档先行，探针不得改写
    assert_eq!(reg().sniff(&Source::File(p)), Ok(Sniffed::Single(FormatId::from("zip"), Confidence::High)));
}

#[test]
fn probe_head_bytes_is_honoured() {
    // 探针要 2048 字节：源只有 1024 ⇒ 不命中（落到扩展名），证明窗口按声明给
    let p = write_tmp("short.fakeraw", &vec![0x53, 0xEF][..].iter().cycle().take(1024).copied().collect::<Vec<_>>());
    assert_eq!(reg().sniff(&Source::File(p)), Ok(Sniffed::Single(FormatId::from("fakeraw"), Confidence::Low)));
}

#[test]
fn default_probe_is_absent_and_in_place_is_unsupported() {
    // 既有格式（zip）未实现 probe ⇒ 行为零变化；未实现 write_in_place ⇒ Unsupported
    assert!(hzip_fmt_zip::ZipHandler::new().probe(&vec![0u8; 4096]).is_none());
}
```

### 2. 跑测试确认失败

```bash
cargo test -p hzip-format --test probe_contract
# 预期：编译失败 —— no method named `probe` / no variant `Medium`
```

### 3. 实现

`caps.rs`：能力位 + 写模式枚举（默认值即用户裁定 ②）：

```rust
// 能力声明（`FormatCaps` 不动；原因见 §0）：
pub trait FormatHandler: Send + Sync {
    /* …既有五个方法… */
    /// 可就地编辑：本 handler 的 `ArchiveEditor` 实现了 `write_in_place`。
    /// core 据此对**常规文件源**跳过「重写整档 + 原子替换」（40 GB 镜像不可接受全量拷）。
    fn supports_in_place_edit(&self) -> bool { false }
}

/// 卷类格式的写回模式（用户裁定 ②：默认 `Overlay`，设置页可切 `InPlace`/只读）。
/// 经 `CreateOptions` 由 core 下发到格式实现（设备链末端选择见 T-IMG2 §device）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VolumeWriteMode {
    /// 覆盖层暂存 → 提交时只回放脏块（默认；提交前原文件一字节不动）
    #[default]
    Overlay,
    /// 直接写原文件（快；原子性由格式/文件系统自持）
    InPlace,
    /// 只读（不写任何字节）
    ReadOnly,
}
```

`handler.rs`：探针（**默认实现 ⇒ 既有 8 个 fmt handler 一行不改**）：

```rust
pub trait FormatHandler: Send + Sync {
    /* …既有五个方法… */

    /// 内容启发式探测（无魔数格式用：raw 镜像、裸文件系统卷）。
    /// `head` = 源的前 [`Self::probe_head_bytes`] 字节；`Some(置信度)` = 「这是我的」。
    /// 默认 `None` = 不参与探测（既有格式零成本）。
    fn probe(&self, _head: &[u8]) -> Option<Confidence> { None }

    /// probe 需要的头部字节数。默认 512（与魔数嗅探同一次读）；上限见 `sniffer::PROBE_HEAD_MAX`。
    fn probe_head_bytes(&self) -> usize { 512 }
}
```

`CreateOptions` 增字段（core 构造点已是 `CreateOptions::default()` + 逐字段赋值，故兼容；其余字面量构造点补 `..Default::default()`，先 `grep -rn "CreateOptions {" crates` 清点）：

```rust
pub struct CreateOptions {
    /* …既有字段… */
    /// 卷类格式写回模式（其余格式忽略）
    pub volume_write: VolumeWriteMode,
}
```

`editor.rs`：就地写（默认拒绝 ⇒ core 自动回落 `rewrite`）：

```rust
pub trait ArchiveEditor: Send {
    fn rewrite(&self, changes: ChangeSet, dst: Sink, ctx: &WriteCtx<'_>) -> Result<WriteReport, HzipError>;

    /// 就地写：直接对源对象应用变更集（**不产出新档**）。
    ///
    /// 与 `rewrite` 的分工：`rewrite` 产出 `dst` 后由调用方原子替换；本方法改的是源本身，
    /// 原子性由格式自持（卷格式 = 覆盖层暂存 → 提交回放，见 T-IMG6）。
    ///
    /// **可重入要求**：core 的密码重试会对同一 editor 多次调用 ⇒ 每次调用必须从干净
    /// 中间态开始（丢弃上一次的暂存），不得累积。
    ///
    /// 默认实现 = `Unsupported`（HZ-5001）；仅 `supports_in_place_edit()` 为真的格式实现。
    fn write_in_place(&self, _changes: ChangeSet, _ctx: &WriteCtx<'_>) -> Result<WriteReport, HzipError> {
        Err(HzipError::Unsupported { what: "hz-5001 in-place-edit-unsupported".into() })
    }
}
```

`sniffer.rs`：置信度 + probe 档（插在 ustar 判定之后、扩展名回退之前）：

```rust
pub enum Confidence { High, Medium, Low }        // Medium = 内容探针（强于扩展名，弱于魔数）

pub(crate) const PROBE_HEAD_MAX: usize = 4096;

pub(crate) fn sniff_impl(reg: &FormatRegistry, src: &Source) -> Result<Sniffed, HzipError> {
    let head = read_head(src, 512)?;
    /* …既有：魔数表 → is_ustar → … 命中即返回… */
    if let Some(hit) = probe_sweep(reg, src)? { return Ok(hit); }   // ← 新档
    if let Some(f) = match_extension(src) { return Ok(Sniffed::Single(f, Confidence::Low)); }
    Err(bad_signature("unknown"))
}

/// 只问**声明了探针的** handler；取置信度最高者（并列取注册序先者）。
fn probe_sweep(reg: &FormatRegistry, src: &Source) -> Result<Option<Sniffed>, HzipError> {
    let mut best: Option<(FormatId, Confidence)> = None;
    let mut heads: Vec<(usize, Vec<u8>)> = Vec::new();     // 按需读，同一长度只读一次
    for h in reg.handlers() {                              // registry 新增只读迭代器（见下）
        let need = h.probe_head_bytes().clamp(512, PROBE_HEAD_MAX);
        let head = match heads.iter().find(|(n, _)| *n == need) {
            Some((_, b)) => b.clone(),
            None => { let b = read_head(src, need)?; heads.push((need, b.clone())); b }
        };
        if let Some(c) = h.probe(&head) {
            if best.as_ref().is_none_or(|(_, bc)| c > *bc) { best = Some((h.id(), c)); }
        }
    }
    Ok(best.map(|(id, c)| Sniffed::Single(id, c)))
}
```

`registry.rs`：嗅探入口把自身传下去，并给探针一个只读迭代器：

```rust
pub fn sniff(&self, src: &Source) -> Result<Sniffed, HzipError> { sniff_impl(self, src) }

/// 注册序的全部 handler（探针扫描用；不给可变访问）。
/// 实现：对注册表内部持有的 handler 列表做只读迭代（字段名以现实现为准）。
pub fn handlers(&self) -> impl Iterator<Item = &dyn FormatHandler> { self.registered.iter().map(|h| h.as_ref()) }
```

> 落地注记：`read_head(src, n)` = 现有头部读取函数的参数化版本（`File` 直接读 n 字节；`Volumes` 读首卷；`Stdin` 沿用缓冲代理；`Stream` 走既有「不可重放 ⇒ `Unsupported`」分支）。`is_none_or` 若 MSRV 不支持则改 `map_or(true, …)`。

### 4. 跑测试确认通过

```bash
cargo test -p hzip-format
cargo test -p hzip-format-testkit
cargo test --workspace          # 老格式行为零变化（红线：既有 sniff 用例断言逐条不变）
```

### 5. 提交

```bash
git add crates/hzip-format
git commit -m "feat(format): 契约扩展——内容探针/Medium 置信度、supports_in_place_edit、write_in_place、volume_write"
```

## 验收

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p hzip-format -p hzip-format-testkit
cargo test --workspace
```

- [ ] `FormatCaps { ..Default::default() }` 的老构造点全部仍编译（8 个 fmt crate 零改动）
- [ ] 既有嗅探用例（`tests/sniff_registry.rs` / `tests/sniff_*.rs`）断言不变
- [ ] `Signature` 结构体未改（避免 8 个 crate 的常量字面量连锁改动）

## 风险 / 注意

| 点 | 处置 |
|---|---|
| 探针对**每个** handler 都问一次，等于给嗅探加了一次 512 B~4 KiB 读 | 只有卷类 handler 实现 probe；`read_head` 按长度去重（同长度只读一次）；探针必须**纯函数、不落盘、不 panic** |
| `Confidence` 加变体可能打穿既有 `match` | 编译期即暴露（`cargo check --workspace` 一次过干净）；`Medium` 只由 probe 产生 |
| `CreateOptions` 加字段的连锁 | 先 `grep -rn "CreateOptions {" crates` 清点字面量构造点；`..Default::default()` 补位 |
| `write_in_place` 的可重入 | 契约里写明（上面 doc comment）；T-IMG6 用测试钉死「同 editor 连续两次调用结果等价于一次」 |
