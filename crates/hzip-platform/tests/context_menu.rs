//! 右键菜单后端契约测试（T-CM §2；三平台同一份断言）。
//!
//! 这些用例守的是**跨 crate / 跨平台的接口契约**，不是实现细节：
//!
//! 1. [`VERBS`] 的 key 集合必须与 `hzip-gui` 的 `CTX_EXTRACT_KEYS` /
//!    `CTX_COMPRESS_KEYS` 完全一致（两侧无编译期耦合，靠本表互锁——
//!    漏一个 key 的后果是「勾了但菜单里没有」，静默失效）；
//! 2. 每条 verb 的 argv 形状合法（首元素是旗标、占位符恰好一个）；
//! 3. scope 的落盘标识往返稳定（Windows 的 `shell-menu.json` 与 Linux
//!    产物都读它）；
//! 4. 未知 key 一律 `None`（白名单外不得被后端静默接受）；
//! 5. 菜单配置文件路径与 Windows DLL 的约定一致。

#![allow(clippy::unwrap_used, clippy::expect_used)]
// 真机验收用例（`#[ignore]`）需要把安装前后状态打给人看——这是它的用途。
#![allow(clippy::print_stdout)]

use hzip_platform::context_menu::{
    MenuEntry, MenuScope, MenuSpec, MenuStatus, VERBS, backend, entry_for, menu_config_path,
};

/// `hzip-gui::ui::contracts::CTX_EXTRACT_KEYS` 的 key 列（顺序一致）。
///
/// **改动此处必须同步改 GUI 侧常量**——两侧由本表互锁（GUI 侧另有一条
/// 反向断言：其白名单每个 key 都能在本 crate 命中）。
const GUI_EXTRACT_KEYS: [&str; 7] = [
    // 「用 hzip 打开」置顶（用户要求；顺序即设置页勾选格与资源管理器子菜单顺序）
    "open_hzip",
    "extract_here",
    "auto_extract",
    "extract_to",
    "extract_options",
    "extract_here_del",
    "extract_dir_del",
];

/// `hzip-gui::ui::contracts::CTX_COMPRESS_KEYS` 的 key 列（顺序一致）。
const GUI_COMPRESS_KEYS: [&str; 4] = ["add_zip", "add_tar", "add_7z", "add_archive"];

#[test]
fn verb_keys_match_gui_whitelists_exactly() {
    let ours: Vec<&str> = VERBS.iter().map(|(k, _)| *k).collect();
    let gui: Vec<&str> = GUI_EXTRACT_KEYS
        .iter()
        .chain(GUI_COMPRESS_KEYS.iter())
        .copied()
        .collect();
    assert_eq!(
        ours, gui,
        "VERBS 与 GUI 白名单必须逐项一一对应（顺序即菜单顺序）"
    );
}

#[test]
fn every_verb_has_well_formed_argv_and_scopes() {
    for (key, spec) in VERBS {
        assert!(!spec.argv.is_empty(), "{key}: argv 不得为空");
        assert!(
            spec.argv[0].starts_with("--"),
            "{key}: 首元素须为旗标，实际 {}",
            spec.argv[0]
        );
        assert!(!spec.scopes.is_empty(), "{key}: scope 不得为空");

        let placeholders = spec
            .argv
            .iter()
            .filter(|a| matches!(**a, "%SEL" | "%SELS"))
            .count();
        assert_eq!(
            placeholders, 1,
            "{key}: 占位符须恰好一个（多选语义唯一），实际 {placeholders}"
        );

        let per_file = spec.argv.contains(&"%SEL");
        let one_shot = spec.argv.contains(&"%SELS");
        assert!(
            per_file ^ one_shot,
            "{key}: 两种占位符不得同时出现（语义互斥）"
        );
        // 解压类协议一次只收一个档案 → 默认必须逐档案调用。
        //
        // 例外：**解压参数窗**（`extract_options`）刻意批次——它要先让用户
        // 在一张参数窗里把目标/冲突/编码定好，再对整批生效；逐档案调用会弹
        // N 张一模一样的对话框（Bandizip 的「解压选项」也是整批一次）。
        if key == "extract_options" {
            assert!(
                one_shot,
                "extract_options: 参数窗须一次收下全部选中档案（%SELS）"
            );
        } else if spec.scopes == [MenuScope::ArchiveFile] {
            assert!(per_file, "{key}: 解压类必须用 %SEL（一次一个档案）");
        } else {
            assert!(one_shot, "{key}: 压缩类必须用 %SELS（一次收全部路径）");
        }
    }
}

