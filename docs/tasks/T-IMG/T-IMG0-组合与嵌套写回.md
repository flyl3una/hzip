# T-IMG0：组合/嵌套会话的写回（**前置发现带来的新增任务**）

| 项目 | 内容 |
|---|---|
| 阶段 | P2 之前（与 T-IMG5-8 可并行；**必须在 T-IMG9 之前**） |
| 依赖 | T-IMG2（`hzip-vol` 设备链与物化）；与 T-IMG3-8 无耦合 |
| 状态 | **已落地（2026-10-19）**；剩余：GUI 保存前的**确认框**（当前保存即提交 + 弹进度窗）、深度 > 1 的容器套容器、cpio 外层（无写侧） |
| 目标 | 让「外层被包裹」的镜像也能改——用户三个示例里 `qcow2.gz` 与 `vmdk.zip` **都**属于这一类，而今天 hzip 的组合/嵌套编辑是**明确不支持**的 |

## §0 为什么必须新增这个任务（勘察结论，2026-10-16）

| 证据 | 事实 |
|---|---|
| `crates/hzip-format/src/registry.rs:151-153` | `open_editor` 对 `Sniffed::Composite` 直接 `Err(Unsupported{ what: "组合格式不支持编辑" })`——`.tar.gz`/`.cpio.gz` 这类组合档**没有任何编辑通路** |
| `crates/hzip-gui/src/views/browse.rs:316,466` | `can_edit: snap.caps_edit && !snap.nested`——**嵌套会话一律禁编辑**（进到 `v.zip` 里的 `disk.vmdk` 后，改名/新建/删除全部置灰） |
| `crates/hzip-gui/src/app/state.rs:763-784` | `caps_edit()` 的口径写明：「= registry caps.edit 且**非组合格式**（tar/single/rar/组合 → false）」 |
| `crates/hzip-core/src/services/edit.rs:121-205,476-495` | 编辑落点 = `rewrite` 到同目录 `{name}.hzip-tmp-{uuid}` → `fs_util::atomic_replace`；**只对「源本身是一个可编辑的档案」成立** |

结论：`qcow2.gz` / `vmdk.zip` 里的卷容器即使实现了读写（T-IMG5-8），**用户点不动**。故把「组合/嵌套写回」单列为前置任务，否则 M2 的验收对不上用户需求。

## §1 两种形状（互不相同的实现路径）

| 形状 | 例子 | 外层是什么 | 写回怎么做 |
|---|---|---|---|
| **形状 1：外层是编码层** | `b.qcow2.gz`（也可 `.bz2/.xz/.zst`） | 压缩编码器，没有「条目」概念 | **`CompositeEditor` = 编码链 + 内层编辑器**：按 `layers` 逆序搭编码器挂到 `dst`，把 `Sink::Stream(编码器)` 交给内层编辑器，内层写完自内向外 finish |
| **形状 2：外层是容器条目** | `v.zip` 里的 `disk.vmdk`、`v.7z` 里的 `disk.vmdk` | zip/7z 容器（有编辑器） | **逐层冒泡**：内层编辑器产出「新内层档」临时文件 → 外层 `ArchiveEditor::rewrite(ChangeSet::Replace{ 该条目 id, 新内层档 })` → 最外层原子替换 |

不可写的链**如实保持只读**：外层是 `tar`/`cpio`/`rar`（无编辑器）⇒ 整链 `caps.edit=false`（文案键 `vol.readonly.outer-not-editable`）；此时内层卷也可能「能读不能改」。

