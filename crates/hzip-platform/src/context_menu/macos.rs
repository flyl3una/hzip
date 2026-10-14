//! macOS 右键菜单（T-CM §4）：Finder Sync 扩展轨 + 服务菜单轨。
//!
//! # 参照实现：Bandizip for macOS
//!
//! Bandizip 的实际做法（官方帮助页 + 真机 `pluginkit`/`codesign` 实证）是双轨：
//!
//! | 轨 | 菜单位置 | 用户如何启用 |
//! |---|---|---|
//! | **Finder Sync 扩展**（`.appex`） | 右键**顶层**「hzip」子菜单；**仅限已授权目录** | 系统设置 → 隐私与安全性 → 扩展 → Finder 扩展；Sequoia 起该系统设置已移除 → 用 `pluginkit -e use` |
//! | **Services**（服务） | 右键底部「服务」子菜单；**全局** | Info.plist 声明 `NSServices` 后系统自动登记 |
//!
//! # 两条轨各自的硬约束（真机实证，改代码前必读）
//!
//! - `.appex` 是 **Xcode target，必须内嵌在 `.app` 包内**（`Contents/PlugIns/`）——
//!   库无法在运行时「安装」它。故本模块只能注册/启用**发行包里已有的** appex。
//! - `.appex` **必须开启 sandbox**：`pkd` 对未沙盒化的扩展直接拒收
//!   （日志 `plug-ins must be sandboxed`，该扩展从数据库消失）。
//! - **ad-hoc 签名（`codesign --sign -`）即可注册并加载**，Developer ID 非必需
//!   （发行签名是另一回事，见 `build/package/package.py`）。
//! - 沙盒内的 appex **读不到**宿主的 `~/Library/Preferences/*.plist`，也**无法**
//!   用 `NSWorkspace.OpenConfiguration.arguments` 向宿主注入命令行（args 被丢弃）。
//!   故两侧的**唯一**通道是 App Group 容器：
//!   `~/Library/Group Containers/<APP_GROUP>/{menu.json, intents.jsonl}`。
//!   - `menu.json`：宿主写（条目集 + 标签 + 能力集 + 监听目录），appex 读；
//!   - `intents.jsonl`：appex 追加 `{key, paths}`，宿主消费（见
//!     [`ContextMenu::take_extension_intents`]）。
//!
//! 本模块负责：
//!
//! 1. 写 App Group 容器的 `menu.json`；
//! 2. `lsregister` 注册 app 与内嵌 appex；`pluginkit -a` 入插件库；`pluginkit -e use` 启用；
//! 3. 消费/追加 `intents.jsonl`；
//! 4. 翻转 `NSServicesStatus`（服务轨可见性）。
//!
//! **若发行包未内嵌 appex**（或无宿主 app），只能落到 Services 轨，
//! [`status`] 返回 `Degraded`/`Unsupported` 并**如实**说明（不再因为残留的
//! pbs 键就误报「Finder 扩展已启用」）。
//!
//! # 硬约束（产品面）
//!
//! Finder Sync **只对 extension 自己注册的监控目录**提供顶层菜单（沙盒限制），
//! 不给「任意文件夹」——故 Bandizip 才需要「可访问目录」设置项。本卡不做该
//! 设置面（见任务卡 §9 非目标），appex 侧默认注册用户主目录。
//!
//! # 调试
//!
//! 环境变量 `HZIP_DRY_RUN=1` 时只打印将要执行的命令、不实际调用外部工具
//! （无 macOS 的开发机可据此做语法/流程评审；正式路径不受影响）。

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{
    ContextMenu, ExtensionIntent, InstallMode, InstallReport, MenuError, MenuErrorKind, MenuSpec,
    MenuStatus,
};

/// 后端单例（[`super::backend`] 返回其 trait object）。
#[allow(non_upper_case_globals)]
pub(super) static BACKEND: Mac = Mac;