#[test]
fn archive_verbs_target_archives_and_compress_verbs_target_any() {
    for key in GUI_EXTRACT_KEYS {
        let spec = entry_for(key).unwrap();
        assert_eq!(
            spec.scopes,
            [MenuScope::ArchiveFile],
            "{key}: 解压类只应出现在档案上"
        );
    }
    for key in GUI_COMPRESS_KEYS {
        let spec = entry_for(key).unwrap();
        assert!(
            spec.scopes.contains(&MenuScope::Any),
            "{key}: 应对任意文件出现"
        );
        assert!(
            spec.scopes.contains(&MenuScope::DirectoryBackground),
            "{key}: 应对文件夹空白处出现"
        );
    }
}

#[test]
fn open_hzip_is_mounted_on_windows_too() {
    // 用户勾了「用 hzip 打开」就必须在右键里看到它。历史上它在 Windows 被
    // `win_skip` 过滤掉（理由是「ProgId 的 shell\open 已提供」），但那只在
    // hzip 是默认处理器时成立——没勾关联的用户看到的是「勾了却没项」。
    let spec = entry_for("open_hzip").unwrap();
    assert!(
        !spec.win_skip,
        "open_hzip 必须挂载：勾选项不得因平台过滤而静默失效"
    );
    assert_eq!(spec.argv, ["--open", "%SEL"]);
    assert_eq!(spec.scopes, [MenuScope::ArchiveFile]);
    // 其余条目同样不得被跳过（宁可多一个也不能少一个）
    for (key, s) in VERBS {
        assert!(!s.win_skip, "{key} 不应被 Windows 跳过");
    }
}

#[test]
fn unknown_keys_are_rejected_not_silently_accepted() {
    assert!(entry_for("").is_none());
    assert!(entry_for("nope").is_none());
    assert!(entry_for("EXTRACT_HERE").is_none(), "key 大小写敏感");
    assert!(entry_for("extract-here").is_none(), "key 用下划线");
}

#[test]
fn scope_wire_names_roundtrip() {
    for sc in [
        MenuScope::ArchiveFile,
        MenuScope::Any,
        MenuScope::DirectoryBackground,
    ] {
        assert_eq!(MenuScope::parse(sc.as_str()), Some(sc));
    }
    assert_eq!(MenuScope::parse("bogus"), None);
    // 具体字面量（DLL 与 Linux 产物读它，改动即破坏兼容）
    assert_eq!(MenuScope::ArchiveFile.as_str(), "archive_file");
    assert_eq!(MenuScope::Any.as_str(), "any");
    assert_eq!(MenuScope::DirectoryBackground.as_str(), "dir_background");
}

#[test]
fn menu_config_path_matches_windows_dll_convention() {
    let p = menu_config_path();
    assert_eq!(p.file_name().unwrap(), "shell-menu.json");
    assert_eq!(
        p.parent().unwrap().file_name().unwrap(),
        "hzip",
        "须落在 hzip 配置目录下（DLL 用 %APPDATA%\\hzip 解析）"
    );
}

#[test]
fn spec_resolved_skips_unknown_keys_and_counts_entries() {
    let spec = MenuSpec {
        exe_path: std::path::PathBuf::from("x"),
        extract: vec![
            MenuEntry {
                key: "extract_here",
                label: "a".into(),
            },
            MenuEntry {
                key: "not_a_real_key",
                label: "b".into(),
            },
        ],
        compress: vec![MenuEntry {
            key: "add_zip",
            label: "c".into(),
        }],
        archive_exts: vec!["zip".into()],
        shell_dll: None,
    };
    assert_eq!(spec.len(), 3, "len() 计的是条目数，与白名单无关");
    assert!(!spec.is_empty());
    assert_eq!(spec.resolved().count(), 2, "未知 key 必须被 resolved 过滤");
    assert_eq!(
        spec.entries().count(),
        3,
        "entries() 保持调用方给的顺序与数量"
    );
}

