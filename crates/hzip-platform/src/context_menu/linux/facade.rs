//! Linux 后端的桌面探测与 `ContextMenu` 实现（GNOME / KDE / XFCE 分派）。
//!
//! 桌面专有落盘见 `dolphin`/`nautilus`/`script`/`thunar`；模块路径
//! （`crate::context_menu::linux::*`）由 `linux/mod.rs` 的再导出保持不变。

use std::path::PathBuf;

// 兄弟模块（`linux/mod.rs` 里与 facade 同级声明；桌面专属落盘 + 转发脚本）
use super::{dolphin, nautilus, script, thunar};

use crate::context_menu::{
    ContextMenu, InstallMode, InstallReport, MenuError, MenuErrorKind, MenuSpec, MenuStatus,
};

/// 后端单例（[`crate::context_menu::backend`] 返回其 trait object）。
static BACKEND: Linux = Linux;

/// Linux 后端（按探测到的桌面分派）。
pub(super) struct Linux;

/// 已知桌面家族。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    /// GNOME / Nautilus（含 Ubuntu 的 `ubuntu:GNOME`）。
    Gnome,
    /// KDE Plasma / Dolphin。
    Kde,
    /// XFCE / Thunar。
    Xfce,
}

impl Desktop {
    /// 面向用户的名字。
    fn name(self) -> &'static str {
        match self {
            Self::Gnome => "GNOME / Nautilus",
            Self::Kde => "KDE Plasma / Dolphin",
            Self::Xfce => "XFCE / Thunar",
        }
    }
}

/// 探测当前桌面 **并**带回原始的 `XDG_CURRENT_DESKTOP` 值。
///
/// 探测与「探测失败说明」都要读这个环境变量；分开读会让一次 `status`/`install`
/// 调 `env::var` 两遍。故这里读一次、两处消费（`None` = 未设置/非 UTF-8，
/// 由调用方交给 [`unsupported_reason`] 决定文案）。
pub(super) fn detect_with_env() -> (Option<Desktop>, Option<String>) {
    let current = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let session = std::env::var("DESKTOP_SESSION").unwrap_or_default();
    let from_env = detect_from(current.as_deref().unwrap_or_default(), &session);
    (from_env.or_else(detect_from_dirs), current)
}

/// 按环境变量判定（纯函数，便于单测）。
pub(super) fn detect_from(current: &str, session: &str) -> Option<Desktop> {
    let hay = format!("{current}:{session}").to_ascii_lowercase();
    // 顺序有意：KDE/XFCE 的标识比 GNOME 更独特，先判它们可避免
    // 某些发行版把中性词也塞进 DESKTOP_SESSION（如 `plasma` 与 `gnome-xorg`）
    if hay.contains("kde") || hay.contains("plasma") {
        return Some(Desktop::Kde);
    }
    if hay.contains("xfce") {
        return Some(Desktop::Xfce);
    }
    if hay.contains("gnome") || hay.contains("ubuntu") || hay.contains("nautilus") {
        return Some(Desktop::Gnome);
    }
    None
}

/// 兜底：按桌面专属数据目录是否存在判定。
fn detect_from_dirs() -> Option<Desktop> {
    let base = crate::dirs::data_dir();
    let share = base.parent().unwrap_or(&base).to_path_buf();
    let has = |p: PathBuf| -> bool { p.is_dir() };
    if has(share.join("kio")) || has(share.join("kservices5")) {
        return Some(Desktop::Kde);
    }
    if has(share.join("nautilus-python")) || has(share.join("nautilus")) {
        return Some(Desktop::Gnome);
    }
    if has(crate::dirs::config_dir()
        .parent()
        .unwrap_or(&base)
        .join("Thunar"))
    {
        return Some(Desktop::Xfce);
    }
    None
}

/// 探测失败时的统一说明（面向用户，可直接展示）。
///
/// `current` = 探测时读到的原始 `XDG_CURRENT_DESKTOP`（`None` = 未设置/非 UTF-8）；
/// 由调用方传入以免同一次查询重复读环境变量。
pub(super) fn unsupported_reason(current: Option<&str>) -> String {
    let cur = current.unwrap_or("（未设置）");
    format!(
        "当前桌面环境暂未支持（XDG_CURRENT_DESKTOP={cur}）。\
         已支持：GNOME/Nautilus、KDE Plasma/Dolphin、XFCE/Thunar"
    )
}

impl ContextMenu for Linux {
    fn install(&self, spec: &MenuSpec) -> Result<InstallReport, MenuError> {
        let (desktop, current) = detect_with_env();
        let Some(desktop) = desktop else {
            return Err(MenuError::new(
                MenuErrorKind::Unsupported,
                unsupported_reason(current.as_deref()),
            ));
        };
        if spec.is_empty() {
            return Err(MenuError::new(MenuErrorKind::Spec, "未勾选任何右键菜单项"));
        }
        if !spec.exe_path.is_file() {
            return Err(MenuError::new(
                MenuErrorKind::Spec,
                format!("可执行文件不存在：{}", spec.exe_path.display()),
            ));
        }

        // 共享步：写转发脚本（各桌面的菜单项都指向它）
        let written = script::write_scripts(spec)?;

        // 桌面专属步
        let items = match desktop {
            Desktop::Gnome => nautilus::install(spec)?,
            Desktop::Kde => dolphin::install(spec)?,
            Desktop::Xfce => thunar::install(spec)?,
        };

        tracing::info!(
            desktop = desktop.name(),
            scripts = written,
            items,
            "右键菜单已安装（需重启文件管理器生效）"
        );
        Ok(InstallReport {
            mode: InstallMode::Desktop,
            items,
            restart_required: true,
        })
    }

    fn uninstall(&self) -> Result<(), MenuError> {
        // 清除**全部已知桌面**的痕迹：用户可能换过桌面，或探测期与安装期不同
        let mut details = Vec::new();
        details.extend(nautilus::uninstall());
        details.extend(dolphin::uninstall());
        details.extend(thunar::uninstall());
        script::remove_scripts();

        if details.is_empty() {
            tracing::info!("右键菜单已清除（Linux）");
            Ok(())
        } else {
            Err(MenuError {
                kind: MenuErrorKind::Io,
                details,
            })
        }
    }

    fn status(&self) -> MenuStatus {
        let (desktop, current) = detect_with_env();
        let Some(desktop) = desktop else {
            return MenuStatus::Unsupported {
                reason: unsupported_reason(current.as_deref()),
            };
        };
        let installed = match desktop {
            Desktop::Gnome => nautilus::installed(),
            Desktop::Kde => dolphin::installed(),
            Desktop::Xfce => thunar::installed(),
        };
        if !installed {
            return MenuStatus::NotInstalled;
        }
        MenuStatus::NeedsRestart
    }
}
