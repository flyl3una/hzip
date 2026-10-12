//! gpui_kit 薄壳装配层（T-G 系列；02 §11.1 分层的 UI 半场）。
//!
//! gpui 无生成代码（原旧框架 `include_modules!` 已随迁移删除）：本模块直接
//! 声明 UI 子模块；窗口/组件全部为 gpui_kit 手写装配。目录职责与分层
//! 约定见 `docs/design/07-GUI代码组织与分层约定.md`（T-GUI 结构重组）。
//!
//! # 模块图（T-GUI T10/T11 结构重组后）
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`shell`] | AppShell 主窗口骨架（chrome 渲染；dispatch/prompt/runtime 分片） |
//! | [`bar`] | 窗口级横条（菜单条/工具栏/页签条） |
//! | [`views`] | 页面视图（浏览/任务中心/预览/设置） |
//! | [`widgets`] | 自绘 widgets（gpui-kit 没有：图标/状态/过滤行/面包屑） |
//! | [`dialogs`] | 四对话框纯 props 组件（按对话框分文件） |
//! | [`theme`] | 主题 token → ActiveTheme（focus/selection 统一） |
//! | [`spacing`] | 布局密度（紧凑/标准/宽松）+ 间距刻度 `sp()` |
//! | [`constant`] | 界面尺寸/颜色**默认值常量表**（布局字面量的唯一定义处） |
//! | [`binding`] | VM 行 → UI 行派生 |
//! | [`contracts`] | 契约类型总表（快照/事件/回调——app 引用 ui 的唯一例外面） |
//! | [`assets`] | RustEmbed 图标资源装配 |
//!
//! # 红线（02 §11.1 平移）
//!
//! 本模块**可** import gpui/gpui_kit；[`crate::viewmodel`] 与 [`crate::app`]
//! **禁止**（验收 grep `! grep -rEn "use (gpui|gpui_kit)" src/viewmodel/ src/app/`）。
//! 依赖方向：ui → app → viewmodel 单向。
//!
//! # lint 豁免
//!
//! 默认不设（T-G00 §2.2）；Spike 实测 gpui-kit 公开 API 迫使调用方 unwrap
//! 时恢复**最小范围**豁免并在此注明具体 API。

pub mod assets;
pub mod bar;
pub mod binding;
pub mod constant;
pub mod contracts;
pub mod dialogs;
pub mod notify;
pub mod shell;
pub mod spacing;
pub mod theme;
pub mod views;
pub mod widgets;
