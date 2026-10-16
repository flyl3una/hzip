# T-IMG10：界面与 CLI 完善（里程碑 M3 = 发版门槛）

| 项目 | 内容 |
|---|---|
| 阶段 | P3 集成（**M3**：用户判定「一起发」的那一版由本任务收口） |
| 依赖 | T-IMG9 |
| 状态 | **部分实施**（2026-10-16）：分区行已落地——`TreeRow.volume`/`BrowseRow.is_partition` + `MainViewModel::volume_row_named`（core 经 `Browser::volume_rows()` 提供结构化元数据）；展示层：**硬盘图标**（`AppIcon::HardDrive`）、**类型列显示文件系统**（未识别 ⇒「未知卷」）、**无标签时按语言显示「分区 N」**（路径保持 ASCII 稳定）；locale 键 `vol.partition_n`/`vol.volume`/`vol.unknown_fs`；GUI lib 532 条全绿。待做：只读徽标与理由 tooltip、CLI 写侧动词、文案键单源化校验、`Confidence::Low` 顶栏提示 |
| 目标 | 把卷层「看得见、说得清、改得动」补齐：分区行/类型列/只读理由/进度；CLI 写侧动词与 JSON；文案单源化 |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 改 | `crates/hzip-format/src/exts.rs` | `FORMAT_EXTS` 增 qcow2 / vmdk（含 `.qcow`、组合写法四件套） |
| 改 | `crates/hzip-platform/src/assoc/mime.rs` | Linux MIME 表补 `qcow2`/`qcow`/`vmdk`（同 T-IMG4 的口径：与 `FORMAT_EXTS` 的互锁测试会逼你做；取值逐个用规范源核对，查不到用 `application/octet-stream`） |
| 改 | `crates/hzip-gui/src/ui/binding.rs:230` | 类型列：**有 `container_format` 就用它**（分区行据此显示 `ext4 卷`/`FAT32 卷`，而 `is_container=false` ⇒ 图标仍是文件夹） |
| 改 | `crates/hzip-gui/src/app/state.rs`（`browse_row_from_tree_row`） | 分区行 `container_format = Some(fs_id)`；`volume_kind`（分区/裸卷）随行携带 |
| 改 | `crates/hzip-gui/src/ui/views/browse.rs` + `ui/bar/toolbar.rs` | 只读徽标（锁）+ tooltip = 理由文案；`Confidence::Low` 时顶栏提示「按扩展名识别」 |
| 改 | `crates/hzip-gui/locales/{zh-CN,en}.toml` | 新增文案键（§2.3 清单） |
| 改 | `crates/hzip-gui/src/ui/views/settings_render.rs` | 文件关联页新分组「磁盘镜像」标题（`ExtKind::Volume`） |
| 改 | `crates/hzip-cli/src/{ops.rs,cli.rs,json_schema.rs}` | `a`/`u` 支持 `--to <卷内路径>`；`t` 对卷映射为 audit；JSON 增卷字段 |
| 新 | `crates/hzip-vol/tests/i18n_keys.rs` | **每个 `ReadOnlyReason`/拒绝键都在两种语言里有文案**（表驱动，防漏译） |
| 新 | `crates/hzip-cli/tests/e2e.rs` | `tc_cli_volume_add/rm/mv_/t_json` |

## 步骤 1：写失败测试

`crates/hzip-vol/tests/i18n_keys.rs`：

```rust
#[test]
fn every_reason_key_has_both_locales() {
    for r in ReadOnlyReason::ALL {                       // 枚举提供 ALL（由宏/手写维护，加变体即编译期提醒）
        let key = r.key();
        assert!(zh_cn_has(key), "缺少中文文案：{key}");
        assert!(en_has(key), "缺少英文文案：{key}");
    }
    for k in VOL_UNSUPPORTED_KEYS { /* 同上：vol.unsupported.* / vol.warn.* / vol.err.* */ }
}
```

`crates/hzip-gui/src/ui/tests/binding.rs`：

