//! Linux 右键菜单：桌面探测与后端分派（T-CM §5）。
//!
//! # 为什么必须探测
//!
//! Linux 没有统一的文件管理器菜单机制——GNOME 用 nautilus-python/脚本、KDE 用
//! KIO servicemenu、XFCE 用 UCA。同一台机器可能装了多个桌面，但**当前会话**只有一个。
//!
//! # 探测顺序
//!
//! 1. `XDG_CURRENT_DESKTOP`（冒号分隔，如 `KDE`、`ubuntu:GNOME`、`XFCE`）
//! 2. `DESKTOP_SESSION`（如 `plasma`、`xfce`、`gnome`）
//! 3. 兜底按数据目录存在性判定（如 `~/.local/share/kio` 存在 → KDE）
//!
//! # 宁可不写
//!
//! 探测失败（未知桌面）→ [`MenuStatus::Unsupported`]，**不向任何位置写文件**。
//! 写错位置的后果（污染别的桌面配置、菜单不出现且无法自查）比不写更糟。

mod dolphin;
mod facade;
mod nautilus;
mod script;
mod thunar;

// 非测试构建唯一的跨文件取用方：`crate::context_menu::backend()`（Linux 分支）。
// 门控条件与那个调用点一致——`cross-check`（在非 Linux 主机上编译本模块）时
// 没人取用它，留着会是 unused import。
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) use facade::backend;

// 搬进 `facade` 的项在此再导出：当前唯一的跨文件取用方是 `tests/mod_rs.rs`
// （`use super::*`），故只随测试编译存在——非测试构建里这些名字在 `linux`
// 命名空间中没有使用者（`-D warnings` 会以 unused import 拦下）。
#[cfg(test)]
use facade::{Desktop, detect_from, unsupported_reason};

#[cfg(test)]
#[path = "tests/mod_rs.rs"]
mod tests;
