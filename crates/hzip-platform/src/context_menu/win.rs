//! Windows 右键菜单后端（T-CM §3）：经典注册表 verbs + Windows 11 稀疏包。
//!
//! # 两条路径，同一个 `IExplorerCommand` DLL
//!
//! | OS | build | 机制 | 菜单形态 |
//! |---|---|---|---|
//! | Windows 10 / Server 2016-2022 | < 22000 | 经典注册（`ExplorerCommandHandler`） | 唯一菜单 |
//! | Windows 11 / Server 2025 | ≥ 22000 | 稀疏包（`windows.fileExplorerContextMenus`） | **现代折叠菜单顶层** |
//!
//! Windows 11 上若稀疏包注册失败（无签名证书 / 策略拦截 / 无 PowerShell），
//! 回落经典注册——**永不劣于**「仅显示更多选项」，并由 [`status`] 如实报 `Degraded`。
//!
//! # 为什么经典路径也用 COM
//!
//! `ExplorerCommandHandler = {CLSID}` 让 shell 调 `IExplorerCommand`，于是
//! `Invoke(IShellItemArray)` **一次拿到全部选中项**（无静态 verb 的
//! `MultiSelectModel` 15 项上限），且菜单内容由 DLL 现读配置决定——增删勾选项
//! 无需改注册表、无需重注册包、无需重启 Explorer。
//!
//! # 基础 ProgID 的默认动词保护（必须）
//!
//! 在 `*` / `Directory` 上挂 verb 时必须把其 `Shell` 子键默认值置 `"none"`，
//! 否则 shell 会把我们的 verb 当成这些基础类的默认动作（**双击行为被改**）。
//! 原值先备份到 [`BACKUP_KEY`]，卸载时精确还原（原本无值 → 删值，不留 `"none"`）。
//!
//! # 落盘
//!
//! 全部写 **HKCU**（免管理员）；写/删完成后调 `SHChangeNotify(SHCNE_ASSOCCHANGED)`。

use std::path::{Path, PathBuf};
use std::process::Command;

use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE};

use super::{
    ContextMenu, InstallMode, InstallReport, MenuError, MenuErrorKind, MenuScope, MenuSpec,
    MenuStatus, menu_config_path,
};

/// 后端单例（[`super::backend`] 返回其 trait object）。
pub(super) static BACKEND: Win = Win;

/// Windows 后端。
pub(super) struct Win;

/// 组件 CLSID（**全链路唯一**；经典注册与稀疏包清单共用）。
///
/// 必须与 `shell/hzip-shell/src/lib.rs::CLSID`（DLL 侧 `DllGetClassObject`
/// 只认这一个）、包清单 `com:Class/@Id`、`build/shell-ext/build-shell-ext.ps1` 的
/// `$clsid` 完全一致。**不一致的后果是右键菜单完全不出现**（资源管理器
/// 加载 DLL 后得到 `CLASS_E_CLASSNOTAVAILABLE`），且没有任何报错提示。
/// `tests/context_menu.rs` 有一致性测试覆盖这四处。
pub const CLSID: &str = "{A7C4D1E8-5B93-4F60-9E27-8D3A1B5C7F04}";

/// 稀疏包 Identity Name（`Add-AppxPackage` 用）。
const PACKAGE_NAME: &str = "hzip.ShellMenu";

/// Shell 扩展 DLL 文件名（与 `hzip-gui.exe` 同目录）。
const DLL_NAME: &str = "hzip-shell.dll";

/// 稀疏包文件名（与 `hzip-gui.exe` 同目录）。
const MSIX_NAME: &str = "hzip-shellmenu.msix";

/// 右键菜单 verb 键名（装载点 = `Shell` 子键名；卸载按此清理）。
const VERB: &str = "hzip";

/// 菜单顶层显示名（`MUIVerb`）。
const MUI_VERB: &str = "hzip";

/// 默认动词备份键（原值精确还原用）。
const BACKUP_KEY: &str = "Software\\hzip\\ShellMenu\\backup";

/// `HKCU\Software\Classes`。
const CLASSES: &str = "Software\\Classes";

/// 需要挂载 verb 的基础 ProgID（与稀疏包清单的三个 `ItemType` 一一对应）。
const SCOPES: [&str; 3] = ["*", "Directory", "Directory\\Background"];

/// Win11 现代上下文菜单的最低 build（`windows.fileExplorerContextMenus` 生效线）。
///
/// 该值（22000）已高于 `AllowExternalContent` 的 19041 门槛，故现代路径无需
/// 再单独判 OS 下限。
const BUILD_WIN11: u32 = 22000;

