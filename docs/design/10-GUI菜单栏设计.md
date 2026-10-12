# 10 · GUI 菜单栏设计（完整菜单树 + 点击行为 + 实施分期）

| 项目 | 内容 |
|---|---|
| 文档版本 | v1.1（**已实施**——T-MENU1~11 全部落地；Spike 结论见 §5.2/§5.3，多窗口裁定见 §5.1） |
| 输入 | 用户菜单结构清单（六组：打开/编辑/工具/选项/视图/帮助）+ 逐项点击功能要求 |
| 现状 | [menu_bar.rs](../../crates/hzip-gui/src/ui/bar/menu_bar.rs)：gpui-kit `AppMenuBar` + `OwnedMenu` 快照装配；现有 11 个 action（Open/Settings/Quit/TestIntegrity/SelectAll/SelectInvert/RefreshView/ShowBrowse/ShowJobs/About/EncodeRepair——后两者无实现已裁撤） |
| 能力勘定 | `reveal_in_file_manager`（hzip-platform/open.rs:80 ✓）、`Change::Append/Delete` + EditSave 服务（hzip-format/editor.rs:19 ✓）、`set_filter`（main_vm ✓ 已接线过滤行）、`cx.open_window`（T-PW/op_window 在用 ✓）、MRU 存储（✓ `ui.recent_archives`）、About 对话框（✓ `dialogs/about.rs`）、CF_HDROP 剪贴板（**已勘定**：读 ✓ / 写 ✗——见 §5.2） |
| 上游文档 | [06-GUI-gpui-kit界面设计](06-GUI-gpui-kit界面设计.md)、[07-GUI代码组织与分层约定](07-GUI代码组织与分层约定.md)、[T-SET README](../tasks/T-SET/README.md)（设置注册表——多个菜单项的持久化载体） |

---

## 1. 设计总则

1. **六个触发器**：打开(F) / 编辑(E) / 工具(T) / 选项(O) / 视图(V) / 帮助(H)，顺序按用户清单（工具在选项前、视图在选项后——与现行 文件/编辑/视图/工具/选项/帮助 不同，按新结构重排）。
   > **待裁定**：首组触发器名「打开」建议改「文件(F)」——桌面惯例（7-Zip/资源管理器），且「打开」通常留给菜单内首项；保留用户命名亦可，仅改显示名一处。
2. **禁用态矩阵**：无档案时 编辑全组、在文件管理器显示、解压文件、查找、测试完整性 置灰（沿用现有 `menu_entries(which, has_archive)` 的 disabled 投影，扩展为 `MenuContext { has_archive, has_selection, caps_edit, mru_empty }`）。
3. **勾选态**：视图组 单选/开关项 用 `MenuItem::Action { checked }`（menu_bar.rs:227 已有字段，未启用）。
4. **菜单快照重建触发扩展**：`MenuBar::sync` 现有 两条件（has_archive / locale），本设计新增 **第三条件「菜单数据版本」**——MRU 列表、勾选态（树型目录/工具栏样式/视图样式）任一变化即重建 reload。实现：`ShellState` 持 `menu_epoch: u64`，设置变更（SettingChanged）与 MRU 写入时递增。
5. **i18n**：全部条目走 `crate::tr("menu.*")` 双语 key（沿用现有快照随语言重建机制）。
6. **加速键**（括号内，全局唯一）：Ctrl+O / Ctrl+E / Ctrl+Shift+N / Del / Ctrl+C / Ctrl+V / Ctrl+F / Ctrl+T / Ctrl+, / Ctrl+J / F5。
7. **持久化协同**：树型目录、工具栏样式、视图样式 三个新设置项走 T-SET 注册表（`SettingChanged` 同一事件流）；**菜单入口与设置面板是同一设置的两个触发入口**——正好验证注册表的多入口能力。MRU 列表存储 `ui.recent_archives: Vec<String>`（LRU cap 10）。

## 2. 完整菜单树与点击行为

### 2.1 打开(F)