#[test]
fn empty_spec_reports_itself_empty() {
    let spec = MenuSpec {
        exe_path: std::path::PathBuf::from("x"),
        extract: vec![],
        compress: vec![],
        archive_exts: vec![],
        shell_dll: None,
    };
    assert!(spec.is_empty());
    assert_eq!(spec.len(), 0);
}

#[test]
fn install_requires_existing_executable() {
    let spec = MenuSpec {
        exe_path: std::path::PathBuf::from("definitely/not/here/hzip-gui"),
        extract: vec![MenuEntry {
            key: "extract_here",
            label: "a".into(),
        }],
        compress: vec![],
        archive_exts: vec![],
        shell_dll: None,
    };
    let err = backend().install(&spec).unwrap_err();
    // 文件不存在是 Spec 类；不支持类（Server Core）也在此路径上被接受
    assert!(
        matches!(
            err.kind,
            hzip_platform::context_menu::MenuErrorKind::Spec
                | hzip_platform::context_menu::MenuErrorKind::Unsupported
        ),
        "应拒绝不存在的可执行文件，实际 {err:?}"
    );
}

#[test]
fn status_is_callable_and_never_panics() {
    // status() 是设置页状态行数据源：必须始终可调用且自洽
    match backend().status() {
        MenuStatus::NotInstalled => {}
        MenuStatus::Installed { items, .. } => {
            let _ = items;
        }
        MenuStatus::NeedsRestart => {}
        MenuStatus::Degraded { reason } | MenuStatus::Unsupported { reason } => {
            assert!(!reason.is_empty(), "降级/不支持必须给出面向用户的原因");
        }
    }
}

// ---------------------------------------------------------------- 真机验收（手工触发）

