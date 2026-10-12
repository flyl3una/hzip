#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::context_menu::{MenuEntry, MenuScope, entry_for};
use std::path::PathBuf;

/// 沙箱根：绝不触碰真实 `HKCU\Software\Classes`（那里改 `*`/`Directory`
/// 默认动词会影响用户双击行为）。
const TEST_CLASSES: &str = "Software\\hzip\\__ctxmenu_test__\\Classes";
const TEST_BACKUP: &str = "Software\\hzip\\__ctxmenu_test__\\backup";

/// 序列化：多个用例同写 HKCU 沙箱。
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn spec() -> MenuSpec {
    MenuSpec {
        exe_path: PathBuf::from(r"C:\Program Files\hzip\hzip-gui.exe"),
        extract: vec![
            MenuEntry {
                key: "extract_here",
                label: "解压到当前文件夹".into(),
            },
            MenuEntry {
                key: "open_hzip",
                label: "用 hzip 打开".into(),
            },
        ],
        compress: vec![MenuEntry {
            key: "add_zip",
            label: "添加到 zip".into(),
        }],
        archive_exts: vec!["zip".into(), "7z".into()],
        shell_dll: None,
    }
}

fn temp_config(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("hzip-ctxmenu-{tag}-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn cleanup() {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let _ = hkcu.delete_subkey_all("Software\\hzip\\__ctxmenu_test__");
}

#[test]
fn classic_registration_roundtrip_is_clean() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    cleanup();

    let cfg = temp_config("roundtrip");
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // —— 句柄作用域：本块的 RegKey 句柄全部在块末释放 ——
    // 注册表把「已 RegDeleteTree 但仍有打开句柄」的键标记为待删而非立即移除，
    // `RegQueryInfoKey` 仍能数到它。若句柄泄漏到下面，测的就是测试自己的句柄
    // 语义而不是生产的修剪行为（第一版测试就栽在这里）。
    {
        let classes = hkcu.create_subkey(TEST_CLASSES).unwrap().0;
        // 预置一个「用户原本就有的」默认动词值：卸载必须把它还原
        let star_shell = classes.create_subkey("*\\Shell").unwrap().0;
        star_shell.set_value("", &"open").unwrap();

        let dll = PathBuf::from(r"C:\Program Files\hzip\hzip-shell.dll");
        let mut errors = Vec::new();
        write_classic(
            TEST_CLASSES,
            TEST_BACKUP,
            r"C:\Program Files\hzip\hzip-gui.exe",
            &dll,
            &mut errors,
        );
        write_menu_config_at(&cfg, &spec()).unwrap();
        assert!(errors.is_empty(), "写入不应报错: {errors:?}");

        // 三个作用域各挂到 verb，且是 ExplorerCommandHandler（COM 路径）
        for prog in SCOPES {
            let k = classes
                .open_subkey(format!("{prog}\\shell\\{VERB}"))
                .unwrap_or_else(|e| panic!("{prog} 未挂载: {e}"));
            assert_eq!(
                k.get_value::<String, _>("ExplorerCommandHandler").unwrap(),
                CLSID
            );
            assert_eq!(k.get_value::<String, _>("MUIVerb").unwrap(), MUI_VERB);
        }
        // CLSID 指向 DLL
        let inproc = classes
            .open_subkey(format!("CLSID\\{CLSID}\\InProcServer32"))
            .unwrap();
        assert_eq!(
            inproc.get_value::<String, _>("").unwrap(),
            dll.to_string_lossy()
        );
        // 默认动词保护：`*` 被置 none，原值进备份
        assert_eq!(star_shell.get_value::<String, _>("").unwrap(), "none");
        let backup = hkcu.open_subkey(TEST_BACKUP).unwrap();
        assert_eq!(backup.get_value::<String, _>("*").unwrap(), "open");
    }

    // 配置：**勾了什么就落什么**（逐项与 spec 对应，不再有平台过滤）
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["archive_exts"], serde_json::json!(["zip", "7z"]));
    let archive = doc["items"]["archive_file"].as_array().unwrap();
    assert_eq!(
        archive.len(),
        2,
        "解压组两项都应落盘（含 open_hzip）: {doc}"
    );
    assert_eq!(archive[0]["key"], "extract_here");
    assert_eq!(archive[1]["key"], "open_hzip");
    assert_eq!(doc["items"]["any"].as_array().unwrap().len(), 1);
    assert_eq!(doc["items"]["any"][0]["key"], "add_zip");
    // schema 只两组：压缩类对文件夹与空白处同组适用（shell 在空白处传的是该
    // 文件夹本身），故没有独立的 dir_background 组——DLL 侧也按此解析。
    assert!(
        doc["items"]["dir_background"].is_null(),
        "不应再写已废弃的 dir_background 组: {doc}"
    );
    // 条目数 = 各 scope 组之和（组间互斥）：archive_file 2 项 + any 1 项 = 3。
    // 取最大值会漏报，状态行就会把实际项数说少。
    assert_eq!(read_menu_config_items_at(&cfg), 3);

    // 卸载：全清 + 默认动词还原
    let mut errs = Vec::new();
    clear_classic(TEST_CLASSES, TEST_BACKUP, &mut errs);
    std::fs::remove_file(&cfg).unwrap();
    assert!(errs.is_empty(), "清理不应报错: {errs:?}");

    {
        let classes = hkcu.open_subkey(TEST_CLASSES).unwrap();
        for prog in SCOPES {
            assert!(
                classes
                    .open_subkey(format!("{prog}\\shell\\{VERB}"))
                    .is_err(),
                "{prog} verb 应已删除"
            );
        }
        assert!(classes.open_subkey(format!("CLSID\\{CLSID}")).is_err());
        let star_shell = classes.open_subkey("*\\Shell").unwrap();
        assert_eq!(
            star_shell.get_value::<String, _>("").unwrap(),
            "open",
            "默认动词必须还原为原值"
        );
    }
    assert!(
        hkcu.open_subkey(TEST_BACKUP).is_err(),
        "备份键清空后不应留空壳"
    );

    // 再跑一次清理（幂等）：不得因重复清理而报错，也不得越界删掉别人的键。
    let mut errs2 = Vec::new();
    clear_classic(TEST_CLASSES, TEST_BACKUP, &mut errs2);
    assert!(errs2.is_empty(), "重复清理不应报错: {errs2:?}");
    assert!(
        hkcu.open_subkey("Software\\Classes").is_ok(),
        "清理绝不许触碰系统 Classes 键"
    );
    assert!(
        hkcu.open_subkey(TEST_CLASSES).is_ok(),
        "沙箱夹具键不属于我们，不得被删"
    );
    cleanup();
}

