//! 右键菜单的**后端分发**：`backend()` 按编译期选定本平台实现，另有 verb 查表、
//! 配置路径与**宿主侧入口**（扩展意图 watch / macOS Services provider 注册）。
//!
//! 类型与 trait 在 `api.rs`；各平台后端同目录 `win.rs` / `macos.rs` / `linux/`。
//!
//! # 为什么宿主侧入口放在这里（而不是 `mod.rs`）
//!
//! `mod.rs` 按仓库红线 L1 只做模块声明与再导出（`build/checks/layout-conventions.py`）；
//! 而这两个入口都是「按平台分发」的薄壳——`watch_extension_intents` 取
//! [`backend()`] 的意图队列路径，`register_services_provider` 转发到 macOS 实现——
//! 与本文件的职责同源。模块路径（`crate::context_menu::*`）由 `mod.rs` 的再导出保持不变。

use std::path::PathBuf;

use super::api::ContextMenu;
/// 无后端平台（`cross-check` 语法校验期占位）的实现体用到的类型——
/// 条件编译下才存在，故 import 同样条件化（否则本机 `-D warnings` 报 unused）。
#[cfg(not(any(windows, target_os = "macos", unix)))]
use super::api::{InstallReport, MenuError, MenuErrorKind, MenuSpec, MenuStatus};

/// 右键菜单配置落盘路径（Windows 的 DLL 与 GUI 共用，**两端必须一致**）。
///
/// Windows：`%APPDATA%\hzip\shell-menu.json`（= [`crate::dirs::config_dir`]）；
/// 其余平台仅作调试/状态回读用。
///
/// # 与 DLL 的约定
///
/// DLL 侧经 `SHGetKnownFolderPath(FOLDERID_RoamingAppData)` 拼 `hzip\shell-menu.json`，
/// 语义等同 [`crate::dirs::config_dir`]（win 收敛为 `%APPDATA%\hzip`）。
/// **不使用** `HZIP_CONFIG` 环境变量覆写（DLL 在 Explorer 进程内无法读取调用方环境）。
pub fn menu_config_path() -> PathBuf {
    crate::dirs::config_dir().join("shell-menu.json")
}

/// 当前平台的右键菜单后端（编译期选定；调用方只见 trait）。
pub fn backend() -> &'static dyn ContextMenu {
    #[cfg(target_os = "windows")]
    {
        &super::win::BACKEND
    }
    #[cfg(target_os = "macos")]
    {
        &super::macos::BACKEND
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        super::linux::backend()
    }
    // feature `cross-check`（CI/开发期跨平台语法校验）下本机后端不存在——
    // 此时不该有人调用 backend()；给出显式不可用实现而非编译失败。
    #[cfg(not(any(windows, target_os = "macos", unix)))]
    {
        &Unsupported
    }
}

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

/// 注册 macOS Services provider（幂等；仅主界面模式、`NSApplication` 起来后调用）。
///
/// 必须在 Info.plist 声明 `NSServices` 之后才有意义——否则系统不会派发这些
/// selector（条目不存在，注册了也没人调）。非 macOS 为 no-op。
pub fn register_services_provider() {
    #[cfg(target_os = "macos")]
    super::macos_services::register_services_provider();
}

/// 无后端平台（`cross-check` 语法校验期占位；正常运行期不可达）。
#[cfg(not(any(windows, target_os = "macos", unix)))]
struct Unsupported;

#[cfg(not(any(windows, target_os = "macos", unix)))]
impl ContextMenu for Unsupported {
    fn install(&self, _spec: &MenuSpec) -> Result<InstallReport, MenuError> {
        Err(MenuError::new(
            MenuErrorKind::Unsupported,
            "当前平台暂不支持右键菜单集成",
        ))
    }
    fn uninstall(&self) -> Result<(), MenuError> {
        Err(MenuError::new(
            MenuErrorKind::Unsupported,
            "当前平台暂不支持右键菜单集成",
        ))
    }
    fn status(&self) -> MenuStatus {
        MenuStatus::Unsupported {
            reason: "当前平台暂不支持右键菜单集成".to_string(),
        }
    }
}