## §2 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 新 | `crates/hzip-core/src/services/codec_chain.rs` | 编码链装配（`build_encoder_chain`）：把「内层已写好的字节」逐层包编码器 → `Sink::Stream`；**从既有组合写路径抽公共函数**（`services/convert.rs` 的组合写 + `services/compress.rs::composite_codec/compress_composite`），三处共用一份，不新造第二份 |
| 新 | `crates/hzip-core/src/services/composite_edit.rs` | `CompositeEditor`（形状 1）与 `NestedEditPlan`/`WriteBackStep`（形状 2）的装配与执行 |
| 改 | `crates/hzip-format/src/registry.rs:151-153` | **保持 `Composite ⇒ Unsupported`**（只把文案改成「组合编辑由 core 编辑管线装配」）；新增**辅助 API** `pub fn open_inner_editor(&self, src: &Source, opts: &OpenOptions) -> Result<(Vec<FormatId>, Box<dyn ArchiveEditor>), HzipError>`：返回「编码层清单 + 最内层容器编辑器」，供 core 装配 |
| 改 | `crates/hzip-core/src/services/edit.rs` | ① 组合档（`Sniffed::Composite`）走 `composite_edit`，不再直接 `Unsupported`；② 新增**嵌套会话**的逐层冒泡（形状 2）：`NestedEditPlan` 描述 [外层档案, 条目路径, …, 最内层] 的写回链；③ `dst` 仍是同目录临时档 + `fs_util::atomic_replace` |
| 改 | `crates/hzip-core/src/services/browse.rs`（`Browser`/会话帧） | 会话携带 `chain_editable: bool`（打开时逐层求「caps.edit ∩ 外层可重建」）与写回链信息 |
| 改 | `crates/hzip-gui/src/app/state.rs:763-784`、`views/browse.rs:316,466` | `caps_edit` 的快照值改由会话的 `chain_editable` 给出（**删掉硬编码的 `!snap.nested`**）；改名/新建/追加/删除的启用条件随之一致 |
| 改 | `crates/hzip-gui/src/ui/shell/open_flow.rs:289-580` | 打开嵌套会话时把「写回链」记进会话（形状 2 需要条目 id 与父档案监听） |
| 新 | `crates/hzip-core/tests/composite_edit.rs` | 形状 1/2 端到端 + 不可编辑链的拒绝 |

## §3 步骤 0：先钉死现状行为（红→绿之前它就是文档）

```rust
#[test]
fn composite_edit_is_unsupported_today() {
    let gz = build_gz_of(&editable_inner());          // 用一个可编辑的内层（zip）造 `.zip.gz`
    let reg = registry_with_defaults();
    let err = reg.open_editor(Source::File(gz), &OpenOptions::default()).unwrap_err();
    assert!(matches!(err, HzipError::Unsupported { .. }), "registry 侧仍不支持组合编辑（本任务把它挪到 core 装配）");
}

#[test]
fn inner_editor_helper_exposes_layers_and_the_container_editor() {
    // 新增辅助 API：open_inner_editor ⇒ (layers == ["gz"], inner.caps().edit == true)
}
```

## §4 步骤 1：写失败测试

`crates/hzip-core/tests/composite_edit.rs`：

```rust
#[test]
fn shape1_edit_inside_gz_wrapped_volume_rewrites_the_outer_file() {
    // a.raw.gz：内层是可编辑卷（raw+ext4 夹具）
    let gz = make_gz(&testkit::vol::raw_with_mbr(FsKind::Ext4, &[("/note.txt", b"v1")]));
    let before_other = read_entry_sha256(&gz, "…/keep.txt");
    /* 改 /note.txt → 提交 */
    // 断言：① gz 仍可解、镜像内容 == v2；② 未改动文件 sha256 不变；③ 原档在失败/取消时 sha256 不变
}

#[test]
fn shape2_edit_inside_zip_wrapped_volume_replaces_the_entry() {
    let zip = make_zip(&[("disk.vmdk", testkit::vol::vmdk_sparse(&testkit::vol::raw_with_mbr(FsKind::Ext4, &[("/note.txt", b"v1")])).as_slice()),
                         ("unrelated.bin", b"keep")]);
    /* 打开 → 进 disk.vmdk → 改文件 → 提交 */
    // 断言：zip 条目数不变；disk.vmdk 字节变；unrelated.bin 字节不变（**子压缩包不被重压**——既有注释语义）
}

#[test]
fn tar_wrapped_volume_stays_read_only_with_a_reason() {
    // a.raw.tgz：外层 tar 无编辑器 ⇒ caps_edit=false 且理由是 vol.readonly.outer-not-editable
}

#[test]
fn nested_edit_report_mentions_the_chain() {
    // 报告里给出「哪一层被重写」（外层 gz / 条目 disk.vmdk），便于进度窗与任务中心展示
}
```

