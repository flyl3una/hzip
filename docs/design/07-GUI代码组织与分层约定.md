# 07 · GUI 代码组织与分层约定

| 项目 | 内容 |
|---|---|
| 文档版本 | v1.0（T-GUI 结构重组落地后固化） |
| 适用范围 | `crates/hzip-gui/src/**` |
| 上游文档 | [02-程序详细设计.md](02-程序详细设计.md) §11.1、[T-GUI 任务规划](../tasks/T-GUI/README.md) |
| 读者 | 后续在 hzip-gui 上编码的所有开发者 |

> **本文是新增/修改代码时放置位置的判定依据。** 拿不准时按 §5 决策树走；
> 决策树覆盖不了的，先更新本文再写代码。

---

## 1. 分层总览

```
main.rs (bin 入口：日志/argv 解析)
   │
   ▼
ui/shell/runtime.rs :: run_ui()        ← 组合根（唯一装配点）
   │
   ├─► ui/     gpui_kit 薄壳（渲染 + 事件上行；可 import gpui_kit）
   ├─► app/    进程/应用级设施（零 gpui；见 §3 例外规则）
   └─► viewmodel/  纯 MVVM 业务逻辑（零 gpui，无窗口可测）
```

数据流（自下而上）：

```
viewmodel（VM 状态机/条目/任务聚合）
    │  export_snapshot()
    ▼
app::state::AppData ── 聚合 VM + Engine + Config + Router
    │  consume_dialog_event() ← UI 设置事件回投
    ▼
ui::contracts::AppStateUiSnapshot（只读快照）
    │  ShellState::apply（字段级变化检测）
    ▼
ui::shell::AppShell ── chrome 渲染 → views 渲染
```

## 2. 目录职责（当前实际结构）