```rust
#[test]
fn partition_row_shows_volume_type_in_type_column() {
    let row = TreeRowMeta { path: "1. EFI System (FAT32)".into(), is_dir: true, is_container: false, container_format: Some("FAT32 卷") };
    let ui = browse_row_from_tree_row(&row);
    assert_eq!(ui.type_text, "FAT32 卷", "类型列显示卷类型（不再是「文件夹」）");
    assert_eq!(ui.icon, file_icon::IconKind::Dir, "图标仍是文件夹（分区不是压缩包）");
}

#[test]
fn readonly_badge_carries_the_reason_key() {
    /* snap.caps_edit = false + reason = Dirty ⇒ 徽标在场；tooltip 文本 == tr("vol.readonly.dirty") */
}
```

`crates/hzip-cli/tests/e2e.rs`：

```rust
#[test] fn tc_cli_volume_add()    { /* hzip a disk.img ./note.txt --to /etc/note.txt ⇒ 重开可见 */ }
#[test] fn tc_cli_volume_rm()     { /* hzip rm disk.img /etc/note.txt ⇒ 重开消失；目录非空时报错且不改 */ }
#[test] fn tc_cli_volume_mv()     { /* hzip mv disk.img /etc/note.txt /etc/renamed.txt */ }
#[test] fn tc_cli_volume_t_json() { /* hzip t -j disk.img ⇒ { "fs": "ext4", "clean": true, "partitions": [...] } */ }
#[test] fn tc_cli_volume_readonly_mode() { /* --volume-write=readonly ⇒ 写类动词报 HZ-5001 且退出码非 0 */ }
```

## 步骤 2：实现

### 2.1 扩展名表补 qcow2 / vmdk

```rust
    FormatExts { format: "qcow2", container: Some("qcow2"), kind: ExtKind::Volume, exts: &["qcow2", "qcow"] },
    FormatExts { format: "qcow2.gz",  container: Some("qcow2"), kind: ExtKind::Volume, exts: &["qcow2.gz"]  },
    FormatExts { format: "qcow2.xz",  container: Some("qcow2"), kind: ExtKind::Volume, exts: &["qcow2.xz"]  },
    FormatExts { format: "vmdk", container: Some("vmdk"), kind: ExtKind::Volume, exts: &["vmdk"] },
    FormatExts { format: "vmdk.gz", container: Some("vmdk"), kind: ExtKind::Volume, exts: &["vmdk.gz"] },
```

### 2.2 分区行的呈现（GUI）

| 形态 | `is_dir` | `is_container` | `container_format` | 类型列 | 图标 |
|---|---|---|---|---|---|
| 镜像文件（在别处列表里） | false | true | `qcow2` | `qcow2` | `FolderZip` |
| 分区行（会话根） | true | false | `ext4 卷` | `ext4 卷` | `Folder` |
| 未识别卷 | true | false | `未知卷` | `未知卷` | `Folder`（**进得去但空**，并在右栏提示原因） |
| 无分区表的裸 FS 镜像 | — | — | — | 直接铺 FS 根（少一层） | — |

实现即三处：`state.rs` 造行时带 `container_format`；`binding.rs:230` 改为「有值就用」；`views/browse.rs` 的右栏提示位复用既有空态区。

### 2.3 文案键清单（`locales/{zh-CN,en}.toml` 各一份）

```toml
[type]
volume = "磁盘镜像"          # / "Disk image"
volume_partition = "{fs} 卷" # / "{fs} volume"
volume_unknown = "未知卷"    # / "Unknown volume"

[vol.readonly]
dirty = "卷内有未提交的变更（dirty），默认只读"
journal = "卷内有未重放的日志，默认只读；可选择先重放日志再写入"
hibernated = "卷处于休眠状态（Windows 快速启动），只读"
encrypted = "加密镜像，只读"
layout = "该镜像布局不支持写入（如 streamOptimized），只读"
fs_unknown = "未识别的文件系统，只读"
device = "目标不可写（只读介质或权限不足），已自动切到覆盖写"
by_setting = "按设置以只读方式打开"

[vol.unsupported]
external_data = "镜像使用外部数据文件，不支持"
ext_l2 = "镜像使用扩展 L2 表，不支持"
layout_flat = "该 VMDK 为 flat 布局，不支持（可用 qemu-img 转 monolithicSparse）"
layout_vmfs = "该 VMDK 为 ESXi vmfsSparse 布局，不支持"
multi_extent = "分卷 VMDK（twoGbMaxExtent）不支持"
multi_volume = "多卷归档源不支持"

[vol.warn]
backing_copyup = "该镜像有 backing 父文件：写入会把相关簇复制到本文件，体积会增长"
identify_by_extension = "按扩展名识别为磁盘镜像（内容未确认）"

[vol.err]
no_space = "镜像内空间不足"
dir_not_empty = "目录非空，未删除"
audit = "提交前自检未通过，已放弃写入（原文件未改动）"
partial_commit = "提交中断：镜像可能处于部分写入状态"
```