/// 「新式右键菜单被禁用」的开关 CLSID。
///
/// Windows 11 上最常见的注册表改法：在 HKCU 建
/// `Software\Classes\CLSID\{86CA1AA0-…}\InprocServer32` 且**默认值为空字符串**。
/// 该键会**遮蔽** HKLM 下的同名 CLSID（"File Explorer Context Menu"），使资源管理器
/// 永远回落到 Windows 10 经典菜单——**新式菜单根本不渲染**。
///
/// 影响：打包装的现代菜单项在这类机器上**永远不可能显示**（没有承载它的菜单）。
/// 故此时必须同时注册经典路径，否则用户在两边都看不到（这正是本机实测踩到的坑：
/// 包、清单、COM 激活、DLL 行为全部正确，却「右键里什么都没有」）。
const CLASSIC_MENU_SWITCH_CLSID: &str =
    "Software\\Classes\\CLSID\\{86CA1AA0-34AA-4E8B-A509-50C905BAE2A2}\\InprocServer32";

/// 新式右键菜单是否被本机开关禁用（只读探测）。
fn modern_menu_disabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(CLASSIC_MENU_SWITCH_CLSID, KEY_READ)
        .is_ok()
}

/// AppX 包仓库（枚举判断是否已注册；免 PowerShell，快）。
const APPX_REPO: &str = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages";

impl ContextMenu for Win {
    fn install(&self, spec: &MenuSpec) -> Result<InstallReport, MenuError> {
        install(spec)
    }

    fn uninstall(&self) -> Result<(), MenuError> {
        uninstall()
    }

    fn status(&self) -> MenuStatus {
        status()
    }

    fn sync(&self, spec: &MenuSpec) -> Result<(), MenuError> {
        sync(spec)
    }

    fn refresh_capabilities(&self, archive_exts: &[String]) -> Result<bool, MenuError> {
        refresh_archive_exts(archive_exts)
    }
}

// ---------------------------------------------------------------- install

fn install(spec: &MenuSpec) -> Result<InstallReport, MenuError> {
    install_at(spec, CLASSES, BACKUP_KEY, &menu_config_path())
}

/// 规格校验（`install` / `sync` 共用，**唯一一处**）：桌面体验可用、勾选非空、
/// 可执行文件与随行的 shell 扩展 DLL 都在位。
///
/// 返回 `(exe 字符串, 安装目录, DLL 路径)`——后续注册步骤的公共输入。
/// 校验与写入分开是刻意的：`sync` 只写配置，若它自带一份更宽松的校验，
/// 就会出现「sync 成功但菜单永远不出现」的静默失败。
///
/// `os` 由调用方传入而非在此再探一次：[`install_at`] 要用**同一份**能力判定
/// 决定走经典还是稀疏包，若此处自行 [`os_info`]，`HKLM\…\CurrentVersion`
/// 会被读两遍（Server 上还要多探一次 `Server\ServerLevels`）。
fn validate_spec(os: &OsInfo, spec: &MenuSpec) -> Result<(String, PathBuf, PathBuf), MenuError> {
    if !os.desktop_shell {
        return Err(MenuError::new(
            MenuErrorKind::Unsupported,
            "当前系统未安装桌面体验（Server Core 无资源管理器右键菜单）",
        ));
    }
    if spec.is_empty() {
        return Err(MenuError::new(MenuErrorKind::Spec, "未勾选任何右键菜单项"));
    }
    let exe = spec.exe_path.to_string_lossy().to_string();
    if !spec.exe_path.is_file() {
        return Err(MenuError::new(
            MenuErrorKind::Spec,
            format!("可执行文件不存在：{exe}"),
        ));
    }
    let dir = spec
        .exe_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    // DLL 路径：配置覆盖（`shell_dll`）优先，未指定 = 主程序同目录默认
    // （走查 #14：DLL 与主程序分目录部署时 InProcServer32 指向配置路径）
    let dll = spec.shell_dll.clone().unwrap_or_else(|| dir.join(DLL_NAME));
    if !dll.is_file() {
        return Err(MenuError::new(
            MenuErrorKind::Spec,
            format!("Shell 扩展未随程序安装：{}", dll.display()),
        ));
    }
    Ok((exe, dir, dll))
}