| 菜单项 | 加速键 | 点击行为 | 状态/禁用 | 实现 |
|---|---|---|---|---|
| 打开压缩文件… | Ctrl+O | 后台 `pick_archive` 文件选择器 → `open_archive` 打开 → 刷新浏览页 + MRU 记录 | 恒可用 | **现有**（Open action 直连） |
| 最近的文件 ▸ | — | 二级子菜单（见下） | 无历史时整组显示禁用项「（无最近文件）」 | **新增** |
| 在文件管理器显示 | — | `reveal_in_file_manager(archive_path, select=true)`（hzip-platform/open.rs:80 现成） | 无档案 disabled | **新增接线**（平台函数已有，菜单直连 S） |
| 解压文件… | Ctrl+E | 打开解压对话框：右栏有选中 → 按选中范围预填；无选中 → 整档解压（与工具栏「解压」同源） | 无档案 disabled | **现有**（解压对话框复用） |
| ———— | | | | |
| 新窗口 | Ctrl+Shift+N | **同进程** `cx.open_window` 第二主窗口（独立 ShellState + 独立 AppData 视图，共享 Engine/注册表——勘定点见 §5.1） | 恒可用 | **新增**（多窗口改造 L） |
| 退出 | Alt+F4 | 有活跃任务 → 确认对话框（沿用 exit_on_finish 语义链）；无任务直接退出（现有 QuitRequested 主线程路径） | 恒可用 | **现有** |

**最近的文件 ▸ 二级子菜单**（动态装配）：

```
最近的文件 ▸  ├ a.zip        （label=文件名，tooltip=完整路径）
              ├ docs.7z
              ├ …（LRU 序，cap 10）
              ├ ————————————
              └ 清空所有历史记录
```

- 条目点击 = 走「打开压缩文件」同一条 open 流程 + MRU 置顶；文件已不存在 → 错误提示 + 从列表移除。
- 「清空所有历史记录」= 清空 `ui.recent_archives` + 落盘 + `menu_epoch` 递增即时收起列表。
- 记录时机：`open_archive` **成功后** append（失败不记）；去重置顶；超 10 条裁尾。
  **不分入口**（T-MENU4 后走查修正）：对话框 / 最近列表 / 工具栏 / 资源管理器双击
  右键（`LaunchIntent::Open`）/ 右键解压家族（`open_then_extract*`，同样把档案装进
  浏览会话）**一律记录**——旧实现给外部入口传 `record_mru=false`，症状是「资源管理器
  双击打开的档案在最近记录里看不到」。唯一例外：**档案内的子压缩包**（进容器 =
  嵌套会话压栈，不是打开一个磁盘档案）不记。
- 数据源：`AppData::recent_archives`（进程内唯一真源，启动自 `ui.recent_archives`
  读入、变更即回写落盘）。菜单直接读它 ⇒ 记录后 `MenuEpochBump` 一帧即刷新，
  不再依赖「泵重读配置」这条间接链路（旧实现：外部入口不发信号 ⇒ 菜单一直显示旧列表）。
- 装配：`menu_entries` 增 `MenuEntry::Submenu` 变体（gpui-kit `MenuItem` 子菜单能力勘定——若 kit 无嵌套弹层，降级为一级平铺「最近：a.zip / …」）。

### 2.2 编辑(E)（全组无档案 disabled；四项均需档案 `caps.edit`）

| 菜单项 | 加速键 | 点击行为 | 状态/禁用 | 实现 |
|---|---|---|---|---|
| 添加文件… | — | 后台 file_picker 多选本地文件 → 逐项 `Change::Append { src, name }` 组 ChangeSet → 确认 → `rewrite` 后台任务（进度窗）→ 完成后刷新浏览 | 无档案 / 格式无编辑能力（tar/single/rar/组合档案）disabled + tooltip 说明原因 | **新增**（底层 `Change::Append` + EditSave 服务已有，GUI 接线 M） |
| 删除文件 | Del | 右栏选中条目批量 → `Change::Delete(entry_id)` 组 ChangeSet → 确认对话框（列条数）→ rewrite 后台任务 → 刷新 | 同上；无选中 disabled | **新增**（M） |
| 复制文件到剪切板 | Ctrl+C | 选中条目物化到 TempArea（复用编辑服务物化路径）→ 写系统剪贴板文件列表（Windows CF_HDROP） | 同上；无选中 disabled | **新增**（剪贴板 API 勘定，M + 风险） |
| 从剪切板粘贴文件 | Ctrl+V | 读剪贴板文件列表 → 批量 `Change::Append` → rewrite 后台任务 → 刷新 | 档案无编辑能力 disabled；剪贴板无文件 disabled | **新增**（与上配对，M） |

