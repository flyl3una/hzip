# T-IMG9：编辑管线接入 + 写模式开关（默认 B，可切 A）

| 项目 | 内容 |
|---|---|
| 阶段 | P3 集成（用户裁定 ② 的落点：设置页可选「覆盖写（默认）/ 就地写 / 只读」） |
| 依赖 | T-IMG6、T-IMG7、T-IMG8 |
| 状态 | 未开工 |
| 目标 | 让 core 的既有编辑链路（双击编辑→保存 / 追加 / 删除 / 改名）在卷容器上生效，并把写模式从设置页一路传到设备链末端 |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 改 | `crates/hzip-core/src/services/edit.rs` | ① 路径选择：`handler.supports_in_place_edit() && 源是常规文件 && 非组合链` ⇒ `write_in_place`；否则 `rewrite`；② `CreateOptions.volume_write` 由配置填入 |
| 改 | `crates/hzip-core/src/engine/core.rs`（或 `JobCtx` 组装处） | 读取 `hzip_config::Config.ui.volume_write_mode` → `CreateOptions` |
| 改 | `crates/hzip-config/src/lib.rs` | `Ui.volume_write_mode: String`（`"overlay"` 默认 / `"inplace"` / `"readonly"`；同 `toolbar_style` 的三态字符串口径） |
| 改 | `crates/hzip-gui/src/app/settings_registry.rs` + `crates/hzip-gui/locales/settings.toml` + 设置页渲染 | 新设置项「磁盘镜像写回方式」（三选一）+ 中英文案 |
| 改 | `crates/hzip-gui/src/app/state.rs`（SettingsPrefs / load_settings / save） | 读写该设置（照 `toolbar_style_ix` 的索引 ↔ 值映射写法） |
| 新 | `crates/hzip-core/tests/volume_edit.rs` | 编辑管线端到端（含提交前原档不变、取消、脏卷拒写三条硬断言） |

## 步骤 0：前置（已查实，取代原「未决勘察点」）

**2026-10-16 已按源码查实**：core 的既有编辑链 `rewrite_in_place`（:121）→ `rewrite_and_replace`（:182，`Sink::File(tmp)`）→ `finish_replace` → `hzip_platform::fs_util::atomic_replace`（:490）**只支持「源本身是一个可编辑档案」**：

- `hzip-format/src/registry.rs:151-153`：`Sniffed::Composite` ⇒ `Unsupported{ "组合格式不支持编辑" }`；
- `hzip-gui/src/views/browse.rs:316,466`：`can_edit: snap.caps_edit && !snap.nested`（嵌套会话一律禁编辑）；
- `hzip-gui/src/app/state.rs:763-784`：`caps_edit()` 口径即「非组合格式」。

⇒ 本任务**依赖 T-IMG0**：组合/嵌套写回由 T-IMG0 打通（`CompositeEditor` + 逐层冒泡 + `chain_editable` 会话判定）。本任务只做两件事：

1. **路径选择**：`handler.supports_in_place_edit() && 源是常规文件 && 非组合链` ⇒ `write_in_place`；其余（含组合链）⇒ `rewrite`（组合链必然重写外层，见 T-IMG0 §7）；
2. **写模式下发**：`CreateOptions.volume_write` 由配置填入（一处）。

## 步骤 1：写失败测试

`crates/hzip-core/tests/volume_edit.rs`：

