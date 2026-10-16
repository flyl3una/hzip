# T-IMG4：core 注册 + 扩展名表 + CLI 只读闭环（里程碑 M1）

| 项目 | 内容 |
|---|---|
| 阶段 | P1 只读闭环（用户三个示例的「能打开预览目录结构」在此达标；**不单独发版**，随 T-IMG5-10 一起发） |
| 依赖 | T-IMG3 |
| 状态 | **已实施（core 侧）**（2026-10-16）：扩展名表 `ExtKind::Volume` + raw/qcow2/vmdk 族、平台 MIME 表（互锁测试通过）、`hzip-core` 的 `volume-formats` feature + `FsRegistry`（ext4/FAT）+ `RawHandler` 注册；**引擎级端到端用例** `crates/hzip-core/tests/volume.rs` 6 条全绿（分区行/卷内树/读文件/GPT 标签/裸卷/内容探针）。CLI `l`/`x` 的 e2e 用例与 qcow2/vmdk 注册待各自任务收口 |
| 目标 | 把 raw+ext4 接进 hzip 的正常链路：嗅探认得出、扩展名表认得出、core 注册得到、CLI 列得/取得出 |

## 交付物

| 动作 | 文件 | 改什么 |
|---|---|---|
| 改 | `crates/hzip-format/src/exts.rs` | `ExtKind::Volume`（新分组「磁盘镜像」）+ `FORMAT_EXTS` 加 raw 族（含组合写法，照 cpio 先例全列） |
| 改 | `crates/hzip-platform/src/assoc/mime.rs` | Linux MIME 表补 `raw`/`img`/`dd`（**必做**：`hzip-gui` 的 `ui/tests/contracts.rs::assoc_mime_consistency::every_association_candidate_has_a_linux_mime` 会在新候选出现时变红——该测试正是为此存在） |
| 新 | `crates/hzip-fmt-raw/tests/conformance.rs` | `TC-CONF-001..010` 真实档案重驱动（`hzip-format-testkit` 的 `KitCtx`/`run_conformance`；夹具取 `vol` 模块） |
| 改 | `crates/hzip-core/src/engine/core.rs:167` | `register_builtin_handlers` 加 `hzip_fmt_raw::RawHandler`（`feature = "volume-formats"` 门控） |
| 改 | `crates/hzip-core/Cargo.toml` | 新 feature `volume-formats`（进 `default` 组合）；依赖 `hzip-fmt-raw` |
| 改 | `crates/hzip-format/src/exts.rs` 的测试 | 表驱动用例：`raw`/`img`/`dd` 可判定、`a.raw.gz` 能推出内层层名 |
| 新 | `crates/hzip-core/tests/volume.rs` | browse 树 + 提取 + 卷层断言（用 testkit 夹具） |
| 改 | `crates/hzip-cli/tests/e2e.rs` | `tc_cli_volume_l/x/t`（照 cpio 用例结构） |

## 步骤 1：写失败测试

`crates/hzip-core/tests/volume.rs`：

```rust
#[test]
fn browse_shows_partition_layer_then_fs_tree() {
    let img = testkit::vol::raw_with_mbr(FsKind::Fat32, &[("/EFI/BOOT/BOOTX64.EFI", b"EFI")]);
    // 或 ext4 版：raw_with_mbr(FsKind::Ext4, &[("/etc/fstab", b"…")])
    let tmp = write_temp("disk.img", &img);
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let b = engine.open(&tmp, &OpenOptions::default()).unwrap();
    let names: Vec<String> = b.entries().iter().map(|e| e.name.as_str().to_string()).collect();
    assert!(names.iter().any(|n| n.starts_with("1. ") && n.ends_with('/')), "分区行在（{names:?}）");
    assert!(names.iter().any(|n| n.starts_with("1. ") && n.contains("/EFI/BOOT/BOOTX64.EFI")));
}

#[test]
fn extract_writes_expected_bytes() {
    /* 提取 /EFI/BOOT/BOOTX64.EFI → sha256 与夹具一致 */
}

#[test]
fn sniff_picks_raw_by_probe_and_by_extension() {
    /* ① 内容判定：无扩展名（或 .bin）的 MBR 镜像也能开（probe）；② 只有扩展名命中（.raw 但内容不认）仍可开（Low） */
}
```

`crates/hzip-cli/tests/e2e.rs`：

```rust
#[test] fn tc_cli_volume_l()  { /* hzip l disk.img：输出含分区行与卷内文件行 */ }
#[test] fn tc_cli_volume_x()  { /* hzip x disk.img -o out：落盘字节与夹具一致 */ }
#[test] fn tc_cli_volume_t()  { /* hzip t disk.img：走 audit，退出码 0；脏卷夹具 ⇒ 非 0 且有说明 */ }
```

## 步骤 2：实现

### 2.1 扩展名表（`hzip-format/src/exts.rs`）

```rust
pub enum ExtKind {
    Archive,
    TarComposite,
    Stream,
    /// 磁盘镜像（含卷内文件系统的容器；`raw`/`qcow2`/`vmdk` 族）。
    /// 组合写法（`raw.gz`/`qcow2.gz`）同归此档——文件关联页只想要一个「磁盘镜像」分组。
    Volume,
}
```

`FORMAT_EXTS` 追加（**照 cpio 先例把组合写法全列**：嗅探按内容装配、UI 若漏列就出现「引擎解得开、菜单/类型列不认」的自相矛盾）：