> **建议保留项（待裁定）**：现行编辑菜单的 全选(Ctrl+A) / 反选(Ctrl+I) 是已实现能力，建议在本组尾部加分隔线保留（选中管理在编辑菜单符合惯例）；视图组的 刷新(F5) 同理。用户清单未列——**请确认弃留**。

### 2.3 工具(T)

| 菜单项 | 加速键 | 点击行为 | 状态/禁用 | 实现 |
|---|---|---|---|---|
| 查找 ▸ 查找文件 | Ctrl+F | 聚焦浏览页过滤输入框（过滤框未显示时先唤出）——`main_vm.set_filter` VM 链路已就绪，本项主要是 UI 聚焦接线 | 无档案 disabled | **接线**（S） |
| 测试完整性 | Ctrl+T | 提交 `JobKind::Test`（当前档案）→ 进度窗口显示结果（失败条目红色列表） | 无档案 disabled | **现有** |

### 2.4 选项(O)

| 菜单项 | 加速键 | 点击行为 | 状态/禁用 | 实现 |
|---|---|---|---|---|
| 设置… | Ctrl+, | 打开设置对话框（保持**唯一入口**：菜单与工具栏 ⚙ 同源 dispatch_toolbar Settings 分支） | 恒可用 | **现有** |

### 2.5 视图(V)

| 菜单项 | 加速键 | 点击行为 | 状态/禁用 | 实现 |
|---|---|---|---|---|
| 树型目录（☑ 勾选） | — | 切换浏览页左栏树显隐；持久化 `ui.show_tree`（T-SET 注册表新设置项；checked = 当前值） | 恒可用（无档案时切换仅存偏好） | **新增**（S-M） |
| 工具栏 ▸ 大 / 小 / 隐藏（单选 ⊙） | — | 三选一：大=32px 图标 / 小=24px / 隐藏=整条收起；持久化 `ui.toolbar_style`；勾选态随值 | 恒可用 | **新增**（S-M） |
| 视图样式 ▸ 图标 / 详细 / 列表（单选 ⊙） | — | 三选一：浏览右栏渲染模式；持久化 `ui.view_style`；默认详细 | 恒可用（未开档案仅存偏好） | **新增**（详细=现有 DataTable 直连；图标/列表两个新渲染组件 **L，P2 分期**） |
| 任务中心（☑ 勾选） | Ctrl+J | **显隐开关**（2026-09-20 起，与「树型目录」同款语义）：勾选 = 界面出现任务中心**页签**，取消 = 页签消失；持久化 `ui.show_jobs`（默认 **false**）。2026-09-23 起页签栏**关闭钮**取消勾选也走同一事实源（`AppShell::close_tab` → `apply_setting(ShowJobs, false)`）；页签栏挂在工具栏右侧（左侧为工具按钮），只要**有任意页签**就渲染（打开档案即出「浏览」页签）。Ctrl+J 仍是**切页签**，且任务中心未开时会先落盘打开再切 | 恒可用 | **现有**（ToggleJobs 开关 + ShowJobs 切页签） |

> **动作派发的焦点前提（2026-09-23 实测缺陷与修复）**：菜单项的动作用 gpui
> `dispatch_action` 派发，它从**当前焦点节点**沿渲染树向上冒泡；若窗口**一个焦点都
> 没有**，派发会回落成「从根节点派发」——根节点没有祖先，挂在主窗口壳层 div 上的
> `on_action` 链**整条收不到**，表现为「菜单点了没反应」。用户踩到的实例：关掉设置
> 页签时，持有焦点的设置面板实体被销毁 ⇒ 焦点变空 ⇒ 之后「选项 ▸ 设置…」再也打不
> 开设置页签；频繁开关页签更容易命中。
>
> 修复（两面）：① 主窗口壳层 `.track_focus(壳层句柄)`，成为**可聚焦的焦点恢复目标**
> （gpui 的 focus-lost 恢复会把焦点交还最近的存活祖先）；② 渲染帧里发现
> `window.focused(cx).is_none()` 时把焦点自补到壳层。回归用例：
> `shell::tests::rapid_tab_cycles_keep_settings_openable`（真实 `Settings` action 派发
> + 真实点击关闭钮 + 多轮开关三条页签，逐帧断言设置页签出现/消失）。