/// 可注入路径的安装实现（生产入口 [`install`] 传真实常量；测试传沙箱路径，
/// 避免污染真实 HKCU 的 `*`/`Directory` 默认动词与用户配置）。
fn install_at(
    spec: &MenuSpec,
    classes_root: &str,
    backup_root: &str,
    config_path: &Path,
) -> Result<InstallReport, MenuError> {
    let os = os_info();
    let (exe, dir, dll) = validate_spec(&os, spec)?;

    // ① 配置落盘（DLL 现读；增删条目无需改注册表 / 重注册包）
    write_menu_config_at(config_path, spec)?;

    // ② 注册路径 **二选一**（两条都写 = 右键里出现两个 hzip 项，实测）。
    //
    // 关键实测（Win11 + 本机经典菜单开关开）：稀疏包 manifest 的
    // `desktop5:Verb` **也会**渲染进经典菜单，与经典注册的
    // `ExplorerCommandHandler` 并列 → 两个「hzip」目录。故：
    //   · 经典菜单开关开 → 只写经典注册，并注销可能残留的旧稀疏包；
    //   · 否则 → 优先稀疏包（现代菜单顶层）；包注册失败才回落经典。
    let msix = dir.join(MSIX_NAME);
    let modern_capable = os.build >= BUILD_WIN11;

    let mode = if modern_menu_disabled() {
        tracing::info!(
            "检测到经典菜单开关（HKCU CLSID {{86CA1AA0-…}}）：只写经典注册（避免与稀疏包重复）"
        );
        if package_installed() {
            match unregister_package() {
                Ok(()) => tracing::info!("旧稀疏包已注销（本机走经典菜单，避免重复项）"),
                Err(e) => tracing::warn!(
                    "旧稀疏包注销失败（可能残留重复项——重启资源管理器后如仍有 hzip 重复，请再点一次「应用到系统」）：{e}"
                ),
            }
        }
        write_classic_checked(classes_root, backup_root, &exe, &dll)?;
        InstallMode::LegacyMenu
    } else if modern_capable && msix.is_file() {
        match register_package(&msix, &dir) {
            Ok(()) => {
                // 现代菜单已接管：经典残留必须清掉，否则「新菜单 + 显示更多选项」重复
                let mut leftover = Vec::new();
                clear_classic(classes_root, backup_root, &mut leftover);
                for e in leftover {
                    tracing::warn!("经典注册残留清理失败（不影响现代菜单）：{e}");
                }
                InstallMode::ModernTopLevel
            }
            Err(e) => {
                tracing::warn!("稀疏包注册失败，回落经典注册：{e}");
                write_classic_checked(classes_root, backup_root, &exe, &dll)?;
                InstallMode::LegacyMenu
            }
        }
    } else {
        if modern_capable {
            tracing::warn!(
                "缺少 {}，回落经典注册（Win11 将仅出现在「显示更多选项」）",
                msix.display()
            );
        }
        write_classic_checked(classes_root, backup_root, &exe, &dll)?;
        InstallMode::LegacyMenu
    };

    notify_shell();
    tracing::info!(items = spec.len(), mode = ?mode, build = os.build, "右键菜单已安装");
    Ok(InstallReport {
        mode,
        items: spec.len(),
        restart_required: true,
    })
}

/// 先清经典残留（幂等）再写：图标 / exe 路径 / 条目集每次「应用」都以当前状态为准。
///
/// 「先清后写」是 `write_classic` 的前置约定（它只负责写）；不先清时，上一版本
/// 留下的键会被原样保留（例如换过 exe 路径后 `Icon` 仍指向旧位置）。
fn write_classic_checked(
    classes_root: &str,
    backup_root: &str,
    exe: &str,
    dll: &Path,
) -> Result<(), MenuError> {
    let mut errors = Vec::new();
    clear_classic(classes_root, backup_root, &mut errors);
    write_classic(classes_root, backup_root, exe, dll, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(MenuError {
            kind: MenuErrorKind::Io,
            details: errors,
        })
    }
}

/// 只同步**条目集**（勾选项）：一次小文件写入，不碰注册表、不起 PowerShell。
///
/// 未安装 → 退回完整 [`install`]：只写配置等于装了个系统上看不见的菜单，
/// 比「不同步」更糟（用户以为点过了、菜单里却什么都没有）。
fn sync(spec: &MenuSpec) -> Result<(), MenuError> {
    if !matches!(status(), MenuStatus::Installed { .. }) {
        return install(spec).map(|_| ());
    }
    sync_at(spec, &menu_config_path())
}

/// 可注入配置路径的同步实现（与 [`install_at`] 同一套测试接缝）。
///
/// **不发** `SHChangeNotify`：DLL 在每次构建菜单时重新 `CreateInstance` 并重读
/// 配置文件（实测 trace 逐次确认），故改条目集无需惊动 shell 重建图标/关联缓存。
fn sync_at(spec: &MenuSpec, config_path: &Path) -> Result<(), MenuError> {
    validate_spec(&os_info(), spec)?;
    write_menu_config_at(config_path, spec)?;
    tracing::debug!(items = spec.len(), "右键菜单条目集已同步（仅重写配置）");
    Ok(())
}

// ---------------------------------------------------------------- uninstall

fn uninstall() -> Result<(), MenuError> {
    uninstall_at(CLASSES, BACKUP_KEY, &menu_config_path())
}

/// 可注入路径的卸载实现（与 [`install_at`] 同一套接缝）。
fn uninstall_at(
    classes_root: &str,
    backup_root: &str,
    config_path: &Path,
) -> Result<(), MenuError> {
    let mut errors = Vec::new();

    // ① 稀疏包注销（未注册不算错）
    if let Err(e) = unregister_package() {
        errors.push(e);
    }

    // ② 经典注册清理（含默认动词还原）
    clear_classic(classes_root, backup_root, &mut errors);

    // ③ 配置删除
    let cfg = config_path.to_path_buf();
    if let Err(e) = std::fs::remove_file(&cfg)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        errors.push(format!("删除 {} 失败: {e}", cfg.display()));
    }

    notify_shell();

    if errors.is_empty() {
        tracing::info!("右键菜单已清除");
        Ok(())
    } else {
        Err(MenuError {
            kind: MenuErrorKind::Io,
            details: errors,
        })
    }
}

