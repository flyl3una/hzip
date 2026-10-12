//! macOS 右键菜单（T-CM §4）：Finder Sync 扩展轨 + 服务菜单轨。
//!
//! # 参照实现：Bandizip for macOS
//!
//! Bandizip 的实际做法（官方帮助页实证）是双轨：
//!
//! | 轨 | 菜单位置 | 用户如何启用 |
//! |---|---|---|
//! | **Finder Sync 扩展**（`.appex`） | 右键**顶层**「hzip」子菜单；**仅限已授权目录** | 系统设置 → 隐私与安全性 → 扩展 → Finder 扩展；Sequoia 起该系统设置已移除 → 用 `pluginkit -e use` |
//! | **Services**（服务） | 右键底部「服务」子菜单；**全局** | 系统设置 → 键盘 → 键盘快捷键 → 服务 |
//!
//! # 本后端的职责边界（必须清楚）
//!
//! `.appex` 是 **Xcode target，必须内嵌在 `.app` 包内**——库无法在运行时「安装」
//! 它。故本模块只能：
//!
//! 1. `lsregister` 注册 app 与内嵌 appex（让系统发现）；
//! 2. `pluginkit -e use/ignore` 启用/停用扩展（**Sequoia 起的唯一途径**）；
//! 3. 写 App Group 共享偏好（条目集 + 标签，appex 读它决定展示什么）；
//! 4. 翻转 `NSServicesStatus`（服务轨可见性）。
//!
//! **若发行包未内嵌 appex**（或无宿主 app），只能落到 Services 轨，
//! [`status`] 返回 `Degraded` 并说明。
//!
//! # 硬约束（产品面）
//!
//! Finder Sync **只对 extension 自己注册的监控目录**提供顶层菜单（沙盒限制），
//! 不给「任意文件夹」——故 Bandizip 才需要「可访问目录」设置项。本卡不做该
//! 设置面（见任务卡 §9 非目标），appex 侧可先注册用户主目录。
//!
//! # 调试
//!
//! 环境变量 `HZIP_DRY_RUN=1` 时只打印将要执行的命令、不实际调用外部工具
//! （无 macOS 的开发机可据此做语法/流程评审；正式路径不受影响）。

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{
    ContextMenu, InstallMode, InstallReport, MenuError, MenuErrorKind, MenuSpec, MenuStatus,
};

/// 后端单例（[`super::backend`] 返回其 trait object）。
#[allow(non_upper_case_globals)]
pub(super) static BACKEND: Mac = Mac;

/// macOS 后端。
pub(super) struct Mac;

/// Finder Sync 扩展的 bundle id（`pluginkit` 用；**必须与 appex 的 Info.plist 一致**）。
pub(super) const EXTENSION_ID: &str = "com.hzip.mac.findersyncextension";

/// 宿主 app bundle id（`lsregister` 与 App Group 命名用）。
pub(super) const APP_ID: &str = "com.hzip.mac";

/// App Group 标识（共享偏好容器；app 与 appex 必须在同一组）。
///
/// appex（`hzipFinderSync.appex`）侧读取共享容器时使用；该 target 属 macOS 打包
/// 产物（阶段 C），不在本 workspace 编译，故此处暂为未被引用的**跨产物契约常量**
/// ——它必须与 appex 的 entitlements/Info.plist 保持一致，不可删除。
#[allow(dead_code)]
pub(super) const APP_GROUP: &str = "com.hzip.mac.shared";

/// 共享偏好域名（`defaults write` 用；App Group 容器内的 plist 名）。
pub(super) const PREFS_DOMAIN: &str = "com.hzip.mac.shared";

/// 共享偏好键：菜单条目 JSON。
pub(super) const KEY_ITEMS: &str = "ContextMenuItems";

/// 共享偏好键：监听目录。
pub(super) const KEY_FOLDERS: &str = "MonitoredFolders";

/// 是否干跑（只打印命令；无 macOS 的开发机评审用）。
fn dry_run() -> bool {
    std::env::var("HZIP_DRY_RUN").is_ok_and(|v| v == "1")
}

impl ContextMenu for Mac {
    fn install(&self, spec: &MenuSpec) -> Result<InstallReport, MenuError> {
        if spec.is_empty() {
            return Err(MenuError::new(MenuErrorKind::Spec, "未勾选任何右键菜单项"));
        }
        if !spec.exe_path.is_file() {
            return Err(MenuError::new(
                MenuErrorKind::Spec,
                format!("可执行文件不存在：{}", spec.exe_path.display()),
            ));
        }

        // ① 共享偏好：条目集（appex 与 Services 都读它）
        write_shared_prefs(spec)?;

        // ② 服务轨（Services）：始终尽力启用（全局可用，不依赖 appex）
        let services_ok = match enable_services() {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Services 轨启用失败（不影响 Finder 扩展轨）：{e}");
                false
            }
        };

        // ③ Finder Sync 轨：只有宿主 app 带内嵌 appex 时才能注册
        let app_bundle = app_bundle_path(&spec.exe_path);
        let appex_present = app_bundle.as_ref().is_some_and(|b| appex_path(b).is_dir());