```
crates/hzip-gui/src/
├── main.rs               bin 入口：日志初始化 + argv 解析 → run_ui
├── lib.rs                模块声明（viewmodel / app / ui）
├── app/                  ★ 进程与应用级（零 gpui）
│   ├── instance.rs       单实例锁 + 二实例 argv 转发
│   ├── exit.rs           退出钩子（2 s 宽限 + TempArea 清理）
│   ├── pump_host.rs      后台任务泵线程 + 信号 forwarder + 完成通知
│   ├── file_picker.rs    rfd 原生文件/目录/可执行选择器
│   ├── state.rs          AppData 数据面（engine/jobs/router/设置偏好 + 快照导出）
│   ├── settings_registry.rs  设置注册表（SettingDef/SettingValue/WidgetKind/Effect + REGISTRY/ACTIONS 常量表 + SettingsView 活读）
│   └── labels.rs         label↔value 白名单（UI 文案 ↔ config 落盘值，唯一真相源）
├── viewmodel/            纯 MVVM（main_vm / jobs_vm / dialog_vm / router / rows / pump）
│   ├── setting_id.rs     SettingId 设置项标识（注册表键；gpui 元素 id 派生源）
│   └── …
└── ui/                   ★ gpui_kit 薄壳
    ├── mod.rs            模块声明（不使用 ui.rs + ui/ 双轨形式）
    ├── shell/            主窗口骨架：AppShell + ShellState
    │   ├── mod.rs        模块声明与再导出（**不含业务实现** —— T-STD 2026-10-08 口径）
    │   ├── state.rs      ShellState + `apply`/`to_snapshot`/`present_tabs`（快照投影与修态）
    │   ├── app_shell.rs  AppShell 结构体 + 生命周期/页签/快照应用/配置缓存
    │   ├── render.rs     `impl Render for AppShell`（chrome：标题栏/菜单/工具栏/页签/内容插槽/状态栏）
    │   ├── file_ops.rs   增删/改名/新建（含名称行内编辑与确认对话框接线）
    │   ├── open_flow.rs  打开链（选中打开/容器进入/档案打开 + run_open_flow/open_archive_task）
    │   ├── clipboard_ops.rs  复制/粘贴/过滤聚焦
    │   ├── session.rs    会话保活/行重建/换会话代数（spawn_* 后台线程）
    │   ├── errors.rs     HzipError → 人读原因等纯映射
    │   ├── dispatch.rs   dispatch_toolbar（工具栏动作 → 业务分发）
    │   ├── prompt.rs     密码/冲突请求-应答弹窗（open + answer）
    │   ├── windows.rs    主窗口注册表（T-MENU10：多窗口信号路由 + 退出语义）
    │   └── runtime.rs    run_ui 启动序列 + 10 Hz 主线程泵 + LaunchIntent + 窗口工厂
    ├── bar/              窗口级横条（命名避开 chrome——防浏览器技术误解）
    │   ├── menu_bar.rs   菜单条（gpui-kit AppMenuBar + GlobalState 菜单注册）
    │   ├── toolbar.rs    工具栏
    │   └── tab_bar.rs    页签条（TabBar on_click(usize) 单通道）
    ├── views/            页面视图
    │   ├── browse.rs     浏览页数据装配 + BrowseDelegate（表格委托）
    │   ├── browse_layout.rs   浏览页双栏布局（树/表格/面包屑/过滤行装配）
    │   ├── jobs.rs       任务中心页（11 列 DataTable）
    │   ├── preview.rs    预览独立窗口（文本/HEX/图片）
    │   ├── progress/     压缩/解压进度独立窗口（T-PW；mod.rs = 窗口实体 + 注册表 + 泵分发；file_list.rs = 已处理文件虚拟列表）
    │   ├── settings_layout.rs    设置面板骨架（Dialog + TabBar + footer + 活读刷新）
    │   └── settings_render.rs    注册表驱动页渲染器（WidgetKind 分派 + 列块/勾选组叶子）
    ├── widgets/          自绘 widgets（仅 gpui-kit 没有的）+ kit 缺陷封装
    │   ├── checkbox.rs  复选框封装（**唯一入口**：修 kit 行高写死致下降部裁切）
    │   ├── icon.rs      AppIcon 业务图标收口
    │   ├── file_icon.rs 条目图标（系统图标 + 回退；T-ICON：`views` 调用本件，回退映射（`row_type_icon`/`tree_row_icon`）仍在 `views`）
    │   ├── status.rs     busy 条/状态栏/任务胶囊
    │   ├── filter_row.rs    过滤输入行
    │   └── breadcrumb.rs    面包屑条
    ├── dialogs/          对话框（按逻辑分文件）
    │   ├── mod.rs        共享装配（Form 壳/只读框/Select/Input 实体通道）
    │   ├── password.rs / conflict.rs / extract.rs / compress.rs
    │   ├── about.rs      关于对话框（T-MENU1：版本/格式清单/仓库/许可）
    │   ├── edit_confirm.rs  编辑提交确认（T-MENU6/7：添加/删除/同名冲突）
    │   └── tests.rs      目录级测试（对话框契约）
    ├── constant.rs       界面尺寸/颜色**默认值常量表**（布局字面量的唯一定义处）
    ├── spacing.rs        布局密度提供器 LayoutMetrics（间距/内边距/行高的唯一出口，`sp()`）
    ├── theme.rs          主题 token 唯一真相源（focus/selection 统一 + key 合法性测试）
    ├── contracts.rs      契约类型总表（快照/事件/回调/纯派生函数）
    ├── binding.rs        VM 行 → UI 行派生（JobsRowUi/BrowseRow/格式化）
    └── assets.rs         RustEmbed 图标资源装配
```

> **测试代码放置（T-STD 裁定 C，2026-10-08）**：
> - 单文件测试 → 同目录 `tests/<业务文件名>.rs`（`mod.rs` 用 `tests/mod_rs.rs`）；业务文件只留一行
>   `#[cfg(test)] #[path = "tests/<file>.rs"] mod tests;`
> - 目录级 / 跨文件契约测试 → 同目录 `tests.rs`（如 `dialogs/tests.rs`）或 `tests/mod.rs`
> - 跨模块集成测试 → crate 根 `src/tests/mod.rs`（`hzip-gui` 原有的 `ui/tests.rs` 已迁此）
> - **禁止**业务文件内联 `#[cfg(test)] mod tests { … }`（机检见 §3 表）

## 3. 依赖方向与红线

