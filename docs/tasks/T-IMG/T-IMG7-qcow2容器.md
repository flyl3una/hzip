# T-IMG7：`hzip-fmt-qcow2`（qcow2 容器，读+写）

| 项目 | 内容 |
|---|---|
| 阶段 | P2 读写闭环 |
| 依赖 | T-IMG5（设备链）、T-IMG6（提交语义） |
| 状态 | 未开工 |
| 目标 | `.qcow2` 当作卷容器接入：读（含压缩簇/backing 链）与写（簇分配 + refcount，由上游 `rust-img-qcow2` 负责）；不能写的形态**报名字拒绝**且带理由 |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 新 | `crates/hzip-fmt-qcow2/src/{lib.rs,handler.rs,probe.rs,volume.rs}` | 探针（`QFI\xfb`）+ `Qcow2Handler` + `Qcow2Volume`（`hzip_vol::Volume`）+ reader/editor 组装 |
| 改 | `crates/hzip-format-testkit/src/vol/qcow2.rs`（`vol` feature） | 手写 qcow2 v3 构造器（无 create API ⇒ 自己拼头/L1/L2/refcount） |
| 新 | `crates/hzip-fmt-qcow2/tests/{probe.rs,read.rs,write.rs,refuse.rs,conformance.rs}` | 探针/读/写/拒写 + `TC-CONF-001..010`（testkit `run_conformance`，夹具取 `vol` 模块） |
| 改 | `crates/hzip-vol/src/lib.rs` | `VolCaps.warnings: Vec<&'static str>`（backing copy-up 体积增长这类**提示**，与「拒写理由」分开） |

## 步骤 0：前置核实

| 待核 | 依据 |
|---|---|
| `Header` 的字段名（`dirty`/`corrupt`/`crypt_method`/`incompatible_features`/`backing_file`） | <https://docs.rs/rust-img-qcow2/latest/img_qcow2/struct.Header.html> |
| `open_rw_on_device(dev)` 对「设备不可写」的行为（报错还是降级） | <https://docs.rs/rust-img-qcow2/latest/img_qcow2/struct.Qcow2Reader.html> |
| `ClusterStatus` 变体（未分配/零/已分配/压缩）与 `extents()` 迭代器形状 | 同页 `cluster_status_at` / `extents` |

## 步骤 1：写失败测试

`tests/probe.rs`：

```rust
#[test] fn magic_is_qfi_v3() { assert_eq!(qcow2_probe(b"QFI\xfb\0\0\0\x03"), Some(Confidence::High)); }
#[test] fn other_magic_is_not_qcow2() { assert_eq!(qcow2_probe(b"KDMV\0\0\0\0"), None); assert_eq!(qcow2_probe(&[0u8; 32]), None); }
#[test] fn head_bytes_covers_v3_header() { assert_eq!(Qcow2Handler::new().probe_head_bytes(), 512); }
```

`tests/read.rs`：

```rust
#[test]
fn lists_partition_layer_and_fs_tree() {
    let img = testkit::vol::qcow2_image(&testkit::vol::raw_with_mbr(FsKind::Ext4, &[("/etc/fstab", b"x\n")]));
    /* 打开 ⇒ "1. …(ext4)/" 层 + "/etc/fstab" 可读 */
}

#[test]
fn compressed_clusters_read_back() { /* 夹具用 zlib 簇构造一版 ⇒ 内容一致（上游解压路径） */ }

#[test]
fn sparse_clusters_read_as_zeros() { /* 未分配簇读回 0；且 `cluster_status_at` 反映未分配 */ }

#[test]
fn backing_chain_reads_parent() {
    /* 造 child.allocated=父 + child.dirty 未分配 ⇒ 读 child 得到父的内容（上游 copy-on-read） */
}
```

`tests/write.rs`：

```rust
#[test]
fn write_allocates_cluster_and_survives_qemu_img_check() {
    let img = testkit::vol::qcow2_image(&testkit::vol::raw_with_mbr(FsKind::Ext4, &[("/a.txt", b"old")]));
    let before = img.len();
    /* 打开 r/w → apply(Replace) → flush → 落盘字节 */
    let after = read_back_bytes();
    assert!(after.len() >= before, "分配新簇 ⇒ 容器只会变大");
    assert_eq!(extract_file(&after, "/a.txt"), b"new");       // 用 hzip 自身再打开复核
    #[cfg(feature = "oracle")] qemu_img_check(&after);        // 开发机有 qemu-img 时（不进 CI 硬门）
}

#[test]
fn unallocated_write_allocates_and_updates_refcounts() {
    /* 写一段全零（未分配）区域 ⇒ 该段读回即写值；镜像内出现新分配簇（refcount 表增长不 panic） */
}

#[test]
fn overlay_staging_keeps_source_untouched_then_commits() {
    /* 模式 B：apply 后原文件 sha256 不变；commit 后变化且可读回（T-IMG6 语义在 qcow2 上的落地） */
}
```

