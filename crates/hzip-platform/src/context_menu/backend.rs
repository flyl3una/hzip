//! 右键菜单的**后端分发**：`backend()` 按编译期选定本平台实现，另有 verb 查表与配置路径。
//!
//! 类型与 trait 在 `api.rs`；各平台后端同目录 `win.rs` / `macos.rs` / `linux/`。

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
