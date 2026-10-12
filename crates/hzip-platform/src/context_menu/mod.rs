//! 资源管理器右键上下文菜单（T-CM；三平台 trait 抽象 + OS 实现隔离）。
//!
//! # 职责
//!
//! 把「勾选哪些解压/压缩项」变成操作系统文件管理器里真实可见的右键菜单，并
//! 提供对称的撤销能力。**本模块是右键菜单的唯一入口**；[`crate::assoc`] 只负责
//! 文件关联（ProgId / OpenWithProgids），不再持有任何 verb 语义。
//!
//! # 分层与隔离（T-CM §7）
//!
//! ```text
//! 调用方（hzip-gui） ──► backend() -> &dyn ContextMenu
//!                              │
//!        ┌─────────────────────┼─────────────────────┐
//!      win.rs               macos.rs            linux/{nautilus,dolphin,thunar}
//!   注册表 + 稀疏包        pluginkit/Services    ext / servicemenus / uca.xml
//! ```
//!
//! 各平台实现互不引用，仅共享本文件的类型与 [`entry_for`] 表。
//!
//! # 单一事实源
//!
//! `key → (argv 模板, 适用范围, Windows 是否跳过)` 只在 [`VERBS`] 定义一次；
//! `hzip-gui` 只持有 `key → i18n label`（`CTX_*_KEYS`）。两侧靠
//! `hzip-gui` 的一致性测试与 [`tests/context_menu.rs`] 互锁。
//!
//! # 占位符（仅非 Windows 后端消费）
//!
//! | 占位符 | 语义 |
//! |---|---|
//! | `%SEL` | 每个选中项各启动一次进程 |
//! | `%SELS` | 一次进程、全部选中项依次插入（背景语境 = 当前目录） |
//!
//! Windows 侧经 COM `IShellItemArray` 直接取选中集，不使用占位符。

mod api;
mod backend;

pub use api::{
    COMPONENT_CLSID, ContextMenu, EntrySpec, InstallMode, InstallReport, MenuEntry, MenuError,
    MenuErrorKind, MenuScope, MenuSpec, MenuStatus, VERBS, entry_for,
};
pub use backend::{backend, menu_config_path};

#[cfg(target_os = "windows")]
pub(crate) mod win;

// `cross-check` 在本机（非目标平台）编译这些后端只为语法校验：其中绝大多数
// 符号不会被本机 `backend()` 引用，故仅在「非目标平台」这一编译条件下豁免死代码。
#[cfg(any(target_os = "macos", feature = "cross-check"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod macos;

#[cfg(any(all(unix, not(target_os = "macos")), feature = "cross-check"))]
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub(crate) mod linux;