GUI 侧（`crates/hzip-gui/src/ui/views/tests/browse.rs`）：

```rust
#[test]
fn nested_session_edit_enablement_follows_chain_editable() {
    // chain_editable = true  ⇒ 嵌套会话里改名/删除/新建**可用**（取代旧 `!nested` 硬禁）
    // chain_editable = false ⇒ 置灰且 tooltip = 理由键
}
```

## §5 步骤 2：实现

### 5.1 形状 1 的落点：**装配在 core，不在 `hzip-format`**

勘察结论（2026-10-16 复核）：**编码链的装配今天就在 core**——`services/convert.rs`（文件头即写「02 §9.2 convert 职责 + §7.5 组合写入」）与 `services/compress.rs`（`composite_codec()` 映射 + 组合档创建两遍法/`composite_temp_path`）；而 `FormatRegistry::create_writer` 是**故意桩**：

```rust
// crates/hzip-format/src/registry.rs:132-136
let _ = (dst, opts);
Err(HzipError::Unsupported { what: "请按已嗅探格式调用 handler(id).create_writer（组合写归 core 转换管线）".into() })
```

故组合**编辑**不应塞回 `registry.open_editor`（那会让 CLI/GUI/既有测试都突然要面对「组合编辑器」这个新对象），而应落在**已有的一处装配地**——core 的编辑管线：

```rust
// crates/hzip-core/src/services/composite_edit.rs
/// 组合编辑 = 编码链 + 内层编辑器（形状 1）。
pub struct CompositeEditor {
    inner: Box<dyn ArchiveEditor>,   // 最内层容器（zip/7z 有；tar/cpio/rar 无 ⇒ 本形状不成立）
    layers: Vec<FormatId>,           // 外→内（来自 registry::open_inner_editor）
}

impl ArchiveEditor for CompositeEditor {
    fn rewrite(&self, changes: ChangeSet, dst: Sink, ctx: &WriteCtx<'_>) -> Result<WriteReport, HzipError> {
        // ① 编码链挂到 dst：File → [zst] → [xz] → [bz2] → [gz]（自内向外包；codec_chain 装配）
        let encoder = codec_chain::build_encoder_chain(dst, &self.layers)?;
        // ② 内层编辑器把「新内层档」写进编码器
        let mut report = self.inner.rewrite(changes, encoder, ctx)?;
        // ③ finish 自内向外（收尾守卫在 drop/close 时逐层结束）
        report.volume_paths = …;
        Ok(report)
    }
    // write_in_place 不实现：组合层没有「源可改」的对象，外层必须重写
}
```

`codec_chain` 的落地要求：**抽出 `convert.rs`/`compress.rs` 里已有的编码器装配**，三处共用一份（本仓库已因「四处各写一份扩展名表」踩过坑，不再制造第二份）。

### 5.2 形状 2：逐层冒泡（`hzip-core`）