`tests/refuse.rs`：

```rust
#[test] fn dirty_image_is_read_only_with_reason()        { /* header.dirty ⇒ caps.read_only_reason == Dirty，write ⇒ 报错 */ }
#[test] fn encrypted_image_is_read_only_with_reason()    { /* crypt_method != 0 ⇒ Encrypted */ }
#[test] fn external_data_file_is_refused_by_name()       { /* incompatible feature ⇒ Unsupported("vol.unsupported.external-data") */ }
#[test] fn extended_l2_is_refused_by_name()              { /* ⇒ Unsupported("vol.unsupported.ext-l2") */ }
#[test] fn backing_chain_warns_about_growth_on_write()   { /* caps.warnings 含 "vol.warn.backing-copyup"，但仍可写 */ }
```

## 步骤 2：实现

### 2.1 探针（`src/probe.rs`）

```rust
const QCOW2_MAGIC: [u8; 4] = [0x51, 0x46, 0x49, 0xFB];   // "QFI\xfb"

/// 魔数 + 版本合理性（版本 ∈ {2,3}）。不做「无魔数」猜测：qcow2 一定带魔数。
pub fn qcow2_probe(head: &[u8]) -> Option<Confidence> {
    (head.len() >= 8 && head[..4] == QCOW2_MAGIC && matches!(be32(&head[4..8]), 2 | 3)).then_some(Confidence::High)
}
```

> 注意：qcow2 头是大端（规格如此），别用 LE 读——这一条写进注释防回归。

### 2.2 `Qcow2Volume`（`src/volume.rs`）

```rust
pub struct Qcow2Volume { dev: Arc<dyn BlockDevice>, reader: img_qcow2::Qcow2Reader, parts: Vec<PartitionView>, warnings: Vec<&'static str> }

impl Volume for Qcow2Volume {
    fn geometry(&self) -> Geometry {
        Geometry { virtual_size: self.reader.virtual_size(), sector_size: 512,
                   table: /* partition 层探测结果 */, is_sparse: true, has_backing: self.reader.has_backing() }
    }
    fn partitions(&self) -> VolResult<Vec<PartitionView>> { Ok(self.parts.clone()) }
    fn open_fs(&self, part: Option<PartitionId>) -> VolResult<Option<Box<dyn FsVolume>>> { hzip_vol::registry::mount_best(&self.dev, part) }
    fn caps(&self) -> VolCaps { VolCaps { writable: self.reader.is_writable(), in_place: true, read_only_reason: self.reason, warnings: self.warnings.clone() } }
    fn flush(&mut self) -> VolResult<()> { self.reader.flush()?; Ok(()) }
}
```

打开时的判定序（**顺序即优先级**，全部落在 `open` 里、不留模糊）：

```rust
let reader = if opts.writable { Qcow2Reader::open_rw_on_device(dev.clone()) } else { Qcow2Reader::open_on_device(dev.clone()) };
// ① incompatible_features & EXTERNAL_DATA_FILE ⇒ Err Unsupported("vol.unsupported.external-data")
// ② incompatible_features & EXTENDED_L2      ⇒ Err Unsupported("vol.unsupported.ext-l2")
// ③ header.crypt_method != 0                 ⇒ 只读 + Encrypted
// ④ header.dirty 或 corrupt                  ⇒ 只读 + Dirty（**不给清标记入口**）
// ⑤ has_backing()                            ⇒ 可写 + warnings.push("vol.warn.backing-copyup")
```

### 2.3 夹具（`hzip-format-testkit/src/vol/qcow2.rs`）

```rust
/// 把「原始镜像字节」包成一个合法 qcow2 v3（未压缩、全部簇已分配）。
/// 布局：header(104B) → refcount table(1 簇) → refcount blocks → L1 → L2 → 数据簇。
pub fn qcow2_image(raw: &[u8], cluster_bits: u32) -> Vec<u8>;
/// 稀疏变体：`holes` 列出的虚拟簇标为未分配（读回 0）。
pub fn qcow2_image_sparse(raw: &[u8], cluster_bits: u32, holes: &[u64]) -> Vec<u8>;
/// zlib 压缩簇变体（读路径专测；写侧不依赖）。
pub fn qcow2_image_zlib(raw: &[u8], cluster_bits: u32) -> Vec<u8>;
/// backing 链：父 = 已分配簇，子 = 同尺寸但部分簇未分配（读子得到父数据）。
pub fn qcow2_with_backing(parent: &[u8], cluster_bits: u32) -> (Vec<u8> /*parent*/, Vec<u8> /*child*/);
```