```rust
    FormatExts { format: "raw", container: Some("raw"), kind: ExtKind::Volume, exts: &["raw", "img", "dd"] },
    FormatExts { format: "raw.gz",  container: Some("raw"), kind: ExtKind::Volume, exts: &["raw.gz"]  },
    FormatExts { format: "raw.bz2", container: Some("raw"), kind: ExtKind::Volume, exts: &["raw.bz2"] },
    FormatExts { format: "raw.xz",  container: Some("raw"), kind: ExtKind::Volume, exts: &["raw.xz"]  },
    FormatExts { format: "raw.zst", container: Some("raw"), kind: ExtKind::Volume, exts: &["raw.zst"] },
```

> `a.raw.tgz` / `a.raw.tar.gz` 无需额外行：内层层名由 `composite_inner_name` 逐层剥出（`a.raw.tgz` → `a.raw.tar`），进入 tar 后 `a.raw` 是**真条目**，双击即按扩展名进 raw 容器（既有 `RowAction::EnterContainer` 路由）。用例钉死这条链（exts 测试 + core 集成各一条）。

**同时必须补 `hzip-platform` 的 MIME 表**（`crates/hzip-platform/src/assoc/mime.rs`）：该 crate 不依赖 `hzip-format`（红线 4），两张表的一致性由 `hzip-gui/src/ui/tests/contracts.rs::assoc_mime_consistency` 互锁——「凡出现在文件关联候选里的单点扩展名，必须能查到 MIME」。取值的做法：**逐个用规范源核对**（`xdg-mime query filetype` 或 freedesktop `shared-mime-info` 的 `freedesktop.org.xml`），查不到就填 `application/octet-stream` 并在注释里写明理由（该表只用于 Linux 的 `.desktop` `MimeType=` 与 `mimeapps.list` 键，宁可保守也不要编一个不存在的 MIME）。组合写法（`raw.gz`）不做关联候选，故不进 MIME 表。

`ExtKind` 相关 `match` 全部编译期暴露（加变体即报错）；文件关联页分组按 `Volume` 新增标题「磁盘镜像」（locale 键 T-IMG10 补）。

### 2.2 core 注册（`crates/hzip-core/src/engine/core.rs:167`）

```rust
    #[cfg(feature = "default-formats")]
    {
        reg.register_handler(Box::new(hzip_fmt_tar::TarHandler::new()))?;
        /* …既有… */
        #[cfg(feature = "volume-formats")]
        {
            reg.register_handler(Box::new(hzip_fmt_raw::RawHandler::new()))?;
        }
    }
```

`crates/hzip-core/Cargo.toml`：

```toml
[features]
default = ["default-formats", "volume-formats"]
volume-formats = ["dep:hzip-fmt-raw"]        # 后续：hzip-fmt-qcow2 / hzip-fmt-vmdk / hzip-fs-*
```

### 2.3 不需要改的地方（**明确记录，防止「顺手改」破坏既有链路**）

| 位置 | 为什么不用改 |
|---|---|
| `crates/hzip-core/src/services/browse.rs:282-331`（`Browser::enter`） | 按扩展名选 handler；`raw` 进表即可用（组合/嵌套两态都不碰） |
| `crates/hzip-format/src/registry.rs` 组合装配 | `raw.gz` 的内层就是我们要的字节流；`Chain` 自己物化（T-IMG2） |
| `hzip-gui` | `supported_open_extensions`（`ui/contracts.rs:698`）、`is_container_name`（`viewmodel/main_vm.rs:678`）、类型列（`ui/binding.rs:73`）**全部表驱动** ⇒ 自动跟随 |
| `hzip-cli` | `l`/`x`/`t` 对 `ArchiveReader` 泛化 ⇒ 自动可用 |

## 步骤 3：跑测试与验收

```bash
cargo test -p hzip-format -p hzip-core --test volume -p hzip-cli --test e2e volume
cargo test --workspace
# 手工（M1 观感验收）：
hzip l <测试镜像>
hzip x <测试镜像> -o out
```

### 步骤 4：提交

```bash
git add crates/hzip-format/src/exts.rs crates/hzip-core crates/hzip-cli/tests
git commit -m "feat(core): 接入 raw 卷容器——扩展名表新分组、注册、CLI 只读闭环（M1）"
```

## 验收

- [ ] `hzip l disk.img`：分区层 + 卷内树，两态（有表/无表）都对
- [ ] `hzip x disk.img -o out`：字节与夹具一致；选择/冲突策略/净化沿用既有行为
- [ ] `hzip t disk.img`：干净镜像退出码 0；脏卷（夹具造）非 0 且给出理由键
- [ ] 「无扩展名但内容像镜像」的文件也能打开（probe 路径）；「.raw 但内容不认」按扩展名仍能开（Low）
- [ ] `a.raw.tgz` 三层：`a.raw.tar`（组合层）→ tar 内 `a.raw` 条目 → 双击进容器
- [ ] 既有格式（zip/7z/tar/cpio/rar）行为零变化：`cargo test --workspace` 全绿

## 风险 / 注意

| 点 | 处置 |
|---|---|
| 弹「按扩展名识别」的误开 | 打开失败必须给**明确理由**（`Confidence::Low` 时 UI 顶部提示「按扩展名识别」，T-IMG10） |
| `p.tgz` 与 `raw` 组合层的双重下钻 | 一层一条用例钉死（`a.raw.tgz` → `a.raw.tar` → `a.raw`），防「双击无反应」历史缺陷重演 |
| feature 裁剪 | `volume-formats` 关掉时：表仍在（UI 广告）但 handler 未注册 ⇒ 与既有「表 ∩ 已注册」口径一致（`ui/contracts.rs:698` 已按注册表过滤，不会广告不可用格式） |
| `container: Some("raw")` 的 `container_handler` 误判普通文件 | `container_handler` 只按扩展名判定；`raw/img/dd` 是很强的镜像信号，误判代价仅是「双击后报格式错」，可接受 |