// ---------------------------------------------------------------- status

fn status() -> MenuStatus {
    let os = os_info();
    decide_status(
        os.desktop_shell,
        os.build,
        classic_installed_at(CLASSES),
        package_installed(),
        modern_menu_disabled(),
        read_menu_config_items_at(&menu_config_path()),
    )
}

/// 状态判定（**纯函数**：无 I/O，故整张决策表可单测）。
///
/// # 为什么要有它
///
/// 用户看到的「菜单形态」由**两个独立开关**共同决定，只看其中任一个都会报错：
///
/// | 已注册的路径 | 本机新式菜单被禁用？ | 用户实际看到的 | 应报 |
/// |---|---|---|---|
/// | 稀疏包 | 否 | 新式菜单顶层的 hzip | `Installed{ModernTopLevel}` |
/// | 稀疏包 | **是** | **什么都看不到**（新式菜单不渲染） | `Degraded`（附原因） |
/// | 经典 | 是 | 经典菜单里的 hzip | `Installed{LegacyMenu}` |
/// | 经典 | 否（Win11） | 仅「显示更多选项」里的 hzip | `Degraded` |
/// | 经典 | 否（Win10） | 唯一菜单里的 hzip | `Installed{LegacyMenu}` |
///
/// 早期实现先判「包是否注册」就返回 `ModernTopLevel`，在禁用新式菜单的机器上
/// 会给出与实际相反的结论（用户明明看不到任何东西）。
fn decide_status(
    desktop_shell: bool,
    build: u32,
    classic: bool,
    packaged: bool,
    classic_menu_switch: bool,
    items: usize,
) -> MenuStatus {
    if !desktop_shell {
        return MenuStatus::Unsupported {
            reason: "当前系统未安装桌面体验（Server Core）".to_string(),
        };
    }
    // 新式菜单被本机开关禁用时，经典菜单就是用户实际看到的那个——优先如实报它
    if classic && classic_menu_switch {
        return MenuStatus::Installed {
            mode: InstallMode::LegacyMenu,
            items,
        };
    }
    if packaged && !classic_menu_switch {
        return MenuStatus::Installed {
            mode: InstallMode::ModernTopLevel,
            items,
        };
    }
    if classic {
        if build >= BUILD_WIN11 {
            // 新式菜单可用，但包没生效 → 只进「显示更多选项」
            return MenuStatus::Degraded {
                reason: "已注册，但 Windows 11 仅在右键「显示更多选项」中可见\
                         （现代菜单需稀疏包 hzip-shellmenu.msix 成功注册；\
                         常见原因：签名证书未被系统信任，或缺少该 .msix）"
                    .to_string(),
            };
        }
        return MenuStatus::Installed {
            mode: InstallMode::LegacyMenu,
            items,
        };
    }
    if packaged && classic_menu_switch {
        // 包注册了、但本机新式菜单被禁用且经典未写 → 用户看不到任何东西
        return MenuStatus::Degraded {
            reason: "本机已禁用 Windows 11 新式右键菜单（HKCU 的经典菜单开关），\
                     打包装的菜单项无处显示；请在设置页重新「应用」以写入经典菜单"
                .to_string(),
        };
    }
    MenuStatus::NotInstalled
}

// ---------------------------------------------------------------- 经典注册

/// 写经典注册（幂等：由调用方先 `clear_classic`，本函数只负责写）。
fn write_classic(
    classes_root: &str,
    backup_root: &str,
    exe: &str,
    dll: &Path,
    errors: &mut Vec<String>,
) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = match hkcu.create_subkey(classes_root) {
        Ok((k, _)) => k,
        Err(e) => {
            errors.push(format!("打开 HKCU\\{classes_root} 失败: {e}"));
            return;
        }
    };

    // ① COM 类注册（InProcServer32 → DLL）
    let clsid_path = format!("CLSID\\{CLSID}");
    match classes.create_subkey(&clsid_path) {
        Ok((clsid, _)) => {
            if let Err(e) = clsid.set_value("", &"hzip 上下文菜单") {
                errors.push(format!("写 CLSID 描述失败: {e}"));
            }
            match clsid.create_subkey("InProcServer32") {
                Ok((k, _)) => {
                    let dll_s = dll.to_string_lossy().to_string();
                    if let Err(e) = k.set_value("", &dll_s) {
                        errors.push(format!("写 InProcServer32 失败: {e}"));
                    }
                    if let Err(e) = k.set_value("ThreadingModel", &"Apartment") {
                        errors.push(format!("写 ThreadingModel 失败: {e}"));
                    }
                }
                Err(e) => errors.push(format!("创建 InProcServer32 失败: {e}")),
            }
        }
        Err(e) => errors.push(format!("创建 {clsid_path} 失败: {e}")),
    }

    // ② 三个作用域挂 verb（条目内容由 DLL 现读配置决定）
    for prog_id in SCOPES {
        let shell = format!("{prog_id}\\shell\\{VERB}");
        match classes.create_subkey(&shell) {
            Ok((k, _)) => {
                if let Err(e) = k.set_value("MUIVerb", &MUI_VERB) {
                    errors.push(format!("写 {shell}\\MUIVerb 失败: {e}"));
                }
                if let Err(e) = k.set_value("Icon", &format!("{exe},0")) {
                    errors.push(format!("写 {shell}\\Icon 失败: {e}"));
                }
                if let Err(e) = k.set_value("ExplorerCommandHandler", &CLSID) {
                    errors.push(format!("写 {shell}\\ExplorerCommandHandler 失败: {e}"));
                }
            }
            Err(e) => errors.push(format!("创建 {shell} 失败: {e}")),
        }
        // 默认动词保护：基础 ProgID 的 Shell 默认值置 "none"（原值先备份）
        if let Err(e) = protect_default_verb(&classes, backup_root, prog_id) {
            errors.push(e);
        }
    }
}