```rust
#[test]
fn edit_and_save_inside_ext4_image_is_visible_after_reopen() {
    let img = write_temp("disk.img", &testkit::vol::raw_with_mbr(FsKind::Ext4, &[("/note.txt", b"v1")]));
    let engine = Engine::new(cfg_with_write_mode("overlay")).unwrap();
    // ① 打开会话 → 物化条目到临时目录（既有 open_entry_for_edit）→ 本地改写 → save_back
    // ② 重新打开镜像 → /note.txt == "v2"
}

#[test]
fn default_mode_leaves_the_source_untouched_until_commit() {
    /* 覆盖写：物化期原档 sha256 不变；`save_back` 成功后才变（且临时溢出文件已清理） */
}

#[test]
fn read_only_mode_refuses_edit_with_reason() {
    let engine = Engine::new(cfg_with_write_mode("readonly")).unwrap();
    /* 双击编辑 → 保存 ⇒ Err Unsupported{what: "vol.readonly.by-setting"}；原档不变 */
}

#[test]
fn in_place_mode_commits_without_extra_copy() {
    /* 就地写：提交后原档变；并断言未产生「同目录 hzip-tmp-* 全量副本」（用目录清单断言） */
}

#[test]
fn dirty_volume_needs_explicit_replay_to_be_writable() {
    /* 脏卷夹具 ⇒ 首次保存被拒且理由是 JournalPending；
       用户勾选「先重放日志再写入」⇒ 第二次保存成功 */
}

#[test]
fn composite_source_qcow2_gz_rewrites_the_outer_container() {
    /* 在 .qcow2.gz 会话内改文件 ⇒ 提交后：外层 gz 可解、内层镜像内容含新文件、其余文件 sha256 不变 */
}
```

`crates/hzip-gui/src/app/tests/state.rs` + `src/app/tests/settings_registry.rs`：

```rust
#[test] fn volume_write_mode_defaults_to_overlay_and_round_trips() { /* "overlay" 默认；写 "inplace" 读回一致；非法值回落 overlay */ }
#[test] fn settings_row_shows_three_options_with_i18n_keys() { /* 三选一控件在场，标签键存在（zh-CN + en 都有） */ }
```

## 步骤 2：实现

### 2.1 配置项（`hzip-config`，照 `toolbar_style` 先例）

```rust
pub struct Ui {
    /* …既有… */
    /// 磁盘镜像写回方式：`overlay`（默认，覆盖写）| `inplace`（就地写）| `readonly`（只读）。
    /// 非法/缺省值一律回落 `overlay`（读取侧 `parse()`，不 panic）。
    pub volume_write_mode: String,
}
impl Ui { pub fn volume_write(&self) -> hzip_format::VolumeWriteMode { /* parse，默认 Overlay */ } }
```

### 2.2 core：路径选择（`services/edit.rs`）

```rust
fn rewrite_and_replace(ctx: &JobCtx, src_archive: &Path, target: &Path) -> Result<WriteReport, HzipError> {
    let editor = open_editor(src_archive, ctx, pw)?;
    let caps = editor_caps(src_archive, ctx, pw)?;              // 既有：取 fmt caps
    // 卷容器 + 常规文件源 ⇒ 就地写（不产生全量副本；40 GB 镜像的意义所在）
    if handler.supports_in_place_edit() && is_regular_file(src_archive) && !is_composite_chain(src_archive) {
        return editor.write_in_place(changes_for(), &wctx);     // 原子性由格式自持（覆盖层 + 提交回放）
    }
    /* …既有：rewrite 到 Sink::File(tmp) + finish_replace… */
}
```

> `write_in_place` 的**可重入**已由 T-IMG1 契约与 T-IMG6 测试保证；密码重试逻辑（既有 `PwPrompter`）不改。

`CreateOptions.volume_write` 的填入点：core 组装 `WriteCtx` 时 `opts.volume_write = cfg.ui.volume_write()`（一处，`write_opts_for` 附近）。

### 2.3 GUI：设置项与三态控件

- `settings_registry.rs` 新增 `SettingId::VolumeWriteMode`（三态分组控件，与 `toolbar_style` 同款渲染路径）；
- `locales/settings.toml` 新增：`volume.write_mode.label` / `.hint` / `.overlay` / `.inplace` / `.readonly`（中英各一份）；
- 提示语（**必须写清代价**）：
  - 覆盖写：「改动先暂存，提交时才写回镜像；提交前原文件不会被改动（推荐）」
  - 就地写：「改动立即写入镜像；更快，但中断时镜像可能处于中间状态」
  - 只读：「只浏览，不写任何字节」