| 规则 | 守卫方式 |
|---|---|
| `viewmodel/` 禁止 import gpui/gpui_kit | grep `use (gpui\|gpui_kit)` src/viewmodel/ 必须为空 |
| `app/` 禁止 import gpui/gpui_kit | grep `use (gpui\|gpui_kit)` src/app/ 必须为空 |
| 依赖单向：ui → app → viewmodel | review 把守（编译器不拦截同 crate 反向引用） |
| **例外**：`app::state` 允许引用 `ui::contracts`（快照/事件/`crumbs_from_current_dir` 纯派生） | 契约面保持纯类型/纯函数；contracts 新增项不得引入 gpui 行为 |
| `ui/` 根目录只放横切文件（theme/contracts/binding/assets）+ mod.rs；新业务文件必须进子目录 | review 把守 |
| 目录一律用 `mod.rs` 形式（`ui/mod.rs`），禁止 `ui.rs` + `ui/` 双轨并存 | review 把守 |
| **`mod.rs` 只做模块声明与再导出**：不得出现 `impl`/`fn`/`struct`/`enum`/`trait`/`const`/`static`/`type` 定义（T-STD 2026-10-08 用户口径） | `python build/checks/layout-conventions.py`（CI `quality` job） |
| **测试代码只住 `tests` 名下**：业务文件禁止内联 `#[cfg(test)] mod x { … }`；`#[cfg(test)] mod x;` 须解析到 `<dir>/tests/<file>.rs`、`<dir>/tests.rs` 或 crate 根 `src/tests/` | 同上 |
| 依赖方向：仅 `ui → app → viewmodel`；`tests/` 树内的测试代码不受 L1/L2/L3 约束（它本身就是测试） | 检查器 + review |

红线语义：**viewmodel 是可无窗口测试的纯逻辑层；app 是可脱离 UI 的进程层；ui 只做渲染与事件上行**。后台调度（pump_host）、消息通知（pump_host 完成钩子 → hzip-platform）、单实例仲裁（instance）一律在 `app/`，**永不进 `ui/`**。

## 4. 编码约定

1. **组件优先 gpui-kit**：Dialog/DataTable/Tree/Menu(AppMenuBar)/Select/Switch/Badge/TabBar/Form 等一律用 kit；kit 没有的才自绘，且自绘件放 `ui/widgets/`；**kit 组件有缺陷时在 `ui/widgets/` 封一层**（就地绕过，不改依赖）——复选框即此例：kit 的 `Checkbox` 把标签行高写死 `relative(1.)`（行框 = 字号），拉丁字母下降部被内容遮罩裁掉（`gz` 的 g 看起来像 a），故一律经 `ui::widgets::checkbox(id, label)` 构造，标签走 children 槽自绘并显式给行高。
2. **颜色一律 `cx.theme()` token**（danger/success/warning/primary/muted_foreground…）；禁止裸色值（`black()`/`hsla()`/hex）。主题槽位只改 `ui/theme.rs`，并跑其 key 合法性测试。
3. **测试放置（T-STD 2026-10-08 口径，取代旧「测试就地放」）**：测试代码一律独立文件，业务文件只留一行声明：
   - 单文件测试 → 同目录 `tests/<业务文件名>.rs`（`mod.rs` → `tests/mod_rs.rs`），声明写
     `#[cfg(test)] #[path = "tests/browse.rs"] mod tests;`；
   - 目录级 / 跨文件契约测试 → 同目录 `tests.rs`（如 `dialogs/tests.rs`）或 `tests/mod.rs`；
   - 跨模块集成测试 → crate 根 `src/tests/mod.rs`；
   - **禁止**业务文件内联 `#[cfg(test)] mod tests { … }`（`layout-conventions.py` 会拦）；
   - 为什么用 `#[path]` 而不是把测试挪到别处：测试模块仍是**该业务文件的子模块**，
     `use super::*` 与私有项访问语义完全不变（无需为测试放宽任何可见性）；
   - 同一文件原有的多个测试模块合并进同一个测试文件（`tests` 的内容置顶层，其余作嵌套 `mod`）。
4. **事件上行走 ShellHandlers**（`ui/contracts.rs`）；views/widgets 不直接触碰 AppData。
5. **实体稳态纪律**：TreeState/TableState/InputState 等有状态实体经 Hold（Global）持有，渲染期复用，禁止每 render 重建。
6. **label↔value 白名单只写一处**：`app/labels.rs`；UI 文案与 config 落盘值不得在其他文件各留一份。
7. **新增/修改设置项只改注册表**：在 `app/settings_registry.rs` 的 `REGISTRY` 加一行（`load`/`store`/`default`/`effect` 四个纯函数指针）+ 需要时在 `viewmodel/setting_id.rs` 加枚举项。渲染、落盘、生效分发、恢复默认全部由框架走 `normalize → store → persist → dispatch → refresh` 五步；**设置项不得自行 `Config::save()`**（写盘是框架固定尾步，一次事件只碰一次盘）。UI 半场生效（需 Window/App，如主题）只能声明 `Effect`，由泵经 `VmSignal::SettingApplied` 转投。