/// **真机端到端验收**（默认 `#[ignore]`；CI 不跑，因为它会改本机注册表/安装包）。
///
/// 这是本功能唯一的验收途径：右键菜单是否真的出现在资源管理器里，无法用普通
/// 单元测试断言。它走的是**与图形界面「应用」按钮完全相同的生产代码路径**
/// （`backend().install` → `backend().status` → `backend().uninstall`）。
///
/// 用法（先把扩展产物放进 `HZIP_E2E_EXE` 同目录）：
///
/// ```text
/// set HZIP_E2E_EXE=D:\path\to\hzip-gui.exe
/// cargo test -p hzip-platform --test context_menu -- --ignored --nocapture e2e
/// ```
///
/// 前置：同目录下需有 `hzip-shell.dll`（必需）与 `hzip-shellmenu.msix`（Windows 11
/// 顶层菜单需要，缺省则回落经典注册）。可用 `build/shell-ext/build-shell-ext.ps1` 产出。
///
/// 验收动作：安装 → 断言状态非「未安装」→ 打印状态供人工核对 → 卸载 → 断言回到未安装。
#[test]
#[ignore = "真机验收：会修改本机注册表并注册 AppX 包（需 HZIP_E2E_EXE）"]
fn e2e_install_verify_uninstall() {
    use hzip_platform::context_menu::{InstallMode, MenuEntry};

    let Ok(exe) = std::env::var("HZIP_E2E_EXE") else {
        panic!("需设置 HZIP_E2E_EXE 指向 hzip-gui.exe（扩展产物与其同目录）");
    };
    let exe = std::path::PathBuf::from(exe);
    assert!(exe.is_file(), "可执行文件不存在：{}", exe.display());

    // 覆盖全部 10 个 key，确保菜单内容完整（open_hzip 在 Windows 会被跳过）
    let extract = GUI_EXTRACT_KEYS
        .iter()
        .map(|k| MenuEntry {
            key: k,
            label: format!("[e2e] {k}"),
        })
        .collect();
    let compress = GUI_COMPRESS_KEYS
        .iter()
        .map(|k| MenuEntry {
            key: k,
            label: format!("[e2e] {k}"),
        })
        .collect();
    let spec = MenuSpec {
        exe_path: exe,
        extract,
        compress,
        archive_exts: vec!["zip".into(), "7z".into(), "tar".into()],
        shell_dll: None,
    };

    let before = backend().status();
    println!("安装前状态：{before:?}");

    let report = backend()
        .install(&spec)
        .expect("安装应成功（缺 hzip-shell.dll 会报 Spec 类错误）");
    println!(
        "安装结果：mode={:?} items={} restart={}",
        report.mode, report.items, report.restart_required
    );
    assert_eq!(report.items, 10, "应写入 10 个条目");
    assert!(
        matches!(
            report.mode,
            InstallMode::ModernTopLevel | InstallMode::LegacyMenu
        ),
        "Windows 上只应是现代菜单或经典菜单"
    );

    let after = backend().status();
    println!("安装后状态：{after:?}");
    assert!(
        !matches!(after, MenuStatus::NotInstalled),
        "安装后状态不应是 NotInstalled"
    );
    // 经典路径在 Windows 11 上只会出现在「显示更多选项」→ 状态必须是 Degraded；
    // 在 Windows 10 上则是唯一菜单 → Installed(LegacyMenu)。二者都自洽，
    // 不允许出现「装了却报 NotInstalled」（那会让设置页把用户引向错误的排查方向）。
    // 经典回退路径下的状态必须**自洽**：要么如实报 LegacyMenu（Win10，或本机新式
    // 菜单被经典菜单开关禁用），要么报 Degraded（Win11 上只进「显示更多选项」）；
    // 绝不允许出现「装了却报 NotInstalled」——那会把用户引向错误的排查方向。
    //
    // 注意：本机可能同时存在**稀疏包注册**（前次安装残留）与经典回退，此时状态按
    // 用户实际看到的菜单判定，故 ModernTopLevel 也在允许集合内。
    if report.mode == InstallMode::LegacyMenu {
        let consistent = matches!(
            after,
            MenuStatus::Degraded { .. }
                | MenuStatus::Installed {
                    mode: InstallMode::LegacyMenu,
                    ..
                }
                | MenuStatus::Installed {
                    mode: InstallMode::ModernTopLevel,
                    ..
                }
        );
        assert!(consistent, "经典路径状态应自洽，实际 {after:?}");
    }

    println!("--- 请人工确认：资源管理器右键 → hzip 子菜单（含 [e2e] 前缀条目） ---");

    backend().uninstall().expect("卸载应成功");
    let un = backend().status();
    println!("卸载后状态：{un:?}");
    assert!(
        matches!(un, MenuStatus::NotInstalled),
        "卸载后应回到未安装，实际 {un:?}"
    );
}

// ---------------------------------------------------------------- CLSID 一致性（防「菜单不出现」）