        if !appex_present {
            if !services_ok {
                return Err(MenuError::new(
                    MenuErrorKind::Unsupported,
                    "未找到 Finder 扩展（hzip.app/Contents/PlugIns）且服务菜单不可用",
                ));
            }
            tracing::warn!("未内嵌 Finder 扩展，仅服务菜单轨生效（右键底部「服务」子菜单）");
            return Ok(InstallReport {
                mode: InstallMode::Services,
                items: spec.resolved().count(),
                restart_required: true,
            });
        }

        let Some(bundle) = app_bundle else {
            return Err(MenuError::new(
                MenuErrorKind::Unsupported,
                "未找到 hzip.app 包（无法注册 Finder 扩展）",
            ));
        };
        run_tool("lsregister", &["-f", &bundle.to_string_lossy()])
            .map_err(|e| MenuError::new(MenuErrorKind::Platform, e))?;
        // Sequoia 起系统设置已无 Finder 扩展开关 → pluginkit 是唯一途径
        run_tool("pluginkit", &["-e", "use", "-i", EXTENSION_ID])
            .map_err(|e| MenuError::new(MenuErrorKind::Platform, e))?;

        tracing::info!(
            items = spec.resolved().count(),
            "右键菜单已安装（Finder 扩展 + 服务菜单）"
        );
        Ok(InstallReport {
            mode: InstallMode::FinderSync,
            items: spec.resolved().count(),
            restart_required: true,
        })
    }

    fn uninstall(&self) -> Result<(), MenuError> {
        let mut errors = Vec::new();

        if let Err(e) = run_tool("pluginkit", &["-e", "ignore", "-i", EXTENSION_ID]) {
            errors.push(e);
        }
        if let Err(e) = disable_services() {
            errors.push(e);
        }
        if let Err(e) = clear_shared_prefs() {
            errors.push(e);
        }

        if errors.is_empty() {
            tracing::info!("右键菜单已清除（macOS）");
            Ok(())
        } else {
            Err(MenuError {
                kind: MenuErrorKind::Platform,
                details: errors,
            })
        }
    }

    fn status(&self) -> MenuStatus {
        let plugin = plugin_state();
        let has_appex = appex_path_from_current_exe().is_some();

        match (plugin, has_appex) {
            (PluginState::Enabled, true) => MenuStatus::Installed {
                mode: InstallMode::FinderSync,
                items: read_item_count(),
            },
            (PluginState::Enabled, false) => MenuStatus::Installed {
                mode: InstallMode::Services,
                items: read_item_count(),
            },
            (PluginState::Disabled | PluginState::Unknown, _) if services_enabled() => {
                MenuStatus::Degraded {
                    reason: "Finder 扩展未启用，当前仅右键底部「服务」子菜单可用\
                             （可在系统设置 → 键盘 → 键盘快捷键 → 服务中启用，\
                             或运行 pluginkit -e use -i com.hzip.mac.findersyncextension）"
                        .to_string(),
                }
            }
            _ => MenuStatus::NotInstalled,
        }
    }
}

// ---------------------------------------------------------------- 路径