8. **界面尺寸/颜色默认值一律 `ui/constant.rs`**（2026-09-21 用户要求）：窗口与对话框默认/最小尺寸、横条高、列宽、图标像素档、圆角、遮罩透明度、行高基准…全部只有一处定义；业务代码不得再写 `px(420.)` 这类字面量（`px(0.)` 显式清零除外，见 `constant.rs` 模块文档）。分工：**随布局密度缩放的间距/内边距/行高**走 `ui/spacing.rs`（`metrics()`/`sp()`），**颜色**走 `cx.theme()`；`constant.rs` 只放「基准值 × 密度系数」里的**基准值**（如 `CARD_W_BASE`、`ROW_BAND_BASE`）与不属于主题的裸色（`SCRIM`/`TRANSPARENT_BLACK`）。

## 5. 新代码放置决策树

```
这段代码是什么？
├─ 纯业务逻辑/状态机（无 gpui 类型）
│   ├─ 浏览数据/条目/过滤/树状态        → viewmodel/main_vm.rs
│   ├─ 任务队列/进度/取消语义           → viewmodel/jobs_vm.rs
│   ├─ 压缩/解压参数 → JobSpec、密码/冲突状态机 → viewmodel/dialog_vm.rs、router.rs
│   └─ 行数据结构                      → viewmodel/rows.rs
├─ 进程/系统级（后台线程、文件锁、原生对话框、通知触发）
│   ├─ 后台任务泵/信号转发/完成通知      → app/pump_host.rs
│   ├─ 单实例/退出钩子/原生选择器        → app/{instance,exit,file_picker}.rs
│   ├─ AppData 状态聚合/设置持久化      → app/state.rs
│   ├─ 设置项定义（load/store/effect/默认值）→ app/settings_registry.rs
│   ├─ 设置项标识（注册表键）           → viewmodel/setting_id.rs
│   └─ 设置项文案↔落盘值映射            → app/labels.rs
├─ UI（要渲染或接事件）
│   ├─ 跨页面共享类型/快照/事件/纯派生    → ui/contracts.rs
│   ├─ VM 行 → 展示行派生/文本格式化     → ui/binding.rs
│   ├─ 界面尺寸/颜色默认值（窗口/对话框/列宽/圆角…）→ ui/constant.rs
│   ├─ 间距/内边距/行高（随布局密度缩放）   → ui/spacing.rs
│   ├─ 主题色板                          → ui/theme.rs
│   ├─ 主窗口骨架/工具栏分发/启动序列     → ui/shell/
│   ├─ 窗口横条（菜单/工具栏/页签）       → ui/bar/
│   ├─ 整页内容                          → ui/views/
│   ├─ 对话框                            → ui/dialogs/<逻辑名>.rs
│   └─ gpui-kit 没有的自绘件            → ui/widgets/
├─ 测试
│   ├─ 某个文件的测试                    → 同目录 `tests/<文件名>.rs` + 业务文件留 `#[cfg(test)] #[path] mod tests;`
│   ├─ 目录级/跨文件契约测试             → 同目录 `tests.rs`（或 `tests/mod.rs`）
│   └─ 跨模块集成测试                    → crate 根 `src/tests/mod.rs`
└─ 图标资源                              → assets/icons/*.svg + ui/assets.rs + widgets/icon.rs 登记
```

## 6. 结构演变记录

| 轮次 | 变更 |
|---|---|
| T-OPEN1（R-GUI2-06） | 新增 `views/open_progress.rs`（「正在打开」窗口：`open`/`finish`/`tick`/`close_all`，终局延迟关窗与 `progress` 同款 `close_at` + 泵 tick）；`ui/shell/mod.rs` 收口 `run_open_flow`/`open_archive_task`（打开重活移出 UI 线程且**不持 AppData 锁**，按需弹窗）；`AppData` 新增嵌套会话栈 `open_stack`/`current_temp`/`temp_area`（双击容器进入 = 压栈换会话，`..`/面包屑 = 弹栈还原）；`Crumb.level` 承载跨层面包屑 |
| T-PW | 新增 `views/progress/`（mod.rs 窗口实体 + Global 注册表 + 泵分发；file_list.rs 虚拟列表）；`JobRow` 扩展 `current_entry`/`recent_entries`；提交点（工具栏/argv 意图）+ 任务中心双击重开接线；设置页「进度窗口」开关（[jobs].progress_window） |
| T-SET | 设置流程注册表化：新增 `app/settings_registry.rs`（`SettingDef` 常量注册表 + `SettingValue`/`WidgetKind`/`Effect` + `SettingsView` 活读 + `ACTIONS` 旁路表）与 `viewmodel/setting_id.rs`（`SettingId`，仅依赖 `key()` 派生 gpui 元素 id）；`ui/views/settings_sections.rs` 由 `settings_render.rs` 取代（七页手写装配 → 注册表驱动通用渲染器）；`settings_layout.rs` 改为活读快照 + 事件后刷新；`AppData::apply_setting` 收口五次流程 `normalize → store → persist → dispatch → refresh`（落盘恰好一次、`cfg!(test)` 不落盘守卫）；`VmSignal::SettingApplied(SettingId)` 承载 UI 半场（主题/列序）生效；旧 `Settings*` 事件与 `persist_setting`/`persist_config`/`prefs_mapping` 全删 |
| T-MENU | 菜单栏完善：`bar/menu_bar.rs` 重写为 `MenuContext` 投影（六组菜单/勾选态/子菜单/`menu_epoch` 第三重建条件）；新增 `dialogs/about.rs`（关于对话框）与 `dialogs/edit_confirm.rs`（编辑确认族）；新增 `shell/windows.rs`（主窗口注册表——多窗口信号按窗口路由 + 退出语义）；`shell/runtime.rs` 抽 `spawn_main_window` 窗口工厂（每窗口独立 `AppData` + 独立信号通道，共享 `Engine`）并注册菜单加速键；`views/browse_layout.rs` 增加 icon/list 两渲染模式（列设置仅 detail 消费）；`hzip-platform/clipboard.rs` 新增文件列表剪贴板（内部剪贴板 + 系统 CF_HDROP 只读，B3 降级线）；`hzip-core` 新增 `JobKind::EditBatch` + `JobSpec.changes` 批量编辑提交入口 |

| T-STD（2026-10-08 用户口径） | 两条组织规范落地并机检：① **`mod.rs` 只做模块声明/再导出**——`ui/shell/mod.rs` 由 3,308 行业务拆为 `state/app_shell/render/file_ops/open_flow/clipboard_ops/session/errors` 等文件，`views/progress/mod.rs`、`dialogs/mod.rs`、`hzip-core/jobs/mod.rs`、`hzip-platform/{context_menu,assoc,linux}/mod.rs`、`engine/mod.rs`、`services/mod.rs`、`filter/mod.rs` 同步收口；② **测试代码迁出业务文件**——全仓 143 个内联 `mod tests` 迁到「同目录 `tests/` 子目录 + `#[cfg(test)] #[path]` 声明」（测试仍是该业务文件的子模块，零可见性放宽），测试模块名统一为 `tests`，crate 根新增 `src/tests/mod.rs` 跨模块索引。守卫：`build/checks/layout-conventions.py`（CI `quality`） |
| T-ICON（2026-10-10） | 文件列表四处（明细名称列 / 图标网格 / 紧凑列表 / 左栏树）**条目图标**改取系统文件管理器同款图标（设置项 `ui.file_type_icons = system` 默认，`builtin` 零平台调用）：新增 `ui/widgets/file_icon.rs`（`(kind, ext)` 缓存 512 条 FIFO + 1 个 worker 线程 + 负缓存；渲染帧零平台调用）与 `VmSignal::IconCacheFilled`（只重绘、绝不触发行重建，与 `BrowseRowsRebuilt` 同款排除 `needs_browse_refresh`）；`hzip-platform` 新增 `file_icon`/`lang` 模块。**views 调用 widgets，回退映射仍在 views**（`row_type_icon`/`tree_row_icon` 为唯一回退来源） |

未竟事项：T22（设置页 Form → gpui-kit `setting` 模块，白送分组标题/搜索关键词/行内恢复默认）——T-SET 选型为「常量注册表 + 自绘渲染器」（可穷举、可静态断言、无第三方页模型约束），仅在需要「设置项搜索」时才重新评估该挂账；见 [T-GUI README](../tasks/T-GUI/README.md) §6 任务表。