/// 备份键的**空父链**必须被修剪干净（否则「清除」会在注册表里留下空壳键）。
///
/// 用一条只属于我们的链来测（`backup` 自身按契约由调用方删除后调用 prune）。
#[test]
fn prune_removes_our_empty_backup_chain_without_touching_system_keys() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let chain = "Software\\hzip\\__prune_test__\\backup";
    hkcu.create_subkey(chain).unwrap();
    assert!(hkcu.open_subkey("Software\\hzip\\__prune_test__").is_ok());

    // 调用方契约：先删备份键本身，再修剪其父链
    hkcu.delete_subkey_all(chain).unwrap();
    let mut errs = Vec::new();
    prune_empty_parents(chain, &mut errs);
    assert!(errs.is_empty(), "修剪不应报错: {errs:?}");

    assert!(
        hkcu.open_subkey("Software\\hzip\\__prune_test__").is_err(),
        "空父链应被完整修剪"
    );
    assert!(
        hkcu.open_subkey("Software\\Classes").is_ok(),
        "修剪必须止于 Software 层，绝不触碰系统键"
    );
}

#[test]
fn absent_default_verb_is_removed_not_replaced_with_none() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    cleanup();

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu.create_subkey(TEST_CLASSES).unwrap().0;
    // 故意不预置 `*` 的默认值（本机真实情形：只有子键、无默认值）
    classes.create_subkey("*\\Shell").unwrap();

    let dll = PathBuf::from(r"C:\x.dll");
    let mut errors = Vec::new();
    write_classic(TEST_CLASSES, TEST_BACKUP, r"C:\x.exe", &dll, &mut errors);
    assert!(errors.is_empty(), "{errors:?}");

    let star_shell = classes
        .open_subkey_with_flags("*\\Shell", KEY_READ)
        .unwrap();
    assert_eq!(star_shell.get_value::<String, _>("").unwrap(), "none");

    let mut errs = Vec::new();
    clear_classic(TEST_CLASSES, TEST_BACKUP, &mut errs);
    assert!(errs.is_empty(), "{errs:?}");
    assert!(
        star_shell.get_value::<String, _>("").is_err(),
        "原本无默认值 → 必须删值，不能留下 \"none\""
    );
    cleanup();
}