- 只读模式下列表/预览/提取照常，编辑类菜单全部置灰（既有 `caps_edit` 通道自动生效）。

### 2.4 脏卷的显式重放（用户确认门）

首次遇到脏卷保存时，弹既有确认对话框样式（`ui/dialogs/edit_confirm.rs` 同款）：

```text
这个卷里有未重放的日志（上次没正常卸载）。
   [先重放日志并写入（推荐）]  [保持只读]
```

选择重放 ⇒ `FsOptions.replay_journal = true`，重试一次；选择只读 ⇒ 只读 + 理由键。

## 步骤 3：跑测试与验收

```bash
cargo test -p hzip-core --test volume_edit
cargo test -p hzip-gui --lib
cargo test -p hzip-config
cargo test --workspace
```

### 步骤 4：提交

```bash
git add crates/hzip-core crates/hzip-config crates/hzip-gui
git commit -m "feat(vol): 编辑管线接入卷容器 + 写模式三态设置（默认覆盖写）"
```

## 验收

- [ ] 三种写模式在设置页可选、可持久化、非法值回落覆盖写
- [ ] 覆盖写：提交前原档 sha256 不变（含「双击编辑但不保存」路径）；就地写：无全量副本产生
- [ ] 只读模式：编辑类操作全部拒绝且文案正确；浏览/提取不受影响
- [ ] 脏卷：先拒写 → 用户确认重放 → 成功；全程无静默改写
- [ ] **组合源**：`.qcow2.gz` 内改文件后，外层 gz 可解、未改动文件 sha256 不变（TC-CONF-009 同口径）
- [ ] 密码重试路径不重复累积变更（同一 editor 二次调用等价于一次）

## 风险 / 注意

| 点 | 处置 |
|---|---|
| `write_in_place` 绕过 core 的「临时文件 + 原子替换」⇒ 既有 `TC-CONF-009` 原子性口径要重新表述 | 在 02 §7.3 / §9.9 补一句：**就地编辑的原子性由格式自持**（卷格式 = 覆盖层暂存 → 提交回放；提交前原档不变），并把 `volume_edit.rs` 里那条 sha256 断言作为该口径的锚点 |
| 虚拟机可能正在使用该镜像 | 脏标记提示（`uncleanShutdown`/`dirty`）+ 首次写入一次性强提醒（「请确认没有虚拟机在运行」）；不做锁检测（跨平台不可靠） |
| 写模式与「打开会话」的关系 | 模式在**打开时**读取一次（会话内不变），避免运行中切换造成半写半覆盖 |

---

## 落地记录（2026-10-16）

### 已落地并自验