/// macOS 后端。
pub(super) struct Mac;

/// Finder Sync 扩展的 bundle id（`pluginkit` 用；**必须与 appex 的 Info.plist 一致**）。
pub(super) const EXTENSION_ID: &str = "io.github.hzip.hzip.findersync";

/// `lsregister` 的绝对路径（LaunchServices 私有工具；**不在 `PATH` 上**，
/// 必须绝对路径调用——`Command::new("lsregister")` 会得到
/// `No such file or directory`）。
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// 宿主 app bundle id（`lsregister` 与 App Group 命名用）。
///
/// **必须与 `build/package/metadata.toml::bundle_id` 一致**——历史上这里写死
/// `com.hzip.mac`，与发行包真实 id（`io.github.hzip.hzip`）不符，导致即便注册也
/// 永远匹配不上（实测缺陷）。
pub(super) const APP_ID: &str = "io.github.hzip.hzip";

/// App Group 标识（共享容器；app 与 appex 必须在同一组）。
///
/// appex（`hzipFinderSync.appex`）侧读取共享容器时使用；该 target 属 macOS 打包
/// 产物（`shell/hzip-findersync/`），不在本 workspace 编译——它必须与 appex 的
/// entitlements/Info.plist 保持一致，不可单独改。
pub(super) const APP_GROUP: &str = "group.io.github.hzip.hzip";

/// 容器内菜单配置文件名（宿主写，appex/Services 读）。
pub(super) const KEY_MENU_FILE: &str = "menu.json";

/// 容器内意图队列文件名（appex/Services 追加，宿主消费）。
pub(super) const INTENTS_FILE: &str = "intents.jsonl";

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

        // ① App Group 容器：条目集 + 标签 + 能力集 + 监听目录（appex 与 Services 都读它）
        write_menu_file(spec)?;

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
        let appex = appex_path(&bundle);
        run_tool(LSREGISTER, &["-f", &bundle.to_string_lossy()])
            .map_err(|e| MenuError::new(MenuErrorKind::Platform, e))?;
        // 顺序不可颠倒：`-e use` 只对**已入库**的扩展生效；新装的 appex 必须先
        // `pluginkit -a` 入插件库（Sequoia 起系统设置已无「Finder 扩展」开关，
        // pluginkit 是唯一途径）。
        run_tool("pluginkit", &["-a", &appex.to_string_lossy()])
            .map_err(|e| MenuError::new(MenuErrorKind::Platform, e))?;
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
        // 从插件库摘除（幂等：没登记过也返回 0）。
        if let Some(appex) = appex_path_from_current_exe().map(|b| appex_path(&b))
            && let Err(e) = run_tool("pluginkit", &["-r", &appex.to_string_lossy()])
        {
            errors.push(e);
        }
        if let Err(e) = disable_services() {
            errors.push(e);
        }
        if let Err(e) = clear_menu_file() {
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
            // 有 appex 但未启用：诚实报 Degraded（顶层菜单此刻不可见）。
            (PluginState::Disabled | PluginState::Unknown, true) => MenuStatus::Degraded {
                reason: "Finder 扩展未启用（可在系统设置 → 隐私与安全性 → 扩展 → \
                         添加的扩展 / Finder 扩展中启用，或运行 \
                         pluginkit -e use -i io.github.hzip.hzip.findersync）"
                    .to_string(),
            },
            // 无 appex：只有 Services 轨。Services 可见性以 NSServicesStatus 为准。
            (_, false) if services_enabled() => MenuStatus::Degraded {
                reason: "本版本未内嵌 Finder 扩展，仅右键底部「服务」子菜单可用".to_string(),
            },
            (_, false) => MenuStatus::Unsupported {
                reason: "本版本未内嵌 Finder 扩展，且服务菜单未启用".to_string(),
            },
        }
    }

    fn take_extension_intents(&self) -> Vec<ExtensionIntent> {
        take_intents_at(&intents_path())
    }

    fn append_extension_intent(&self, intent: &ExtensionIntent) -> Result<(), MenuError> {
        append_intent_at(&intents_path(), intent).map_err(|e| {
            MenuError::new(MenuErrorKind::Io, format!("写入扩展意图队列失败: {e}"))
        })
    }

    fn extension_intents_path(&self) -> Option<PathBuf> {
        Some(intents_path())
    }

    fn refresh_capabilities(&self, archive_exts: &[String]) -> Result<bool, MenuError> {
        let path = menu_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(false); // 未安装（无 menu.json）→ no-op
        };
        let mut value: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                return Err(MenuError::new(
                    MenuErrorKind::Io,
                    format!("解析 {} 失败: {e}", path.display()),
                ));
            }
        };
        let exts: Vec<serde_json::Value> = archive_exts
            .iter()
            .map(|e| serde_json::Value::String(e.clone()))
            .collect();
        value["archive_exts"] = serde_json::Value::Array(exts);
        let json = serde_json::to_string_pretty(&value)
            .map_err(|e| MenuError::new(MenuErrorKind::Io, format!("序列化菜单配置失败: {e}")))?;
        std::fs::write(&path, json).map_err(|e| {
            MenuError::new(MenuErrorKind::Io, format!("回写 {} 失败: {e}", path.display()))
        })?;
        Ok(true)
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

