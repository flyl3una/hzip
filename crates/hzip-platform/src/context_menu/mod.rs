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
    COMPONENT_CLSID, ContextMenu, EntrySpec, ExtensionIntent, InstallMode, InstallReport, MenuEntry,
    MenuError, MenuErrorKind, MenuScope, MenuSpec, MenuStatus, VERBS, entry_for,
};
pub use backend::{backend, menu_config_path};

/// 监听 Finder 扩展意图队列（macOS appex/Services → 宿主）。
///
/// # 为什么宿主必须自己监听
///
/// appex（沙盒）无法向**已运行**的宿主注入 argv，也无法发通知——它只能把意图
/// 追加进 App Group 容器的 `intents.jsonl`。故宿主必须监听该文件：变更 → 去抖
/// 回调 → 调用方消费（[`ContextMenu::take_extension_intents`]）。这是「宿主已
/// 运行时点右键菜单依然生效」的唯一途径（对应 `hzip-forward-queue` 的运行期
/// watch；缺它则运行期点击是死信）。
///
/// 非 macOS 恒 `None`。返回 `None` 也意味着「该平台无此轨」，调用方据此 no-op。
pub fn watch_extension_intents(
    on_change: impl Fn(std::path::PathBuf) + Send + 'static,
) -> Option<crate::watch::FileWatch> {
    let path = backend().extension_intents_path()?;
    // watch 目标可能尚不存在（容器目录也可能不存在）：先建目录 + 空文件。
    // `FileWatch` 监听父目录 + 路径过滤，目标不存在时事件匹配不上。
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        tracing::warn!("创建 Finder 扩展意图容器失败（watch 不生效）: {e}");
        return None;
    }
    if !path.exists()
        && let Err(e) = std::fs::File::create(&path)
    {
        tracing::warn!("创建 Finder 扩展意图队列失败（watch 不生效）: {e}");
        return None;
    }
    match crate::watch::FileWatch::new(&path, Box::new(on_change)) {
        Ok(w) => {
            tracing::debug!("Finder 扩展意图 watch 已挂载: {}", path.display());
            Some(w)
        }
        Err(e) => {
            tracing::warn!("Finder 扩展意图 watch 挂载失败: {e}");
            None
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) mod win;

// `cross-check` 在本机（非目标平台）编译这些后端只为语法校验：其中绝大多数
// 符号不会被本机 `backend()` 引用，故仅在「非目标平台」这一编译条件下豁免死代码。
#[cfg(any(target_os = "macos", feature = "cross-check"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod macos;

/// macOS 服务菜单（Services）轨的宿主侧 provider 注册；仅 macOS。
#[cfg(target_os = "macos")]
mod macos_services;

/// 注册 macOS Services provider（幂等；仅主界面模式、`NSApplication` 起来后调用）。
///
/// 必须在 Info.plist 声明 `NSServices` 之后才有意义——否则系统不会派发这些
/// selector（条目不存在，注册了也没人调）。非 macOS 为 no-op。
pub fn register_services_provider() {
    #[cfg(target_os = "macos")]
    macos_services::register_services_provider();
}

#[cfg(any(all(unix, not(target_os = "macos")), feature = "cross-check"))]
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub(crate) mod linux;