/// 宿主 app bundle（`…/hzip.app`）：从可执行文件路径向上找到 `.app`。
pub(super) fn app_bundle_path(exe: &Path) -> Option<PathBuf> {
    let mut cur = exe;
    loop {
        let name = cur.file_name()?.to_string_lossy().to_string();
        if name.ends_with(".app") {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

/// appex 路径（`Contents/PlugIns/hzipFinderSync.appex`）。
pub(super) fn appex_path(bundle: &Path) -> PathBuf {
    bundle
        .join("Contents")
        .join("PlugIns")
        .join("hzipFinderSync.appex")
}

/// 当前可执行文件对应的 appex（`status` 用）。
fn appex_path_from_current_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundle = app_bundle_path(&exe)?;
    appex_path(&bundle).is_dir().then_some(bundle)
}

// ---------------------------------------------------------------- 共享偏好

/// 写入 App Group 共享偏好（appex 读它）。
///
/// 用 `defaults write <domain> <key> -string <json>`；`defaults` 写的是
/// `~/Library/Preferences/<domain>.plist`，与 App Group 容器路径不同——
/// appex 侧应优先读 App Group 容器，容器不可得时回退该域（两者内容一致）。
fn write_shared_prefs(spec: &MenuSpec) -> Result<(), MenuError> {
    let items: Vec<serde_json::Value> = spec
        .resolved()
        .map(|(entry, def)| {
            serde_json::json!({
                "key": entry.key,
                "label": entry.label,
                "args": def.argv,
                "scopes": def.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let json = serde_json::to_string(&items)
        .map_err(|e| MenuError::new(MenuErrorKind::Spec, format!("序列化菜单条目失败: {e}")))?;

    write_default(KEY_ITEMS, &json)?;
    write_default(KEY_FOLDERS, &default_monitored_folders())?;
    Ok(())
}

/// 默认监听目录（appex 只对这些目录提供顶层菜单）。
///
/// 无「可访问目录」设置面时取用户主目录——覆盖面最大且无需额外授权配置。
fn default_monitored_folders() -> String {
    let home = crate::dirs::config_dir()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"));
    format!("{}", home.display())
}

/// `defaults write` 一个字符串键。
fn write_default(key: &str, value: &str) -> Result<(), MenuError> {
    run_tool("defaults", &["write", PREFS_DOMAIN, key, "-string", value])
        .map(|_| ())
        .map_err(|e| MenuError::new(MenuErrorKind::Platform, e))
}

/// 清除共享偏好键（幂等）。
fn clear_shared_prefs() -> Result<(), String> {
    let mut errs = Vec::new();
    for key in [KEY_ITEMS, KEY_FOLDERS] {
        if let Err(e) = run_tool("defaults", &["delete", PREFS_DOMAIN, key]) {
            // 键不存在不算错
            if !e.contains("does not exist") {
                errs.push(e);
            }
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

/// 回读条目数（状态行）。
fn read_item_count() -> usize {
    let out = run_tool("defaults", &["read", PREFS_DOMAIN, KEY_ITEMS]).unwrap_or_default();
    serde_json::from_str::<Vec<serde_json::Value>>(&out)
        .map(|v| v.len())
        .unwrap_or(0)
}

// ---------------------------------------------------------------- Services 轨

/// 启用本 app 声明的全部 Services（`NSServicesStatus` 逐个置 `enabled`）。
///
/// Info.plist 里的 `NSServices` 声明属打包期产物；运行时通过
/// `defaults write pbs NSServicesStatus` 打开它们——这正是 Bandizip 帮助页
/// 「如何隐藏服务」描述的开关（用户可反向关掉）。
fn enable_services() -> Result<(), String> {
    let names = service_identifiers();
    let mut errs = Vec::new();
    for name in names {
        let key = format!("{APP_ID} - {name}");
        if let Err(e) = run_tool(
            "defaults",
            &[
                "write",
                "pbs",
                "NSServicesStatus",
                "-dict-add",
                &key,
                "{ enabled = 1; }",
            ],
        ) {
            errs.push(e);
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

/// 关闭本 app 的 Services（`enabled = 0`）。
fn disable_services() -> Result<(), String> {
    let mut errs = Vec::new();
    for name in service_identifiers() {
        let key = format!("{APP_ID} - {name}");
        if let Err(e) = run_tool(
            "defaults",
            &[
                "write",
                "pbs",
                "NSServicesStatus",
                "-dict-add",
                &key,
                "{ enabled = 0; }",
            ],
        ) {
            errs.push(e);
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

/// 当前是否至少有一个服务处于启用态。
fn services_enabled() -> bool {
    let Ok(out) = run_tool("defaults", &["read", "pbs", "NSServicesStatus"]) else {
        return false;
    };
    out.contains("enabled = 1") && out.contains(APP_ID)
}

/// 本 app 声明的 Service 名（与 Info.plist `NSServices` 的 `NSMenuItem` 对齐）。
pub(super) fn service_identifiers() -> Vec<&'static str> {
    vec![
        "Extract Here with hzip",
        "Extract To with hzip",
        "Compress with hzip",
    ]
}

// ---------------------------------------------------------------- pluginkit

/// 扩展启用状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PluginState {
    /// `pluginkit -m` 输出以 `+` 开头。
    Enabled,
    /// 以 `-` 开头。
    Disabled,
    /// 未注册 / 工具不可用。
    Unknown,
}

/// 解析 `pluginkit -m -i <id>` 的输出（`+`/`-` 前缀；纯函数便于单测）。
pub(super) fn parse_plugin_state(output: &str) -> PluginState {
    for line in output.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix('+') {
            if !rest.trim().is_empty() {
                return PluginState::Enabled;
            }
        } else if let Some(rest) = line.strip_prefix('-')
            && !rest.trim().is_empty()
        {
            return PluginState::Disabled;
        }
    }
    PluginState::Unknown
}

/// 查询扩展状态。
fn plugin_state() -> PluginState {
    let Ok(out) = run_tool("pluginkit", &["-m", "-i", EXTENSION_ID]) else {
        return PluginState::Unknown;
    };
    parse_plugin_state(&out)
}

// ---------------------------------------------------------------- 进程调用

/// 执行外部工具（`HZIP_DRY_RUN=1` 时只打印）。
fn run_tool(tool: &str, args: &[&str]) -> Result<String, String> {
    if dry_run() {
        tracing::info!("[dry-run] {tool} {}", args.join(" "));
        return Ok(String::new());
    }
    let out = Command::new(tool)
        .args(args)
        .output()
        .map_err(|e| format!("启动 {tool} 失败: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(format!(
            "{tool} 失败（退出码 {:?}）: {stderr}",
            out.status.code()
        ))
    }
}

#[cfg(test)]
#[path = "tests/macos.rs"]
mod tests;