| 项 | 落地形态 | 现实验证 |
|---|---|---|
| **core 路径选择** | `services/edit.rs`：`in_place_supported()`（源是常规文件 + `sniff` 判为**单格式** + handler 声明 `supports_in_place_edit()`）⇒ `editor.write_in_place()`；否则沿用 `rewrite` + 同卷临时档 + 原子替换。收尾分 `finish_in_place()`／`finish_replace()`，报告同款 | `hzip-core/tests/volume.rs::edit_batch_inside_a_volume_writes_in_place`：`JobKind::EditBatch` 真跑一遍，用**写出字节数**（`stats.bytes_done < 镜像/4`）区分两条路径——只看「临时档残留」区分不了（rewrite 成功后临时档被 rename 掉，同样不留残留） |
| **变更路由（写侧）** | `hzip-vol/src/editor.rs::route_change()`：档案级变更 → 卷内变更。`Replace{entry}`/`Delete(id)` 走 `id - 1`（读侧投影把卷行排在最前）；`Append`/`MakeDir` 剥 `"{根名}/"` 前缀；`Rename` 的 `from` 必是完整路径 ⇒ 剥前缀，而 `to` **两种形态都合法**（GUI 传完整路径 / 契约允许裸叶名，驱动按 `parent(from) + '/' + to` 拼）——只认一种就会误拒 GUI 的改名。不可路由（前缀不符 / id 指向根行 / `to` 带 `/` 却指向别卷）⇒ `vol.unsupported.change-route` **拒绝**，绝不猜路径去写 | `hzip-vol/tests/editor.rs` **7 条**：路由后的**卷内**变更逐条断言（`replace:1` / `delete:0` / `append:etc/new.txt` / `rename:etc/a->etc/b` 与 `rename:etc/a->c`、`append:etc/sub/` 尾 `/` 目录标记保住）、拒绝路径零落盘；另有 `hzip-core/tests/volume.rs` 两条端到端：`edit_batch_inside_a_volume_writes_in_place`（就地写 + 只报脏块）与 `edit_batch_renames_a_file_inside_a_volume`（真 ext4 改名） |
| **命名唯一事实源** | `hzip-vol/src/reader.rs::volume_root_names()` 由读侧投影与写侧路由**共用**（原先各写一份必然漂移，漂移的后果是写到错路径） | 同上用例（mock 卷无表 ⇒ 根名 `Volume`；vmdk 夹具 ⇒ `Partition 1`，两处断言一致） |
| **拒绝文案** | 新增键 `vol.unsupported.change-route`（en/zh-CN），已进 `hzip-gui/src/tests/vol_locale.rs` 的 `VOL_KEYS` 清单（漏登记会当场红） | `cargo test -p hzip-gui vol_locale` |
| **外部编辑器保存写回** | `app/external_edit.rs` + `ui/shell/external_edits.rs`：物化 → `FileWatch`（500 ms 去抖）→ `EditBatch + Replace{entry}`（提交前 `release_session_for_edit` 放句柄——Windows 上目标被打开时原子替换必失败） | `hzip-gui` 6 条：`save_back_rewrites_the_archive`、`save_back_inside_a_volume_writes_in_place`（用户场景：镜像 → 分区 → 改卷内文件 → 保存 → 内容真的变了）、在飞去重与补提交、拒绝只提示一次、目标判定 |
| **写模式默认值** | `CreateOptions.volume_write` 默认 `Overlay`（提交前原档字节不变），core 侧未显式传值时即该语义 | qcow2/vmdk 的覆盖写用例（vmdk 29 / qcow2 24 全绿） |

### 仍未落地（按优先级）

1. **设置页写模式三态开关**（覆盖写 / 就地写 / 只读）：`hzip-config` 的 `Ui` 尚无字段，core 侧也无「配置 → `CreateOptions.volume_write`」的接线。当前默认覆盖写是**安全且正确**的，故未阻塞可用性。
2. **多分区镜像的变更按分区归集**：现在 `mount_writable()` 遇到「卷数 > 1」直接按键拒绝（`vol.unsupported.change-route`）。做它需要：core 侧把每条变更连带**分区归属**一起传下来（或编辑器重建与读侧同款的投影），再把名/id 路由到对应分区。
3. ~~**「外部编辑器保存回卷内条目」**~~ **已落地（2026-10-16，GUI 侧）**：链路改为「物化临时文件 → 挂 `FileWatch`（去抖 500 ms）→ 保存事件 → 提交 `EditBatch + Change::Replace{entry, src=temp}`」——**不需要** `JobSpec` 新增字段，也不走 `save_direct`（那条路固定「条目 0 = 整档单文件」，对卷必然指错目标，保持现状）。实现：`app/external_edit.rs`（纯数据面：登记/`(mtime,len)` 判重/在飞串行/决定） +`ui/shell/external_edits.rs`（UI 半场：watch 句柄、drain、提交、通知） +`runtime.rs` 每 wake 对账（补「在飞期间又保存」）。边界：嵌套会话与不支持编辑的格式**不提交**、保存时出一条通知如实告知（文案 `edit.writeback.*`）。
4. **只读徽标 + 理由 tooltip**（`vol.readonly.*` 文案已备）。
5. **组合源写回**（`.qcow2.gz` 的编辑）：T-IMG0 范围。