/// **四处 CLSID 必须逐字一致**——这是本功能最隐蔽的失败模式。
///
/// 四个位置分属三个编译单元（platform 库 / 独立 workspace 的 DLL / 包清单 /
/// 构建脚本），Rust 类型系统管不到它们。历史上曾让经典注册与 DLL 各用一个
/// CLSID，结果资源管理器加载 DLL 后拿到 `CLASS_E_CLASSNOTAVAILABLE`：
/// **右键菜单一项都不出现，且没有任何错误提示**（只在 DebugView 里能看到）。
///
/// 本测试直接读源文件比对，是这类「跨编译单元常量漂移」唯一可行的自动防线。
#[test]
fn clsid_is_identical_across_all_four_sites() {
    use std::fs;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace 根");

    // ① platform：注册表写入所用的 CLSID（形如 "{A7C4D1E8-...}"）
    let win_rs = fs::read_to_string(root.join("crates/hzip-platform/src/context_menu/win.rs"))
        .expect("读 win.rs");
    let plat = extract(&win_rs, "pub const CLSID: &str = \"").expect("win.rs 应定义 CLSID");
    assert!(
        plat.starts_with('{') && plat.ends_with('}'),
        "注册表 CLSID 须带花括号，实际 {plat}"
    );

    // ② DLL：DllGetClassObject 只认这一个（GUID::from_u128 字面量）
    let lib_rs = fs::read_to_string(root.join("shell/hzip-shell/src/lib.rs")).expect("读 lib.rs");
    let dll = extract_until(&lib_rs, "pub const CLSID: GUID = GUID::from_u128(0x", ')')
        .expect("DLL 应定义 CLSID");
    let dll_braced = format!("{{{}}}", format_guid(&dll));

    // ③ 包清单：占位符由构建脚本替换；此处校验脚本里的字面量
    let ps1 =
        fs::read_to_string(root.join("build/shell-ext/build-shell-ext.ps1")).expect("读构建脚本");
    let script = extract(&ps1, "$clsid = \"").expect("构建脚本应定义 $clsid");
    let script_braced = format!("{{{script}}}");

    assert_eq!(
        plat.to_ascii_uppercase(),
        dll_braced.to_ascii_uppercase(),
        "平台注册表 CLSID 与 DLL 接受的 CLSID 不一致 → 右键菜单不会出现"
    );
    assert_eq!(
        plat.to_ascii_uppercase(),
        script_braced.to_ascii_uppercase(),
        "平台注册表 CLSID 与包清单 CLSID 不一致 → 现代菜单不会出现"
    );

    // ④ 包清单模板里的占位符名（构建脚本按此名替换，写错则清单带原始占位符提交）
    let manifest =
        fs::read_to_string(root.join("shell/hzip-shell/appx/AppxManifest.xml")).expect("读包清单");
    assert!(
        manifest.contains("{{CLSID}}"),
        "清单须保留 {{{{CLSID}}}} 占位符供构建脚本替换"
    );
}

/// 取 `prefix` 之后到下一个 `"` 的内容（用于 Rust 字符串常量 / PowerShell 双引号值）。
fn extract(hay: &str, prefix: &str) -> Option<String> {
    extract_until(hay, prefix, '"')
}

/// 取 `prefix` 之后到下一个 `until` 的内容。
fn extract_until(hay: &str, prefix: &str, until: char) -> Option<String> {
    let start = hay.find(prefix)? + prefix.len();
    let rest = &hay[start..];
    let end = rest.find(until)?;
    Some(rest[..end].to_string())
}