/// 基础 ProgID 默认动词保护：备份原值 → 置 `"none"`。
fn protect_default_verb(classes: &RegKey, backup_root: &str, prog_id: &str) -> Result<(), String> {
    let shell_path = format!("{prog_id}\\Shell");
    let (shell, _) = classes
        .create_subkey(&shell_path)
        .map_err(|e| format!("创建 {shell_path} 失败: {e}"))?;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (backup, _) = hkcu
        .create_subkey(backup_root)
        .map_err(|e| format!("创建备份键失败: {e}"))?;

    let name = prog_id.replace('\\', "_");
    match shell.get_value::<String, _>("") {
        Ok(prev) => backup
            .set_value(&name, &prev)
            .map_err(|e| format!("备份 {prog_id} 默认动词失败: {e}"))?,
        Err(_) => {
            // 原本无默认值 → 记缺失标记（卸载时删值，而非写回空串）
            backup
                .set_value(format!("{name}__absent"), &1u32)
                .map_err(|e| format!("记录 {prog_id} 默认动词缺失失败: {e}"))?;
        }
    }
    shell
        .set_value("", &"none")
        .map_err(|e| format!("写 {shell_path} 默认值失败: {e}"))
}

/// 清经典注册（verb + CLSID + 默认动词还原）；逐项收集错误，不首个失败即中断。
fn clear_classic(classes_root: &str, backup_root: &str, errors: &mut Vec<String>) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(classes) = hkcu.open_subkey_with_flags(classes_root, KEY_READ | KEY_WRITE) else {
        return; // 无该键 → 无残留
    };

    for prog_id in SCOPES {
        let shell = format!("{prog_id}\\shell\\{VERB}");
        if let Err(e) = classes.delete_subkey_all(&shell)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(format!("删除 {shell} 失败: {e}"));
        }
        if let Err(e) = restore_default_verb(&classes, backup_root, prog_id) {
            errors.push(e);
        }
    }

    let clsid_path = format!("CLSID\\{CLSID}");
    if let Err(e) = classes.delete_subkey_all(&clsid_path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        errors.push(format!("删除 {clsid_path} 失败: {e}"));
    }

    // 备份键删除后，其父键（`Software\hzip\ShellMenu`、`Software\hzip`）会作为
    // **空壳**留下。这些键是 `create_subkey` 为写备份而隐式创建的，不属于用户数据，
    // 「清除」应一并收干净。
    prune_empty_parents(backup_root, errors);
}

/// 还原基础 ProgID 的 `Shell` 默认值（精确还原：原本无值 → 删值）。
///
/// `protect_default_verb` 的逆操作；两者必须成对，否则「清除」会把用户的
/// 默认动词永久改成 `"none"`（双击行为被破坏）。
fn restore_default_verb(classes: &RegKey, backup_root: &str, prog_id: &str) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(backup) = hkcu.open_subkey_with_flags(backup_root, KEY_READ | KEY_WRITE) else {
        return Ok(()); // 无备份 → 我们没改过
    };
    let name = prog_id.replace('\\', "_");
    let shell_path = format!("{prog_id}\\Shell");
    let Ok(shell) = classes.open_subkey_with_flags(&shell_path, KEY_WRITE) else {
        return Ok(());
    };

    let absent = backup
        .get_value::<u32, _>(format!("{name}__absent"))
        .unwrap_or(0)
        == 1;
    if absent {
        if let Err(e) = shell.delete_value("")
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(format!("清除 {shell_path} 默认值失败: {e}"));
        }
    } else if let Ok(prev) = backup.get_value::<String, _>(&name)
        && let Err(e) = shell.set_value("", &prev)
    {
        return Err(format!("还原 {shell_path} 默认值失败: {e}"));
    }

    let _ = backup.delete_value(&name);
    let _ = backup.delete_value(format!("{name}__absent"));
    // 备份值清空后删除备份键本身（否则留下空壳）；其父键由
    // `prune_empty_parents` 进一步修剪（`RegDeleteKeyExW` 在本机对空键亦会失败，
    // 故这里仍用已经实证可用的 `delete_subkey_all`）。
    if backup.enum_values().filter_map(|v| v.ok()).count() == 0 {
        let _ = hkcu.delete_subkey_all(backup_root);
    }
    Ok(())
}