/// 当前可执行文件对应的宿主 bundle（`status`/`uninstall` 用）。
fn appex_path_from_current_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundle = app_bundle_path(&exe)?;
    appex_path(&bundle).is_dir().then_some(bundle)
}

/// App Group 容器目录：`~/Library/Group Containers/<APP_GROUP>`。
///
/// 宿主是**非沙盒**进程，直接按路径访问即可（appex 沙盒侧才需经
/// `containerURL(forSecurityApplicationGroupIdentifier:)`——两者指向同一目录）。
pub(super) fn group_container_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    home.join("Library")
        .join("Group Containers")
        .join(APP_GROUP)
}

/// 容器内菜单配置文件。
fn menu_path() -> PathBuf {
    group_container_dir().join(KEY_MENU_FILE)
}

/// 容器内意图队列文件。
fn intents_path() -> PathBuf {
    group_container_dir().join(INTENTS_FILE)
}

// ---------------------------------------------------------------- 菜单配置

/// 写入 App Group 容器的 `menu.json`（appex 读它决定展示什么）。
///
/// 这是宿主 → appex 的**唯一**通道：appex 在沙盒内读不到宿主的普通偏好
/// （`~/Library/Preferences/*.plist` 实测不可读），只能读 App Group 容器。
fn write_menu_file(spec: &MenuSpec) -> Result<(), MenuError> {
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
    let value = serde_json::json!({
        "items": items,
        "archive_exts": spec.archive_exts,
        "folders": [default_monitored_folders()],
    });
    let json = serde_json::to_string_pretty(&value)
        .map_err(|e| MenuError::new(MenuErrorKind::Spec, format!("序列化菜单配置失败: {e}")))?;

    let dir = group_container_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("创建 App Group 容器失败（{}）: {e}", dir.display()),
        )
    })?;
    let path = menu_path();
    std::fs::write(&path, json)
        .map_err(|e| MenuError::new(MenuErrorKind::Io, format!("写 {} 失败: {e}", path.display())))
}