/// `0xA7C4D1E8_5B93_4F60_9E27_8D3A1B5C7F04` → `A7C4D1E8-5B93-4F60-9E27-8D3A1B5C7F04`。
fn format_guid(raw: &str) -> String {
    let hex: String = raw
        .trim_start_matches("0x")
        .chars()
        .filter(|c| *c != '_')
        .collect();
    assert_eq!(hex.len(), 32, "GUID 应为 32 个十六进制位，实际 {raw}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

// ---------------------------------------------------------------- COM 契约（真机，跨 crate）

/// **DLL ↔ 注册表契约的真机验证**（默认 `#[ignore]`：需先装好扩展产物）。
///
/// # 为什么必须有这条（它抓到过两个真 bug）
///
/// DLL 是**独立 workspace**，其单元测试只覆盖自身内部逻辑；platform 侧的注册表
/// 测试只覆盖键的形状。两者之间的**接缝无人看管**，而接缝出错的表现是
/// 「右键菜单一项都不出现、且没有任何报错」：
///
/// 1. **CLSID 不一致**：经典注册写的 CLSID 与 DLL `DllGetClassObject` 接受的
///    不一致 → 资源管理器拿到 `CLASS_E_CLASSNOTAVAILABLE`，菜单静默消失。
///    （`clsid_is_identical_across_all_four_sites` 守常量，本用例守**实际链路**。）
/// 2. **未对齐指针解引用**：DLL 直接 `*rclsid` 时，调用方给未对齐指针会在 debug
///    下 abort —— 在 Explorer 进程内即整个桌面外壳崩溃。
///
/// 因此本用例走**完整真实链路**：注册表 → `LoadLibrary` → `DllGetClassObject`
/// → `IClassFactory::CreateInstance` → `IExplorerCommand::GetTitle` /
/// `EnumSubCommands` → 子项 `GetTitle`，并断言菜单内容**等同于设置页写入
/// `shell-menu.json` 的条目**。
///
/// 用法：
/// ```text
/// set HZIP_ACCEPT_EXE=D:\path\to\hzip-gui.exe
/// set HZIP_ACCEPT_DLL=D:\path\to\hzip-shell.dll
/// cargo test -p hzip-platform --test context_menu -- --ignored --nocapture acceptance_menu_contents
/// ```
#[cfg(windows)]
#[test]
#[ignore = "真机验收：需装好 hzip-shell.dll（HZIP_ACCEPT_EXE / HZIP_ACCEPT_DLL）"]
fn acceptance_menu_contents_via_com() {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    let exe = std::env::var("HZIP_ACCEPT_EXE").expect("需 HZIP_ACCEPT_EXE");
    let dll = std::env::var("HZIP_ACCEPT_DLL").expect("需 HZIP_ACCEPT_DLL");

    // ① 用生产代码路径安装（写入注册表 + shell-menu.json）
    let spec = MenuSpec {
        exe_path: std::path::PathBuf::from(&exe),
        extract: vec![
            MenuEntry {
                key: "extract_here",
                label: "解压到当前文件夹".into(),
            },
            MenuEntry {
                key: "auto_extract",
                label: "智能解压".into(),
            },
        ],
        compress: vec![MenuEntry {
            key: "add_zip",
            label: "添加到 zip".into(),
        }],
        archive_exts: vec!["zip".into(), "7z".into()],
        shell_dll: None,
    };
    backend().install(&spec).expect("安装应成功");

    // ② 加载 DLL，走完整 COM 链路
    let wide: Vec<u16> = dll.encode_utf16().chain(std::iter::once(0)).collect();
    let h = unsafe { LoadLibraryW(wide.as_ptr()) };
    assert!(!h.is_null(), "加载 {dll} 失败");

    type GetClassObject =
        unsafe extern "system" fn(*const u8, *const u8, *mut *mut core::ffi::c_void) -> i32;
    let get_class_object: GetClassObject = unsafe {
        std::mem::transmute(
            GetProcAddress(h, c"DllGetClassObject".as_ptr().cast())
                .expect("DLL 缺 DllGetClassObject 导出"),
        )
    };

    // CLSID 取自 platform 自身的常量，避免测试与实现各写一份
    let clsid = GuidBytes::parse(win_clsid_for_test());
    let iid_class_factory = GuidBytes::parse("{00000001-0000-0000-C000-000000000046}");
    let iid_explorer_command = GuidBytes::parse("{a08ce4d0-fa25-44ab-b57c-c7b1c323e0b9}");

    let mut factory: *mut core::ffi::c_void = std::ptr::null_mut();
    assert_eq!(
        unsafe { get_class_object(clsid.ptr(), iid_class_factory.ptr(), &mut factory) },
        0,
        "IClassFactory 必须可用（CLSID 与 DLL 不一致时这里失败）"
    );

    let vtable = unsafe { *(factory as *const *const usize) };
    type CreateInstance = unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *mut core::ffi::c_void,
        *const u8,
        *mut *mut core::ffi::c_void,
    ) -> i32;
    let create: CreateInstance = unsafe { std::mem::transmute(*vtable.add(3)) };
    let mut command: *mut core::ffi::c_void = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            create(
                factory,
                std::ptr::null_mut(),
                iid_explorer_command.ptr(),
                &mut command,
            )
        },
        0,
        "IExplorerCommand 必须可查"
    );

    // IExplorerCommand vtable（IUnknown 占前 3 项）：3=GetTitle, 10=EnumSubCommands
    let cvt = unsafe { *(command as *const *const usize) };
    type GetTitle = unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *mut core::ffi::c_void,
        *mut *mut u16,
    ) -> i32;
    let get_title: GetTitle = unsafe { std::mem::transmute(*cvt.add(3)) };
    let enum_sub: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *mut *mut core::ffi::c_void,
    ) -> i32 = unsafe { std::mem::transmute(*cvt.add(10)) };

    let mut title: *mut u16 = std::ptr::null_mut();
    assert_eq!(
        unsafe { get_title(command, std::ptr::null_mut(), &mut title) },
        0
    );
    assert_eq!(
        unsafe { take_wide(title) }.as_deref(),
        Some("hzip"),
        "顶层菜单标题"
    );

    let mut en: *mut core::ffi::c_void = std::ptr::null_mut();
    assert_eq!(
        unsafe { enum_sub(command, &mut en) },
        0,
        "根命令须给出子命令"
    );

    // IEnumExplorerCommand：3=Next
    let evt = unsafe { *(en as *const *const usize) };
    let next: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        u32,
        *mut *mut core::ffi::c_void,
        *mut u32,
    ) -> i32 = unsafe { std::mem::transmute(*evt.add(3)) };

    let mut labels = Vec::new();
    loop {
        let mut child: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut got: u32 = 0;
        let hr = unsafe { next(en, 1, &mut child, &mut got) };
        if got == 0 || hr != 0 {
            break;
        }
        let child_vt = unsafe { *(child as *const *const usize) };
        let child_title: GetTitle = unsafe { std::mem::transmute(*child_vt.add(3)) };
        let mut t: *mut u16 = std::ptr::null_mut();
        unsafe { child_title(child, std::ptr::null_mut(), &mut t) };
        if let Some(s) = unsafe { take_wide(t) } {
            labels.push(s);
        }
    }

    // ③ 菜单内容必须等于安装时写入的条目（证明「勾选 → 菜单」链路真的通）
    assert_eq!(
        labels,
        vec!["解压到当前文件夹", "智能解压", "添加到 zip"],
        "菜单条目须与 shell-menu.json 一致"
    );

    // ④ 收尾：不留痕迹
    backend().uninstall().expect("卸载应成功");
    assert!(matches!(backend().status(), MenuStatus::NotInstalled));

    /// 取走 DLL 用 `SHStrDupW` 分配的宽字符串（CoTaskMem）。
    unsafe fn take_wide(p: *mut u16) -> Option<String> {
        if p.is_null() {
            return None;
        }
        let mut len = 0usize;
        while unsafe { *p.add(len) } != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
        unsafe { CoTaskMemFree(p as *const core::ffi::c_void) };
        Some(s)
    }
}