/// 自底向上删除 `backup_root` 的空父键（**仅当既无子键也无值**）。
///
/// 停在 `Software` 层：不动注册表根，也不动任何还留有内容的键（用户若在
/// `Software\hzip` 下另有状态，此处自动停止）。
///
/// # 为什么不用 `delete_subkey`
///
/// winreg 的 `delete_subkey` 走 `RegDeleteKeyExW(..., perms = 0, ...)`，本机实测
/// **对空键也返回失败**（`perms = 0` 不被接受）。故改为：用 `RegQueryInfoKeyW`
/// 拿到权威的子键数/值数（不依赖可能出错的枚举迭代器），确认双零后用已实证可用的
/// `delete_subkey_all`（`RegDeleteTreeW`）删除。
fn prune_empty_parents(backup_root: &str, errors: &mut Vec<String>) {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegDeleteTreeW, RegOpenKeyExW, RegQueryInfoKeyW,
    };

    let mut parts: Vec<&str> = backup_root.split('\\').collect();
    parts.pop(); // `backup_root` 自身由调用方删除
    // 保留最顶层一段（`Software`），不向上越界
    while parts.len() > 1 {
        let path = parts.join("\\");
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut hkey = std::ptr::null_mut();
        // SAFETY: wide 以 NUL 结尾且在本调用期间存活；hkey 为出参
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide.as_ptr(), 0, KEY_READ, &mut hkey) };
        if rc != 0 {
            // ERROR_FILE_NOT_FOUND(2) 是**正常**情况：没有备份键，本就没有父键要修剪。
            // 只有其它错误（权限、损坏）才值得上报。
            if rc != 2 {
                errors.push(format!("prune: 打开 {path} 失败 rc={rc}"));
            }
            break;
        }
        let mut subkeys = 0u32;
        let mut values = 0u32;
        // SAFETY: 句柄有效；除子键数/值数外全部传空
        let query = unsafe {
            RegQueryInfoKeyW(
                hkey,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
                &mut subkeys,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut values,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        // SAFETY: 句柄由上面的 RegOpenKeyExW 产出且仅关闭一次
        unsafe { RegCloseKey(hkey) };
        if query != 0 {
            errors.push(format!("prune: 查询 {path} 失败 rc={query}"));
            break;
        }
        if subkeys != 0 || values != 0 {
            // 正常停止：该键还有内容（**本就不该由我们删**）——例如生产里的
            // `Software\Classes`，或测试夹具键。停手即正确行为，不记错误。
            break;
        }
        // SAFETY: wide 以 NUL 结尾；HKCU 预定义句柄有最大访问权
        let rc = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide.as_ptr()) };
        if rc != 0 {
            errors.push(format!("prune: 删除 {path} 失败 rc={rc}"));
            break;
        }
        parts.pop();
    }
}

/// 经典注册是否在位（verb 键 + CLSID 同时在）。
fn classic_installed_at(classes_root: &str) -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(classes) = hkcu.open_subkey_with_flags(classes_root, KEY_READ) else {
        return false;
    };
    classes
        .open_subkey_with_flags(format!("*\\shell\\{VERB}"), KEY_READ)
        .is_ok()
        && classes
            .open_subkey_with_flags(format!("CLSID\\{CLSID}\\InProcServer32"), KEY_READ)
            .is_ok()
}

// ---------------------------------------------------------------- 稀疏包

/// 稀疏包是否已注册（Identity Name 前缀匹配 `hzip.ShellMenu_<ver>_<arch>__<hash>`）。
fn package_installed() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(repo) = hkcu.open_subkey_with_flags(APPX_REPO, KEY_READ) else {
        return false;
    };
    let prefix = format!("{PACKAGE_NAME}_");
    repo.enum_keys()
        .filter_map(|k| k.ok())
        .any(|k| k.starts_with(&prefix))
}

/// 注册稀疏包（`Add-AppxPackage -ExternalLocation`）。
fn register_package(msix: &Path, install_dir: &Path) -> Result<(), String> {
    // 已注册同版本包时直接 `Add-AppxPackage` 会失败（版本相同不允许覆盖）——
    // 设置页「应用」是可重复点击的，故这里必须幂等：已装则用 -ForceUpdateFromAnyVersion
    // 覆盖（换了新构建、版本号未变时也正确）。
    let already = package_installed();
    let cmd = format!(
        "Add-AppxPackage -Path {} -ExternalLocation {}{}",
        ps_quote(&msix.to_string_lossy()),
        ps_quote(&install_dir.to_string_lossy()),
        if already {
            " -ForceUpdateFromAnyVersion"
        } else {
            ""
        }
    );
    let diag = run_powershell(&cmd)?;
    if package_installed() {
        Ok(())
    } else {
        Err(format!("Add-AppxPackage 未生效（{diag}）"))
    }
}