/// 删除菜单配置与意图队列（幂等）。
fn clear_menu_file() -> Result<(), String> {
    let mut errs = Vec::new();
    for path in [menu_path(), intents_path()] {
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            errs.push(format!("删除 {} 失败: {e}", path.display()));
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
    let Ok(text) = std::fs::read_to_string(menu_path()) else {
        return 0;
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("items").and_then(|i| i.as_array()).map(Vec::len))
        .unwrap_or(0)
}

/// 默认监听目录（appex 只对这些目录提供顶层菜单）。
///
/// 无「可访问目录」设置面时取用户主目录——覆盖面最大且无需额外授权配置。
pub(super) fn default_monitored_folders() -> String {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .display()
        .to_string()
}

// ---------------------------------------------------------------- 意图队列

/// 消费意图队列：读取 → 解析 → **清空内容但保留文件**。
///
/// 保留文件是刻意的：appex 以追加方式写它，宿主若 `remove_file`，appex 侧
/// 已打开的文件句柄与后续 `seekToEndOfFile` 会落到已删除 inode（写丢）。故只
/// 截断内容（与 `hzip-forward-queue` 的 `drain_queue` 同款语义）。
fn take_intents_at(queue: &Path) -> Vec<ExtensionIntent> {
    let Ok(content) = std::fs::read_to_string(queue) else {
        return Vec::new();
    };
    if content.is_empty() {
        return Vec::new();
    }
    // 清空内容（保留文件本身）。
    if let Err(e) = std::fs::write(queue, b"") {
        tracing::warn!("清空扩展意图队列失败（可能重复消费）: {e}");
    }
    parse_intents(&content)
}

/// 队列文本 → 意图列表（逐行 JSON；**容忍脏行**，逐行丢弃并告警）。
pub(super) fn parse_intents(content: &str) -> Vec<ExtensionIntent> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| match serde_json::from_str::<ExtensionIntent>(line) {
            Ok(intent) => Some(intent),
            Err(e) => {
                tracing::warn!("丢弃无法解析的扩展意图行: {e}（{line}）");
                None
            }
        })
        .collect()
}

/// 追加一条意图（Services provider 宿主内调用；appex 侧由 Swift 直接写文件）。
fn append_intent_at(queue: &Path, intent: &ExtensionIntent) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = queue.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(intent).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(queue)?;
    // 单次 write_all：追加模式下 < 4 KiB 的写入是原子的（不产生半行）。
    file.write_all(line.as_bytes())
}

// ---------------------------------------------------------------- Services 轨

/// 启用本 app 声明的全部 Services（`NSServicesStatus` 逐个置 `enabled`）。
///
/// Info.plist 里的 `NSServices` 声明属打包期产物（见
/// `build/package/templates/macos/Info.plist.in`）；运行时通过
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

/// 当前是否至少有一个**本 app 的**服务处于启用态。
///
/// `defaults read` 输出形如：
///
/// ```text
/// {
///     "io.github.hzip.hzip - Extract Here with hzip" =     {
///         enabled = 1;
///     };
///     "com.other.app - Foo" =     {
///         enabled = 1;
///     };
/// }
/// ```
///
/// 逐条目解析（按 `};` 切块），只认**块内含本 app bundle id** 且 `enabled = 1`
/// 的项。早期的整串 `contains` 判断有两个误报面：① 别的 app 的服务 enabled=1
/// 也算数；② 本 app 全部置 0 后，只要文本里别处还有 `enabled = 1` 就被判为
/// 「已启用」（实测：清除后状态行误报「受限」而非「暂不支持」）。
fn services_enabled() -> bool {
    let Ok(out) = run_tool("defaults", &["read", "pbs", "NSServicesStatus"]) else {
        return false;
    };
    service_entry_enabled(&out, APP_ID)
}

/// 解析 `defaults read pbs NSServicesStatus` 文本：是否存在**属于 `app_id`** 的
/// `enabled = 1` 条目（纯函数便于单测）。
pub(super) fn service_entry_enabled(output: &str, app_id: &str) -> bool {
    // 每个条目从 `"<name>" = {` 到 `};`。按 `};` 切块即可覆盖当前 `defaults`
    // 的输出；块内同时含 app_id（条目名 = "<app_id> - <service>"）与 enabled = 1。
    output.split("};").any(|block| {
        block.contains(app_id) && (block.contains("enabled = 1") || block.contains("enabled=1"))
    })
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