/// 状态决策表（纯函数，覆盖「两条注册路径 × 新式菜单开关 × 系统版本」）。
///
/// 这张表守的是一次真实误判：早期 `status()` 先看包是否注册就报
/// `ModernTopLevel`，而本机新式菜单被经典菜单开关禁用——用户实际什么都看不到，
/// 状态行却说「已安装到新式菜单」。两者必须一致，否则排查方向被彻底带偏。
#[test]
fn status_decision_table_is_consistent_with_what_the_user_sees() {
    use InstallMode::{LegacyMenu, ModernTopLevel};
    let win11 = 26200u32;
    let win10 = 19045u32;

    // 桌面体验缺失 → 不支持
    assert!(matches!(
        decide_status(false, win11, false, false, false, 0),
        MenuStatus::Unsupported { .. }
    ));

    // 新式菜单可用 + 包已注册 → 顶层
    assert_eq!(
        decide_status(true, win11, false, true, false, 9),
        MenuStatus::Installed {
            mode: ModernTopLevel,
            items: 9
        }
    );
    // 新式菜单被禁用 + 经典已写 → 用户看到经典菜单，报 LegacyMenu（不是 Degraded）
    assert_eq!(
        decide_status(true, win11, true, true, true, 9),
        MenuStatus::Installed {
            mode: LegacyMenu,
            items: 9
        }
    );
    assert_eq!(
        decide_status(true, win11, true, false, true, 9),
        MenuStatus::Installed {
            mode: LegacyMenu,
            items: 9
        }
    );
    // 新式菜单可用 + 只有经典（Win11）→ 仅「显示更多选项」→ Degraded
    assert!(matches!(
        decide_status(true, win11, true, false, false, 9),
        MenuStatus::Degraded { .. }
    ));
    // 新式菜单可用 + 只有经典（Win10）→ 那就是唯一菜单 → Installed
    assert_eq!(
        decide_status(true, win10, true, false, false, 9),
        MenuStatus::Installed {
            mode: LegacyMenu,
            items: 9
        }
    );
    // 包已注册但新式菜单被禁用、经典又没写 → 用户什么都看不到 → Degraded（不是 Installed）
    assert!(matches!(
        decide_status(true, win11, false, true, true, 9),
        MenuStatus::Degraded { .. }
    ));
    // 什么都没装
    assert_eq!(
        decide_status(true, win11, false, false, false, 0),
        MenuStatus::NotInstalled
    );
}

/// 经典菜单开关探测：只读、不 panic（本机装了该开关时返回 true）。
///
/// 这条的价值在**行为契约**而非取值：探测绝不能写注册表（它是每次 install/status
/// 都会跑的探测），且必须能容忍键不存在。
#[test]
fn classic_menu_switch_probe_is_read_only_and_tolerant() {
    let before = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            "Software\\Classes\\CLSID\\{86CA1AA0-34AA-4E8B-A509-50C905BAE2A2}\\InprocServer32",
            KEY_READ,
        )
        .is_ok();
    let probed = modern_menu_disabled();
    let after = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            "Software\\Classes\\CLSID\\{86CA1AA0-34AA-4E8B-A509-50C905BAE2A2}\\InprocServer32",
            KEY_READ,
        )
        .is_ok();
    assert_eq!(probed, before, "探测结果应等于键是否存在");
    assert_eq!(before, after, "探测不得改动注册表");
}

#[test]
fn clear_classic_on_pristine_registry_is_a_noop() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    cleanup();
    let mut errs = Vec::new();
    clear_classic(TEST_CLASSES, TEST_BACKUP, &mut errs);
    assert!(errs.is_empty(), "空注册表清理不得报错: {errs:?}");
}

