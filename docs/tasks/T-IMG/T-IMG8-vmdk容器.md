# T-IMG8：`hzip-fmt-vmdk`（VMDK 容器，sparse 读写 / 其余拒写）

| 项目 | 内容 |
|---|---|
| 阶段 | P2 读写闭环 |
| 依赖 | T-IMG5（设备链）、T-IMG6（提交语义） |
| 状态 | 未开工 |
| 目标 | `.vmdk` 接入：`monolithicSparse` 读+写、`streamOptimized` 只读、其余布局**按 create type 报名字拒绝**（绝不误读） |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 新 | `crates/hzip-fmt-vmdk/src/{lib.rs,handler.rs,probe.rs,volume.rs}` | 探针（`KDMV`）+ `VmdkHandler` + `VmdkVolume` |
| 改 | `crates/hzip-format-testkit/src/vol/vmdk.rs`（`vol` feature） | 手写 monolithicSparse 构造器（头 + descriptor + RGD/GD/GT + grain） |
| 新 | `crates/hzip-fmt-vmdk/tests/{probe.rs,read.rs,write.rs,refuse.rs,conformance.rs}` | 四组 + `TC-CONF-001..010`（testkit `run_conformance`） |
| 改 | `crates/hzip-vol/src/error.rs` | 新增理由键：`vol.readonly.layout`、`vol.readonly.dirty`（`uncleanShutdown`）、`vol.unsupported.multi-extent` |

## 步骤 0：前置核实

| 待核 | 依据 |
|---|---|
| `SparseHeader` 字段名（`capacity`/`grainSize`/`numGTEsPerGT`/`gdOffset`/`rgdOffset`/`flags`） | <https://docs.rs/rust-img-vmdk/latest/img_vmdk/struct.SparseHeader.html> |
| `Descriptor` 暴露的 create type / `uncleanShutdown` / `parentFileNameHint` | <https://docs.rs/rust-img-vmdk/latest/img_vmdk/struct.Descriptor.html> |
| `open_rw_on_device` 对非 sparse 布局的失败形态（错误变体） | <https://docs.rs/rust-img-vmdk/latest/img_vmdk/struct.VmdkReader.html> |

## 步骤 1：写失败测试

`tests/probe.rs`：

```rust
#[test] fn magic_is_kdmv() { assert_eq!(vmdk_probe(b"KDMV\x01\0\0\0"), Some(Confidence::High)); }
#[test] fn vhd_footer_or_other_is_not_vmdk() { assert_eq!(vmdk_probe(b"conectix"), None); assert_eq!(vmdk_probe(&[0u8; 64]), None); }
```

`tests/read.rs`：

```rust
#[test] fn sparse_image_lists_partitions_and_tree() { /* testkit::vol::vmdk_sparse(&testkit::vol::raw_with_mbr(FsKind::Ext4, …)) */ }

#[test] fn stream_optimized_reads_but_is_read_only() {
    let img = testkit::vol::vmdk_stream_optimized(&raw);
    /* 读 OK；caps.writable = false；caps.read_only_reason == Some(ReadOnlyReason::Layout) */
}

#[test] fn grain_holes_read_as_zeros() { /* 未分配 grain 读回 0（上游语义），不报错 */ }
```

`tests/write.rs`：

```rust
#[test] fn write_allocates_grain_and_keeps_redundant_gd_in_step() {
    let img = testkit::vol::vmdk_sparse(&testkit::vol::raw_with_mbr(FsKind::Ext4, &[("/a.txt", b"old")]));
    /* r/w 打开 → apply(Replace) → flush → 落盘 */
    /* 断言：① 重新打开读回新内容；② 冗余 GD（rgdOffset）与主 GD 一致；
             ③ header 的 `uncleanShutdown` 在正常关闭后为 0；④ content id 已更新（上游负责，只断言一致/非零） */
}

#[test] fn overlay_staging_keeps_source_untouched_then_commits() { /* 同 T-IMG7 的断言口径 */ }
```

`tests/refuse.rs`：

```rust
#[test] fn monolithic_flat_is_refused_by_name()      { /* descriptor createType="monolithicFlat" ⇒ Unsupported("vol.unsupported.layout-flat") */ }
#[test] fn two_gb_max_extent_is_refused_by_name()    { /* twoGbMaxExtentSparse/Flat ⇒ 同上（多 extent） */ }
#[test] fn vmfs_sparse_cowd_is_refused_by_name()     { /* 0x434F5744 ("COWD") ⇒ Unsupported("vol.unsupported.layout-vmfs") */ }
#[test] fn snapshot_delta_needs_parent()            { /* parentFileNameHint 非空 ⇒ 只读 + reason Layout（或可写？见 §2.3 策略） */ }
#[test] fn unclean_shutdown_is_reported()           { /* header flags 置位 ⇒ 读可，写 ⇒ 只读 + Dirty */ }
```

## 步骤 2：实现

### 2.1 探针（`src/probe.rs`）

```rust
const VMDK_SPARSE_MAGIC: u32 = 0x564D_444B;      // 'K''D''M''V'（小端读文件头前 4 字节）

/// 只认 sparse extent 头魔数（hosted sparse 家族必带）。
/// flat 布局没有该头 ⇒ 由**扩展名回退**进来后再由 descriptor 判定并拒绝（不猜）。
pub fn vmdk_probe(head: &[u8]) -> Option<Confidence> {
    (head.len() >= 512 && u32::from_le_bytes(head[..4].try_into().unwrap()) == VMDK_SPARSE_MAGIC)
        .then_some(Confidence::High)
}
```