### 2.4 CLI

| 动词 | 卷语义 |
|---|---|
| `hzip l` | 分区层 + 卷内树（含包大小列留空、类型列 = 卷类型） |
| `hzip x -o` | 按选择提取（路径含分区前缀，如 `1. EFI System (FAT32)/EFI/…`） |
| `hzip a <img> <local...> --to <dir>` | `ChangeSet::Append`（`--to` 缺省 = 卷根） |
| `hzip rm <img> <path...>` | `ChangeSet::Delete` |
| `hzip mv <img> <from> <to>` | `ChangeSet::Rename` |
| `hzip t <img>` | `FsVolume::audit()`（退出码：0 干净 / 非 0 有问题，风格照 `fsck(8)` 子集） |
| `--volume-write=overlay\|inplace\|readonly` | 覆盖全局设置（CLI 一次性覆盖，不落盘） |

JSON（`json_schema.rs`）新增：

```json
{ "format": "qcow2", "virtual_size": 42949672960, "block_size": 65536, "backing": false, "dirty": false,
  "volume": { "table": "gpt", "partitions": [
    { "index": 1, "fs": "fat32", "start": 1048576, "length": 104857600, "writable": true,  "read_only_reason": null },
    { "index": 2, "fs": "ext4",  "start": 105906176, "length": 41838182400, "writable": true, "read_only_reason": null } ] } }
```

## 步骤 3：跑测试与验收

```bash
cargo test -p hzip-gui --lib -p hzip-cli --test e2e volume -p hzip-vol
cargo test --workspace
# 手工（M3 观感）：三种示例各截一张图
#   ① b.qcow2.gz（组合层 → 分区 → 目录）② v.zip 内 disk.vmdk（嵌套进层）③ a.raw.tgz
```

### 步骤 4：提交

```bash
git add crates/hzip-gui crates/hzip-cli crates/hzip-format/src/exts.rs crates/hzip-vol/tests
git commit -m "feat(vol): 界面与 CLI 完善——分区行/只读理由/文案单源化/CLI 写侧（M3）"
```

## 验收（M3 = 发版门槛）

- [ ] 三类示例端到端可用且观感正确（组合层冒号、嵌套进层、无魔数判定）
- [ ] 类型列分区行显示卷类型；镜像行显示容器格式；未知卷进得去、有空态原因
- [ ] 只读时：徽标在场、tooltip 给出**具体理由**、编辑类菜单置灰
- [ ] `Confidence::Low`（按扩展名识别）有一次性提示
- [ ] CLI 四个写侧动词 + `t` + `--volume-write` 三态各有 e2e
- [ ] **每个理由/拒绝/警告/错误键在中英双语都在**（表驱动单测，加变体即失败）
- [ ] `cargo test --workspace`、`clippy -D warnings`、`deps-direction.py` 全过

## 风险 / 注意

| 点 | 处置 |
|---|---|
| 类型列改为「有 `container_format` 就用」可能影响既有容器行 | 既有容器行本就走该分支（`is_container ⇒ container_format`），改动仅放宽到 `is_dir` 行；既有用例全量回归 |
| **嵌套会话的编辑启用** | 由会话的 `chain_editable` 决定（T-IMG0 §5.3 实现，本任务**只消费快照字段**）；**不要**在任何新代码里重新引入 `!snap.nested` 的硬判（旧口径见 `views/browse.rs:316,466`，T-IMG0 会删掉它） |
| 无分区表镜像「少一层」导致用户以为打不开 | 右栏顶部一行说明「此镜像无分区表，直接显示文件系统根」（一次性、可关闭） |
| CLI 写侧动词与既有档案动词共用参数解析 | `--to` 仅卷语义有效；对普通档案给出明确拒绝（不静默忽略） |