要点：refcount 表项按 `cluster_size / 8` 分块、**大端**写；`l1_size = ceil(virtual_size / (cluster_size * l2_entries))`。这些口径写进构造器注释，测试里对 `virtual_size()`/`cluster_size()` 各有一条断言。

## 步骤 3：跑测试与验收

```bash
cargo test -p hzip-fmt-qcow2
cargo test -p hzip-format-testkit --features vol
cargo clippy --workspace --all-targets -- -D warnings
# 开发机（可选，不进 CI）：
qemu-img check <落盘后的 qcow2>
```

### 步骤 4：提交

```bash
git add crates/hzip-fmt-qcow2 crates/hzip-format-testkit
git commit -m "feat(vol): qcow2 容器——读写接入与拒写理由"
```

## 验收

- [ ] 探针只认 `QFI\xfb` + 版本 2/3（大端），其它一律不认
- [ ] 读：未压缩/稀疏/压缩簇/backing 链四条路径各有专测
- [ ] 写：分配新簇、写未分配区、refcount 表增长都不 panic，写后可被 hzip 重新打开读回
- [ ] 拒写四类（dirty / 加密 / 外部数据文件 / 扩展 L2）各自带**稳定文案键**
- [ ] backing 链可写但带 `vol.warn.backing-copyup` 提示（用户知道体积会涨）
- [ ] 模式 B 下原档在提交前不变（与 T-IMG6 同一断言口径）

## 风险 / 注意

| 点 | 处置 |
|---|---|
| 手写夹具写成「看起来对」的假 qcow2 | 夹具专测：自造镜像必须能被 `Qcow2Reader::open_on_device` 打开、`virtual_size()/cluster_size()` 与构造参数一致；开发机再用 `qemu-img info/check` 复核一次 |
| qcow2 元数据写在**宿主偏移**上（L1/L2/refcount），覆盖层因此必须挂在 qcow2 **下面** | 这正是设备链顺序（`FileDevice → OverlayDevice → Qcow2Reader`）；测试用「镜像文件被写坏的宿主簇只有 L2/refcount 与数据簇」来反证顺序正确 |
| dirty/corrupt 镜像 | 只读 + 理由；**不做修复**（超出工具职责，误修代价极高） |
| 大变更导致 refcount 增长缓慢 | `cluster_status_at` 可用于**跳过全零簇**的加速复制（T-IMG13 视需要） |

## §落地记录（2026-10-16）

**已完成**（子代理交付，**24 条**用例：probe 4 / read 6 / refuse 9 / write 5；clippy 干净）：探针（大端 `QFI\xfb` + 版本 2/3）、`Qcow2Volume`（判定序全在 `open` 里）、`Qcow2Handler` 双实现；测试含**提交前原档 sha256 与头部块逐字节不变**、未分配簇读回 0（L2 挖洞与末尾 tail 两条）、脏/加密/外部数据文件/扩展 L2/backing 的稳定键。

**偏离（均有依据）**：
1. **backing 链 ⇒ 按名字拒绝**（`vol.unsupported.backing-file`），而非计划里的「可写 + `vol.warn.backing-copyup` 提示」：`VolumeHandler::open` 只拿到 `Arc<dyn BlockDevice>`，`Chain` 没有路径访问器，而上游 `open_*_on_device` 在无父路径时**拒解析父镜像** ⇒ 未分配簇会**静默读 0**（错数据）。宁可报名字。**后续**：给 `Chain` 加源路径访问器 + handler 走 `Qcow2Reader::open(path)`（能解析 backing）即可启用该形态。
2. **加密 ⇒ 拒绝**（`vol.readonly.encrypted`）：上游连**读**路径都拒（无解密实现）⇒ 给不出「能读的只读卷」。
3. 夹具放本 crate `tests/common/`（计划写的是 testkit `vol/qcow2.rs`；本轮约定夹具归各自 crate）。
4. 未加 `tests/conformance.rs`：testkit 的 `run_conformance` 由 `HZIPSTUB` 桩档案驱动，真实 handler 必拒（`hzip-fmt-single` 已把该缺陷记为 skip）；`hzip-fmt-raw` 同。
5. `open_rw_on_device` 的条件多一项 `dev.is_writable()`（浏览路径恒给只读链；只看 `opts.writable` 会让 `open_reader` 直接失败）。