### 2.6 帮助(H)

| 菜单项 | 加速键 | 点击行为 | 状态/禁用 | 实现 |
|---|---|---|---|---|
| 关于 hzip… | — | About 对话框：**版本号 + 开源许可**（2026-10-09 用户口径：去掉引擎/格式清单与仓库地址；许可行在编译期由根 `LICENSE` 派生，见 T-LIC §2.3） | 恒可用 | **已实现**（`ui/dialogs/about.rs`） |
| 检查更新… | — | 与设置页「检查更新」按钮同源（T43 挂账）：一期点击 → 提示「在线更新暂未接入」；T43 落地后读配置 `[update].check_url`（默认 = 本项目 GitHub 仓库 releases/latest）发起检查 | 恒可用 | **占位**（S） |

## 3. 状态机：MenuContext（禁用/勾选投影输入）

`menu_entries(which, has_archive)` 签名扩展为：

```rust
pub struct MenuContext {
    has_archive: bool,      // 现有
    has_selection: bool,    // 删除/复制 需要
    caps_edit: bool,        // 档案格式可编辑（registry caps + 非组合）
    mru: Vec<String>,       // 最近文件列表（装配子菜单）
    show_tree: bool,        // 勾选态
    toolbar_style: ToolbarStyle,
    view_style: ViewStyle,
}
```

`ShellState` 增 `menu_epoch`，`MenuBar::sync` 第三触发条件消费（§1.4）。`MenuAction` 枚举同步扩：`Reveal`、`Extract`、`NewWindow`、`AddFiles`、`Delete`、`Copy`、`Paste`、`FindFile`、`ToggleTree`、`ToolbarStyle(…)`、`ViewStyle(…)`、`CheckUpdate`（About 已有）。

## 4. 实施分期

| 期 | 内容 | 任务量 | 依赖 |
|---|---|---|---|
| **P0 现有直连** | 打开压缩文件 / 在文件管理器显示 / 解压文件 / 退出 / 测试完整性 / 查找聚焦 / 设置 / 任务中心 / 关于对话框 / 检查更新占位 + MenuContext 签名扩展 + 菜单重排（六组新序） | S，一批完成 | 无 |
| **P1 偏好与动态** | 最近的文件（MRU 存储 + 子菜单 + 清空 + menu_epoch）/ 树型目录 / 工具栏三态 | M | **T-SET2**（注册表——三个持久化项） |
| **P2 编辑四件套** | 添加 / 删除 / 复制 / 粘贴（ChangeSet GUI 化 + 确认对话框 + 密码档案处理） | M×4 | P0；无 T-SET 依赖 |
| **P3 视图与窗口** | 视图样式图标/列表渲染组件 / 新窗口（多窗口架构） | L×2 | P1（view_style 设置项） |

## 5. 勘定点与风险

### 5.1 新窗口（最大勘定点）

单实例锁是**进程级**（instance.rs 文件锁 + argv 转发）——同进程多窗口不冲突，但三处共享态需裁定：

- `AppData` 是进程单例（`Arc<Mutex>`）：新窗口建议**独立 AppData**（独立浏览态/任务路由），仅共享 `Engine`（线程池只建一次）；
- `signal_tx` 单接收端：泵信号需按窗口路由（signal 携 window_id 或每窗口一通道）；
- 退出语义：关单窗口 ≠ 退进程（全部窗口关才走退出链）。

若此项成本超预期，降级方案：**「新窗口」一期接 argv 自启**——但会被单实例转发回本窗口（等于无效），故**不可行**；二期再上多窗口。

> **T-MENU10 落地结论（已实施）**：三裁定全按建议执行——每窗口**独立 `AppData`**（独立浏览态/任务路由/设置投影，仅共享 `Engine`）；信号改为**每窗口一条 `SignalTx` 通道**（窗口归属由通道归属回答，未引入 `window_id` 载荷）；退出语义收口在 `shell/windows.rs::all_windows_closed(registered, live)`（注册表 + gpui 窗口集双零才退）。实现：`shell/runtime.rs` 抽 `spawn_main_window` 工厂（自引用槽注入 shell 的「新窗口」入口）+ `shell/windows.rs` 注册表（`WindowEntry{id, shell, data, handle}`，泵按 id 稳定序逐窗口刷快照）。