/// 供验收用例读取的 CLSID（与实现同源，避免测试自带一份常量）。
#[cfg(windows)]
fn win_clsid_for_test() -> &'static str {
    hzip_platform::context_menu::COMPONENT_CLSID.expect("Windows 上必有 CLSID")
}

/// 4 字节对齐的 GUID 字节容器（`[u8;16]` 仅 1 字节对齐，解引用为 GUID 属未对齐访问）。
#[cfg(windows)]
struct GuidBytes(#[allow(dead_code)] [u8; 16]);

#[cfg(windows)]
impl GuidBytes {
    /// 解析 `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`（大端字段序，与内存布局一致）。
    fn parse(s: &str) -> Self {
        let hex: String = s.chars().filter(char::is_ascii_hexdigit).collect();
        assert_eq!(hex.len(), 32, "GUID 字面量长度异常: {s}");
        let mut raw = [0u8; 16];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("GUID 十六进制");
        }
        // 字段序：前 3 字段小端，其余按序
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&[raw[3], raw[2], raw[1], raw[0]]);
        out[4..6].copy_from_slice(&[raw[5], raw[4]]);
        out[6..8].copy_from_slice(&[raw[7], raw[6]]);
        out[8..16].copy_from_slice(&raw[8..16]);
        Self(out)
    }

    /// 供 FFI 的指针（4 字节对齐，满足 GUID 对齐要求）。
    fn ptr(&self) -> *const u8 {
        self.0.as_ptr()
    }
}