```rust
/// 嵌套会话的写回链：从最内层向外，每层一步。
pub enum WriteBackStep {
    /// 本层是档案内的条目：把新内层档替换到该条目（外层 zip/7z 编辑器的 `Change`）
    ReplaceEntry { archive: PathBuf, entry: EntryId, new_bytes: PathBuf },
    /// 本层是编码层：无条目（形状 1，由 CompositeEditor 覆盖）
    EncodeLayer { codec: FormatId },
}

pub struct NestedEditPlan { pub steps: Vec<WriteBackStep> }   // 外→内

/// 执行（在既有 edit 服务的后台线程里）：
/// 1) 内层编辑器 rewrite → 新内层档（同目录临时文件）
/// 2) 逐层向外：`outer_editor.rewrite(ChangeSet::Replace{ entry, tmp }, Sink::File(outer_tmp))`
/// 3) 最外层 `fs_util::atomic_replace`（**只替换最外层一次**）
/// 失败：任一步 Err ⇒ 删掉所有临时档、原档字节不变（既有 TC-CONF-009 口径）
```

要点：**任何中间层都不做原地修改**；只有最外层做一次原子替换 ⇒ 与既有「编辑保存」的失败语义完全一致。

### 5.3 嵌套会话的编辑启用（GUI + core）

```rust
// 打开会话时（hzip-gui/src/app/state.rs::begin_nested_session 与 open_flow::open_container）：
//   chain_editable = 逐层 [caps.edit && 外层可重建] 的与
//     · 形状 1：内层可编辑 + 编码层齐全
//     · 形状 2：内层可编辑 + 外层档案 caps.edit（zip/7z）+ 外层可写
//     · 任一层是 tar/cpio/rar ⇒ false（理由键 vol.readonly.outer-not-editable）
// 快照投影：`AppStateUiSnapshot.caps_edit = chain_editable`（**删掉 `&& !snap.nested`**）
```

`views/browse.rs:316,466` 的 `can_edit: snap.caps_edit && !snap.nested` 改为 `can_edit: snap.caps_edit`；菜单启用条件（`submit_edit_batch_locked` 所在 `dispatch.rs`/`file_ops.rs`）同步改为读快照字段而不是重新推导。

## §6 验收

```bash
cargo test -p hzip-format -p hzip-core --test composite_edit
cargo test -p hzip-gui --lib
cargo test --workspace
```

- [ ] 形状 1：`a.raw.gz` 内改文件 ⇒ 外层 gz 可解、未改内容不变、失败/取消时原档 sha256 不变
- [ ] 形状 2：`v.zip` 内 `disk.vmdk` 改文件 ⇒ 条目更新、**同 zip 内其它条目未被重压**（字节不变）
- [ ] 外层是 tar/cpio/rar ⇒ 整链只读 + 理由键（不是静默失败、不是菜单置灰无说明）
- [ ] 嵌套会话里编辑菜单的启用/置灰与 `chain_editable` 一致（GUI 用例）
- [ ] 组合档**创建**路径（`compress.rs` 的 `composite_codec`）零变化（回归）
- [ ] `TC-CONF-009` 口径在新路径上仍成立（前后 sha256 断言）

## §7 风险 / 注意

| 点 | 处置 |
|---|---|
| 组合写 = 整层重压（`.gz` 无随机写） | 这是格式决定的固有代价，进度窗与文档如实标注；`qcow2.gz` 的编辑写回**一定**是全量重压，与写模式 A/B 无关 |
| 形状 2 的条目重压 | 用 `ChangeSet::Replace` 的既有不变式「未命中的条目无损保留」保证其它条目不重压（zip/7z 编辑器已实现该不变式） |
| 中间层临时档占盘 | 每层一个临时档，逐层用完即删；命名沿用 `{name}.hzip-tmp-{uuid}`（同目录，保证原子替换是同卷 rename） |
| 组合层没有 `write_in_place` ⇒ 写模式开关在组合链上「只影响内层暂存」 | 文档与设置项提示必须说清：**被包裹的镜像**提交时必然重写外层文件；「就地写」只对**未包裹**的镜像成立（`supports_in_place_edit()` 由 core 在会话判定里给，不在 handler 里硬写） |
| 既有 `caps_edit` 记忆化（`app/state.rs` `caps_edit_cache`） | 会话切换时按新口径重算；用例覆盖「同一路径先后以不同链打开」 |