### 5.2 剪贴板（CF_HDROP）

gpui 剪贴板 API 是否暴露文件列表（CF_HDROP / `FileList`）未勘定。降级方案：hzip 内部剪贴板（仅支持 hzip 内复制→粘贴，跨应用不可见），系统剪贴板仅支持单文件物化复制。实施前先做 Spike 验证。

> **T-MENU3 Spike 结论（0.3.4 首查；0.3.8 迁移时复核：结论未变——读 CF_HDROP 可用、写仍是空臂，证据行号见 `crates/hzip-platform/src/clipboard.rs` 模块头）**：
>
> | 方向 | 能力 | 证据 |
> |---|---|---|
> | 读 CF_HDROP（外部 → hzip） | **可用** | `gpui-pre-windows/src/clipboard.rs::read_from_clipboard` 枚举 `CF_HDROP` 并经 `DragQueryFileW` 还原路径 → `ClipboardEntry::ExternalPaths` |
> | 写 CF_HDROP（hzip → 外部） | **不可用** | 同文件 `write_to_clipboard` 对 `ClipboardEntry::ExternalPaths` 是**空臂**（`=> {}`）——gpui 只写文本/图片 |
>
> **落地（T-MENU8）**：按 B3 降级线实施——进程级**内部剪贴板**（`hzip-platform/clipboard.rs`，hzip 内复制→粘贴闭环；复制物化区亦为进程级 `TempArea`，活到粘贴那刻）+ **系统剪贴板只读**（外部资源管理器复制 → hzip 粘贴，走 `cx.read_from_clipboard()` 的 `ExternalPaths`）。hzip 复制 → 外部粘贴**挂账二期**（需自绘 win32 `OpenClipboard` + `DROPFILES` 写入）。

### 5.3 嵌套子菜单

gpui-kit `MenuItem` 是否支持二级弹层（最近文件 / 工具栏 / 视图样式三处用）未勘定。降级方案：一级平铺（「最近：a.zip」× N + 「工具栏：大」式平铺项）。实施前 Spike。

> **T-MENU2 Spike 结论（0.3.4 首查；0.3.8 复核：`gpui-pre-0.3.8/src/platform/app_menu.rs` 仍有 `Submenu` 能力，二级弹层结论未变）**：**支持**。`gpui-pre-0.3.4/src/platform/app_menu.rs` 有 `MenuItem::Submenu(Menu)` 与 `OwnedMenuItem::Submenu(OwnedMenu)`；`gpui-component/src/menu/popup_menu.rs::with_menu_items` 对 `OwnedMenuItem::Submenu` **递归装配**（`self.submenu(...)`），`AppMenuBar::build_popup_menu` 亦按 `OwnedMenuItem` 直通——故二级弹层是一等能力。
>
> **落地（T-MENU4/5）**：`MenuEntry::Submenu{label, entries, disabled}` → `gpui_kit::MenuItem::Submenu(Menu::new(label).items(..))`（`build_app_menus` 递归 `build_items`）。**B4 平铺降级不需要**。三处使用者：最近的文件（T-MENU4，动态条目）、工具栏三态、视图样式三态（T-MENU5，`checked` 单选组）。

### 5.4 编辑四件套共性

- `caps.edit` 前置：tar/single/rar/组合档案 → 四项整组 disabled + tooltip「该格式不支持编辑」；
- 加密档案：rewrite 需密码（EditSave PasswordRef）→ 复用密码对话框请求链（router 已有）；
- 编辑后刷新：rewrite 完成 → `VmSignal::EntriesChanged` 重读档案（勿沿用旧 entries 缓存）。

## 6. 测试锚点

1. `menu_entries` 清单断言：六组条目/顺序/加速键/禁用矩阵（无档案×无选中×caps_edit 组合表驱动）——沿用现有 `file_menu_entries` 风格。
2. MRU 纯函数单测：append 去重置顶 / cap 10 裁尾 / 清空。
3. 勾选态：`SettingChanged(ShowTree)` → menu_epoch 递增 → `menu_entries` checked 翻转（注册表多入口路由锚点）。
4. i18n：`build_app_menus_follows_locale` 扩展到新 key 全覆盖（en/zh 断言）。