/// 注销稀疏包（未注册视为成功）。
fn unregister_package() -> Result<(), String> {
    if !package_installed() {
        return Ok(());
    }
    let cmd = format!("Get-AppxPackage {PACKAGE_NAME} | Remove-AppxPackage");
    let diag = run_powershell(&cmd)?;
    if package_installed() {
        Err(format!("Remove-AppxPackage 未生效（{diag}）"))
    } else {
        Ok(())
    }
}

/// PowerShell 单引号字面量转义（内含单引号 → 双写）。
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 执行 PowerShell，返回**诊断文本**（stdout+stderr 合并）。
///
/// # 为什么不看退出码
///
/// 实测在本环境（`-NonInteractive -WindowStyle Hidden`）下，`Remove-AppxPackage`
/// **实际生效却仍以 255 退出**，且 stderr 为空；同理 `-ErrorAction Stop` 会让它
/// 静默失败。退出码在这里既不可靠又无信息量。
///
/// 因此本函数**只把「进程起不来」当失败**；调用方一律以**后置条件**
/// （包在不在仓库里、键在不在注册表里）为唯一判据，诊断文本仅用于失败时说明。
fn run_powershell(command: &str) -> Result<String, String> {
    // 隐藏窗口靠 **进程创建标志 `CREATE_NO_WINDOW`**，而不是 PowerShell 的
    // `-WindowStyle Hidden`：实测后者会让 `Remove-AppxPackage` 直接以 255 失败
    // 且包依然留在仓库里（Add 却正常，极易误判为「卸载逻辑有 bug」）。
    // `CREATE_NO_WINDOW` 只影响控制台窗口，不干扰 AppX 部署代理。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    use std::os::windows::process::CommandExt as _;

    let out = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            command,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("启动 PowerShell 失败: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let mut diag = stdout;
    if !stderr.is_empty() {
        if !diag.is_empty() {
            diag.push_str(" | ");
        }
        diag.push_str(&stderr);
    }
    if !out.status.success() {
        let code = format!("退出码 {:?}", out.status.code());
        diag = if diag.is_empty() {
            code
        } else {
            format!("{code}: {diag}")
        };
    }
    Ok(diag)
}

// ---------------------------------------------------------------- 配置落盘

/// 写 `shell-menu.json`（DLL 的**唯一**配置来源）。
///
/// 按 scope 分组：DLL 据目标类型（文件 / 文件夹）取对应列表，并用
/// `archive_exts` 在非档案文件上隐藏解压类条目（`ECS_HIDDEN`）。
fn write_menu_config_at(path: &Path, spec: &MenuSpec) -> Result<(), MenuError> {
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        return Err(MenuError::new(
            MenuErrorKind::Io,
            format!("创建 {} 失败: {e}", dir.display()),
        ));
    }

    let scoped = |want: MenuScope| -> Vec<serde_json::Value> {
        spec.resolved()
            .filter(|(_, s)| !s.win_skip && s.scopes.contains(&want))
            .map(|(entry, s)| {
                serde_json::json!({
                    "key": entry.key,
                    "label": entry.label,
                    "args": s.argv,
                })
            })
            .collect()
    };

    let doc = serde_json::json!({
        "schema": 1,
        "exe": spec.exe_path.to_string_lossy(),
        "archive_exts": spec.archive_exts,
        "items": {
            "archive_file": scoped(MenuScope::ArchiveFile),
            "any": scoped(MenuScope::Any),
        },
    });

    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| MenuError::new(MenuErrorKind::Io, format!("序列化菜单配置失败: {e}")))?;
    std::fs::write(path, text).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("写入 {} 失败: {e}", path.display()),
        )
    })
}

/// **只刷新** `shell-menu.json` 的 `archive_exts`（已安装时；未安装 = no-op）。
///
/// # 为什么单独有这个入口
///
/// `archive_exts` 是**引擎能力**（能读哪些扩展名），不是用户选项——它决定 DLL
/// 在某个文件上是否显示解压组（`is_archive_path`）。但它此前只随「应用到系统」
/// 写入，于是**升级后能力变了、已装的菜单却还按旧清单隐藏/显示**：
/// 实测症状 = 新增 rar/tgz 支持后，`.rar` 右键里依旧没有解压项，用户以为
/// 「勾了文件关联才支持解压」（勾选管的是 OS 默认程序，与解压能力无关）。
///
/// 故应用启动时调本函数做一次**保鲜**：读现有文件、只替换 `archive_exts`、
/// 原样回写（`items` 是用户勾选，归「应用到系统」按钮，绝不在此处改）。
///
/// 返回 `Ok(true)` = 文件存在且内容有变化（已回写）；`Ok(false)` = 未安装或
/// 已是最新（无需动盘）。
pub(super) fn refresh_archive_exts(archive_exts: &[String]) -> Result<bool, MenuError> {
    let path = super::menu_config_path();
    refresh_archive_exts_at(&path, archive_exts)
}