> 与 qcow2 相反，VMDK 头是**小端**。两句注释都写死，防「顺手改字节序」。

### 2.2 `VmdkVolume`（`src/volume.rs`）

```rust
pub struct VmdkVolume { dev: Arc<dyn BlockDevice>, reader: img_vmdk::VmdkReader, parts: Vec<PartitionView>, reason: Option<ReadOnlyReason> }

impl Volume for VmdkVolume {
    fn geometry(&self) -> Geometry {
        Geometry { virtual_size: self.reader.virtual_size(), sector_size: 512, table: …, is_sparse: true,
                   has_backing: self.reader.descriptor().parent_file_name_hint().is_some() }
    }
    /* partitions / open_fs / flush —— 与 raw/qcow2 同款（分区探测与挂载全是共用件） */
    fn caps(&self) -> VolCaps { VolCaps { writable: self.reader.is_writable(), in_place: true, read_only_reason: self.reason, warnings: vec![] } }
}
```

### 2.3 打开判定序（**先判布局再要写权限**，把「误读」堵死）

```rust
let layout = classify_layout(&header, &descriptor)?;          // 只看头 + 描述符，不碰数据
match layout {
    Layout::MonolithicSparse   => { /* r/w 按 opts.writable */ }
    Layout::StreamOptimized    => { reason = Some(ReadOnlyReason::Layout); /* 只读打开 */ }
    Layout::MonolithicFlat | Layout::TwoGbMaxExtent { .. } => {
        return Err(VolError::Unsupported("vol.unsupported.layout-flat"))
    }
    Layout::VmfsSparse         => return Err(VolError::Unsupported("vol.unsupported.layout-vmfs")),
    Layout::SnapshotDelta      => { /* 有父 ⇒ 只读（读侧靠上游 copy-on-read）；写 ⇒ 拒绝并说明「需先合并快照」 */ }
}
if header.flags & UNCLEAN_SHUTDOWN != 0 && writable { reason = Some(ReadOnlyReason::Dirty); }
```

`classify_layout` 是**纯函数**（头 + 描述符文本 → 枚举），独立单测覆盖每个 create type 字符串与魔数分支（含大小写与空 descriptor）。

### 2.4 夹具（`hzip-format-testkit/src/vol/vmdk.rs`）

```rust
/// monolithicSparse：头(512B, 小端) + descriptor 文本 + RGD + GD + GT + grain 数据。
/// `raw` 必须 ≤ 容量；全部 grain 已分配（稀疏变体见 `holes`）。
pub fn vmdk_sparse(raw: &[u8]) -> Vec<u8>;
pub fn vmdk_sparse_with_holes(raw: &[u8], holes: &[u64]) -> Vec<u8>;      // 未分配 grain 读回 0
pub fn vmdk_stream_optimized(raw: &[u8]) -> Vec<u8>;                      // DEFLATE grain（只读专测）
pub fn vmdk_flat_descriptor_only(capacity: u64) -> Vec<u8>;               // 只有 descriptor 文本（拒写专测）
```

要点：`grainSize` 取 128（扇区数 ⇒ 64 KiB）、`numGTEsPerGT` 512、`gdOffset`/`rgdOffset` 各一簇；descriptor 的 `createType` 与 `parentFileNameHint` 由参数控制（拒写用例靠它）。

## 步骤 3：跑测试与验收

```bash
cargo test -p hzip-fmt-vmdk
cargo test -p hzip-format-testkit --features vol
# 开发机（可选，不进 CI）：qemu-img info/check
```

### 步骤 4：提交

```bash
git add crates/hzip-fmt-vmdk crates/hzip-format-testkit crates/hzip-vol/src/error.rs
git commit -m "feat(vol): vmdk 容器——sparse 读写、其余布局报名字拒绝"
```

## 验收

- [ ] 探针只认 `KDMV`（小端），flat 布局走扩展名回退后**由 descriptor 判定并拒绝**
- [ ] `monolithicSparse`：读+写；写后重新打开读回正确、冗余 GD 与主 GD 一致
- [ ] `streamOptimized`：读可用、`caps.writable = false` 且理由是 `Layout`
- [ ] `monolithicFlat` / `twoGbMaxExtent*` / `vmfsSparse` / 快照 delta：各自**稳定文案键**，且**不读数据区就拒绝**（用 `CountingDevice` 断言读量）
- [ ] `uncleanShutdown` 置位 ⇒ 读可、写拒（理由是 `Dirty`）
- [ ] 模式 B 提交语义与前两个容器一致（同一断言口径）

## 风险 / 注意

| 点 | 处置 |
|---|---|
| `vmdk.zip`（用户示例）里的 vmdk 通常来自 OVA 导出 ⇒ **streamOptimized，只读** | 能力矩阵如实标注；错误文案直接给出路：「该 VMDK 为 streamOptimized（OVA 导出形态），只读；如需修改请先用 `qemu-img convert -O vmdk -o subformat=monolithicSparse` 转换」（二期可考虑自带「另存为 sparse」） |
| 多 extent 文件（`-s 2Gb` 切分） | 本期拒绝（`vol.unsupported.multi-extent`）；不要试图只读第一个 extent |
| 描述符文本编码/换行差异（CRLF/LF、大小写） | `classify_layout` 用例覆盖；解析失败 ⇒ 报 `vol.err.descriptor`，不猜 |
| 冗余 GD 与主 GD 不一致（第三方工具产物） | 上游负责维护；我们只断言**自己写完**的一致性，不改他人镜像 |