/// `refresh_capabilities` 只替换**能力集**字段（`archive_exts`），
/// 用户勾选（`items`）与 `exe`/`schema` 原样保留；未安装 = no-op；
/// 已是最新 = 不写盘。
///
/// 守的缺陷：升级新增格式（rar / `tgz` 别名）后，右键菜单仍按旧清单把
/// `.rar` 当非档案 → 解压项被 `ECS_HIDDEN` 隐藏（用户实测：勾了 rar 关联
/// 也没用——关联管的是 OS 默认程序，与解压能力无关）。
#[test]
fn refresh_capabilities_patches_archive_exts_only() {
    let cfg = temp_config("refresh");
    assert!(
        !refresh_archive_exts_at(&cfg, &["zip".into()]).unwrap(),
        "未安装（无文件）→ no-op"
    );

    // 已安装的配置：能力集是旧的，且带用户勾选（items 不得被动）
    let orig = serde_json::json!({
        "schema": 1,
        "exe": "C:/hzip/hzip-gui.exe",
        "archive_exts": ["7z", "bz2", "gz", "tar", "xz", "zip", "zst"],
        "items": {
            "archive_file": [{"key": "open_hzip", "label": "用 hzip 打开", "args": ["--open", "%SEL"]}],
            "any": [{"key": "add_7z", "label": "添加到 7z", "args": ["--add", "%SEL"]}],
        },
    });
    std::fs::write(&cfg, serde_json::to_string_pretty(&orig).unwrap()).unwrap();

    // 升级后能力集（含 rar 与别名）→ 回写，且**只**改该字段
    let want: Vec<String> = ["7z", "bz2", "gz", "rar", "tar", "tgz", "xz", "zip", "zst"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(
        refresh_archive_exts_at(&cfg, &want).unwrap(),
        "应报告已回写"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(doc["archive_exts"], serde_json::json!(want));
    assert_eq!(doc["exe"], "C:/hzip/hzip-gui.exe", "exe 不得被动");
    assert_eq!(doc["schema"], 1, "schema 不得被动");
    assert_eq!(
        doc["items"]["archive_file"][0]["key"], "open_hzip",
        "用户勾选（items）不得被动——那是「应用」按钮的职责"
    );
    assert_eq!(doc["items"]["any"][0]["key"], "add_7z");

    // 已是最新 → 不再写盘
    assert!(
        !refresh_archive_exts_at(&cfg, &want).unwrap(),
        "无变化不应回写"
    );
    let _ = std::fs::remove_file(&cfg);
}

/// `sync` 只重写配置、绝不触碰注册表；且必须如实反映缩小后的勾选。
///
/// 守的是「设置与系统菜单不一致」这个实测缺陷：条目集变了而配置没变，
/// 用户右键看到的就还是旧内容。
#[test]
fn sync_rewrites_only_the_config_and_validates() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = crate::testutil::temp_dir("ctxmenu-sync");
    let exe = dir.path().join("hzip-gui.exe");
    std::fs::write(&exe, b"x").unwrap();
    std::fs::write(dir.path().join(DLL_NAME), b"x").unwrap();

    let cfg = temp_config("sync");
    let mut full = spec();
    full.exe_path = exe.clone();
    // spec(): 解压组 2 项（open_hzip 在 Windows 被跳过）+ 压缩组 1 项 → 2 项
    sync_at(&full, &cfg).unwrap();
    assert_eq!(read_menu_config_items_at(&cfg), 3);

    // 缩小勾选 → sync 后配置必须只剩 1 项
    let mut reduced = spec();
    reduced.exe_path = exe;
    reduced.compress.clear();
    sync_at(&reduced, &cfg).unwrap();
    assert_eq!(
        read_menu_config_items_at(&cfg),
        2,
        "sync 必须反映缩小后的勾选（否则菜单与设置不一致）"
    );

    // 不触碰注册表：沙箱类根在 sync 前后都不存在
    assert!(!classic_installed_at(TEST_CLASSES), "sync 不得动注册表");

    // 规格非法 → 报错且**不留半套配置**
    let before = std::fs::read_to_string(&cfg).unwrap();
    let mut bad = spec();
    bad.exe_path = dir.path().join("missing.exe");
    assert!(sync_at(&bad, &cfg).is_err());
    assert_eq!(
        std::fs::read_to_string(&cfg).unwrap(),
        before,
        "校验失败不得改动配置"
    );
    let _ = std::fs::remove_file(&cfg);
}

#[test]
fn install_rejects_empty_spec_and_missing_exe() {
    let empty = MenuSpec {
        exe_path: PathBuf::from(r"C:\nope\hzip-gui.exe"),
        extract: vec![],
        compress: vec![],
        archive_exts: vec![],
        shell_dll: None,
    };
    assert_eq!(install(&empty).unwrap_err().kind, MenuErrorKind::Spec);

    let missing = MenuSpec {
        exe_path: PathBuf::from(r"C:\definitely\missing\hzip-gui.exe"),
        extract: vec![MenuEntry {
            key: "extract_here",
            label: "x".into(),
        }],
        compress: vec![],
        archive_exts: vec![],
        shell_dll: None,
    };
    assert_eq!(install(&missing).unwrap_err().kind, MenuErrorKind::Spec);
}

/// DLL 路径覆盖（T-SEL 走查 #14）：`shell_dll = Some(path)` → 校验/注册
/// 用该路径；`None` → exe 同目录默认（`hzip-shell.dll`）。
/// 覆盖动机：DLL 与主程序分目录部署时，注册表 `InProcServer32` 必须
/// 指向真实 DLL 位置，否则右键菜单静默不出现。
#[test]
fn validate_spec_honors_configured_dll_path() {
    let dir = std::env::temp_dir().join(format!("hzip-ctxmenu-dllpath-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("hzip-gui.exe");
    std::fs::write(&exe, b"x").unwrap();
    let ext_dir = dir.join("ext");
    std::fs::create_dir_all(&ext_dir).unwrap();
    let custom_dll = ext_dir.join("hzip-shell.dll");
    std::fs::write(&custom_dll, b"x").unwrap();

    let mut s = spec();
    s.exe_path = exe.clone();

    // 配置路径优先：exe 同目录没有 DLL 也必须通过（注册用自定义路径）
    s.shell_dll = Some(custom_dll.clone());
    let (_, _, dll) = validate_spec(&os_info(), &s).expect("自定义 DLL 路径应被接受");
    assert_eq!(dll, custom_dll, "注册 DLL 必须用配置路径（非 exe 同目录）");

    // 未指定 → exe 同目录默认查找（补一个同名文件后通过）
    s.shell_dll = None;
    let default_dll = dir.join(DLL_NAME);
    std::fs::write(&default_dll, b"x").unwrap();
    let (_, _, dll) = validate_spec(&os_info(), &s).expect("默认路径应被接受");
    assert_eq!(dll, default_dll);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn config_lookup_failures_degrade_to_zero_items() {
    assert_eq!(read_menu_config_items_at(Path::new(r"C:\nope\x.json")), 0);
    let p = temp_config("garbage");
    std::fs::write(&p, "{ not json").unwrap();
    assert_eq!(read_menu_config_items_at(&p), 0, "坏配置不得 panic");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn ps_quote_escapes_single_quotes_for_powershell_literals() {
    assert_eq!(ps_quote(r"C:\a b\x.msix"), r"'C:\a b\x.msix'");
    assert_eq!(ps_quote("it's"), "'it''s'");
}

#[test]
fn scopes_map_to_stable_wire_names() {
    for (key, _) in crate::context_menu::VERBS {
        let def = entry_for(key).unwrap();
        for sc in def.scopes {
            assert_eq!(MenuScope::parse(sc.as_str()), Some(*sc), "{key}");
        }
    }
}

#[test]
fn os_info_reports_this_machine_as_desktop_shell() {
    let os = os_info();
    assert!(os.desktop_shell, "开发/验收机应具备桌面 shell");
    assert!(os.build > 0, "应能读到 build 号，实际 {}", os.build);
}

/// build 号探测只有一份实现：[`windows_build_number`] 与 [`os_info`] 必须同源。
///
/// `assoc::win::is_windows_11`（Win11 深链）走的是前者、本模块选经典/稀疏包
/// 走的是后者；两者漂移会出现「按 A 判 Win11、按 B 选注册路径」的自相矛盾。
#[test]
fn os_info_build_matches_shared_probe() {
    if let Some(build) = windows_build_number() {
        assert_eq!(os_info().build, build, "两处 build 号应同源");
    }
}