/// [`refresh_archive_exts`] 的**指定路径**变体（测试用；逻辑不带环境依赖）。
pub(super) fn refresh_archive_exts_at(
    path: &Path,
    archive_exts: &[String],
) -> Result<bool, MenuError> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(false); // 未安装（无文件）= no-op
    };
    let mut doc: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("解析 {} 失败: {e}", path.display()),
        )
    })?;
    let want = serde_json::json!(archive_exts);
    if doc.get("archive_exts") == Some(&want) {
        return Ok(false);
    }
    let Some(obj) = doc.as_object_mut() else {
        return Ok(false);
    };
    obj.insert("archive_exts".into(), want);
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| MenuError::new(MenuErrorKind::Io, format!("序列化菜单配置失败: {e}")))?;
    std::fs::write(path, text).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("写入 {} 失败: {e}", path.display()),
        )
    })?;
    Ok(true)
}

/// 回读配置中的条目数（状态行展示；解析失败 → 0）。
///
/// 取各组**之和**：组之间按 scope 互斥（无条目同属 `archive_file` 与 `any`），
/// 故总和即实际装进菜单的条目数。若取最大值，状态行会把 9 项误报成 5 项。
fn read_menu_config_items_at(path: &Path) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0;
    };
    doc.get("items")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.values()
                .filter_map(|v| v.as_array())
                .map(|a| a.len())
                .sum()
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------- OS 判别

/// 本机 OS 能力。
struct OsInfo {
    /// build 号（`CurrentBuildNumber`）。
    build: u32,
    /// 是否具备桌面 shell（Server Core 无）。
    desktop_shell: bool,
}

/// `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`（build / InstallationType 所在键）。
const CURRENT_VERSION_KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";

/// 读本机 build 号（`CurrentBuildNumber`；打不开键 / 值缺失 / 非数字 → `None`）。
///
/// **唯一实现**：本模块 [`os_info`]（选经典注册 / 稀疏包）与
/// `assoc::win::is_windows_11`（`ms-settings` 深链）都需要它，此前两处
/// 各抄了一遍「读 `CurrentBuildNumber` 再比对 22000」。`pub(crate)`：跨模块复用，
/// 不跨平台（与 [`notify_shell`] 同款取舍）。
///
/// 该值在注册表里是字符串，个别镜像会带空白——故按数值解析前先 `trim`
/// （与 [`os_info`] 原有行为一致；空白值本就不该影响「是否 Win11」的判定）。
pub(crate) fn windows_build_number() -> Option<u32> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(CURRENT_VERSION_KEY, KEY_READ)
        .ok()
        .and_then(|cv| build_number(&cv))
}

/// 从**已打开**的 `…\CurrentVersion` 键读 build 号（缺失 / 非数字 → `None`）。
///
/// 单独拆出是因为 [`os_info`] 已经打开了该键（还要读 `InstallationType` 等），
/// 不应为了 build 号再开一次。
fn build_number(cv: &RegKey) -> Option<u32> {
    let raw = cv.get_value::<String, _>("CurrentBuildNumber").ok()?;
    raw.trim().parse::<u32>().ok()
}

/// 读 OS 信息（注册表；读失败保守回落「有桌面 + build 0」→ 走经典注册）。
///
/// **不用 `GetVersionExW`**：它受 application manifest 兼容层影响，可能谎报版本。
fn os_info() -> OsInfo {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(cv) = hklm.open_subkey_with_flags(CURRENT_VERSION_KEY, KEY_READ) else {
        return OsInfo {
            build: 0,
            desktop_shell: true,
        };
    };
    let build = build_number(&cv).unwrap_or(0);
    let installation = cv
        .get_value::<String, _>("InstallationType")
        .unwrap_or_default();
    let desktop_shell = if installation.eq_ignore_ascii_case("Server") {
        // Server：仅 Desktop Experience（Server-Gui-Shell）才有资源管理器
        cv.open_subkey_with_flags("Server\\ServerLevels", KEY_READ)
            .map(|k| k.get_value::<u32, _>("Server-Gui-Shell").unwrap_or(0) == 1)
            .unwrap_or(false)
    } else {
        true
    };
    OsInfo {
        build,
        desktop_shell,
    }
}

// ---------------------------------------------------------------- shell 通知

/// 通知 shell 关联已变更（Explorer 立即重读 verb / 包注册）。
///
/// `pub(crate)`：文件关联后端（[`crate::assoc::win`]）做完注册表写入后也要发同一条
/// 通知——`SHCNE_ASSOCCHANGED` 是「Explorer 请重读关联」的唯一公开信号，两处共用
/// 一份实现（跨模块复用，不跨平台）。
pub(crate) fn notify_shell() {
    use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    // SAFETY: 纯通知调用；两个 item 指针按 API 约定传空。
    // `SHCNF_FLUSH`：等这次通知被处理完再返回（否则 Explorer 可能稍后才重读，
    // 用户在看「默认应用」页时看到的仍是旧值）。
    const SHCNF_FLUSH: u32 = 0x1000;
    // SAFETY: 纯通知调用；两个 item 指针按 API 约定传空。
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED as i32,
            SHCNF_IDLIST | SHCNF_FLUSH,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
}

#[cfg(test)]
#[path = "tests/win.rs"]
mod tests;
