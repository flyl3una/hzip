use super::*;

/// 菜单规格 == 设置勾选（**逐项一致**）。
///
/// 守的是一个实测缺陷：右键菜单里出现了用户**没勾**的项（设置里 3+3 项，
/// 菜单里却是上一次安装的 9 项）。根因之一正是「勾选没有同步到系统」，但
/// 「同步什么」由本函数决定——它必须严格按勾选过滤、按白名单顺序输出，
/// 且文案随当前语言（菜单是给人看的，落盘 key 才是不变的那一侧）。
#[test]
fn menu_spec_mirrors_checked_items_exactly() {
    use crate::viewmodel::SettingId;
    let engine = std::sync::Arc::new(
        hzip_core::engine::Engine::new(hzip_core::engine::EngineConfig::default()).unwrap(),
    );
    let mut d = crate::app::state::AppData::new(engine);
    // 只勾「智能解压 + 添加到 7z」：规格里就必须只有这两项
    d.settings.ctx_extract = vec!["auto_extract".into()];
    d.settings.ctx_compress = vec!["add_7z".into()];
    let spec = build_menu_spec(&d, std::path::PathBuf::from("C:/hzip/hzip-gui.exe"));
    assert_eq!(
        spec.extract.iter().map(|e| e.key).collect::<Vec<_>>(),
        vec!["auto_extract"],
        "不得带出未勾选的解压项"
    );
    assert_eq!(
        spec.compress.iter().map(|e| e.key).collect::<Vec<_>>(),
        vec!["add_7z"],
        "不得带出未勾选的压缩项"
    );
    // 文案来自 i18n（此处只断言「非 key 本身」，具体语言由 locale 决定）
    assert!(!spec.extract[0].label.is_empty());
    assert_ne!(spec.extract[0].label, "ctxmenu.auto_extract");

    // 「用 hzip 打开」置顶（用户要求；它与解压动作同组但最常用，压在
    // 解压组后面每次都要点三下）。这条同时钉住**资源管理器里看到的顺序**：
    // spec.extract 的数组序 = shell-menu.json 的 items 序 = DLL 枚举序。
    d.settings.ctx_extract = CTX_EXTRACT_KEYS
        .iter()
        .map(|(k, _)| (*k).to_string())
        .collect();
    let spec = build_menu_spec(&d, std::path::PathBuf::from("C:/hzip/hzip-gui.exe"));
    assert_eq!(
        spec.extract[0].key,
        "open_hzip",
        "「用 hzip 打开」必须是右键子菜单第一项：{:?}",
        spec.extract.iter().map(|e| e.key).collect::<Vec<_>>()
    );

    // 顺序 = 白名单顺序（不是勾选顺序）：勾选顺序随机时菜单不得抖动
    d.settings.ctx_compress = vec!["add_archive".into(), "add_zip".into()];
    let spec2 = build_menu_spec(&d, std::path::PathBuf::from("C:/hzip/hzip-gui.exe"));
    assert_eq!(
        spec2.compress.iter().map(|e| e.key).collect::<Vec<_>>(),
        vec!["add_zip", "add_archive"],
        "输出顺序须为白名单顺序（add_zip 在 add_archive 之前）"
    );
    let _ = SettingId::CtxExtract; // 白名单常量与设置项同源，避免误改成别的项
}

/// Shell 扩展 DLL 路径（T-SEL 走查 #14）：`[ui].shell_ext_dll` 非空 →
/// 规格 `shell_dll = Some(该路径)`（注册指向它）；空/空白 → None
/// （平台层回落 exe 同目录默认查找）。
#[test]
fn menu_spec_shell_dll_follows_config() {
    let engine = std::sync::Arc::new(
        hzip_core::engine::Engine::new(hzip_core::engine::EngineConfig::default()).unwrap(),
    );
    let mut d = crate::app::state::AppData::new(engine);
    let exe = std::path::PathBuf::from("C:/hzip/hzip-gui.exe");

    // 默认未配置 → None
    assert_eq!(build_menu_spec(&d, exe.clone()).shell_dll, None);

    // 配置路径 → Some（原样传递，注册用该路径）
    d.settings.shell_ext_dll = "D:/tools/hzip-shell.dll".into();
    assert_eq!(
        build_menu_spec(&d, exe.clone()).shell_dll,
        Some(std::path::PathBuf::from("D:/tools/hzip-shell.dll"))
    );

    // 空白串 = 未配置（不把空格当路径注册）
    d.settings.shell_ext_dll = "   ".into();
    assert_eq!(build_menu_spec(&d, exe).shell_dll, None);
}

/// `archive_exts` = **能力集**，与「文件关联」勾选解耦。
///
/// 守的是一次实测缺陷：`assoc_formats = ["7z"]` 的机器上右键 `.zip`，
/// 菜单里只剩压缩项——因为 `archive_exts` 被错取自关联勾选，DLL 判定
/// 「.zip 不是档案」→ 解压组整体 `ECS_HIDDEN`。用户没勾关联不等于不能解压。
#[test]
fn menu_spec_archive_exts_follows_capability_not_association() {
    let engine = std::sync::Arc::new(
        hzip_core::engine::Engine::new(hzip_core::engine::EngineConfig::default()).unwrap(),
    );
    let mut d = crate::app::state::AppData::new(engine);
    // 关联只勾了 7z（最坏情形：用户从不装 hzip 为 zip 处理器）
    d.settings.assoc = vec!["7z".into()];
    let spec = build_menu_spec(&d, std::path::PathBuf::from("C:/hzip/hzip-gui.exe"));
    for must in ["zip", "7z", "tar"] {
        assert!(
            spec.archive_exts.iter().any(|e| e == must),
            "解压可见性不得受关联勾选影响，缺 {must}: {:?}",
            spec.archive_exts
        );
    }
    // 反向：不能反过来把关联勾选当成能力（否则又回到「只认 7z」）
    assert!(
        spec.archive_exts.len() > d.settings.assoc.len(),
        "能力集应大于关联勾选（实测 {} vs {}）",
        spec.archive_exts.len(),
        d.settings.assoc.len()
    );
    // **别名也算档案**（用户报的缺陷复现面）：`a.tgz` 曾因能力集缺 `tgz`
    // 而在右键菜单里没有解压项，`a.tar.gz` 只靠 `.gz` 侥幸命中。
    // DLL 侧 `is_archive_path` 即 `ends_with(".<ext>")`，故这里按同口径断言。
    for name in ["a.tgz", "a.tbz2", "a.txz", "a.tzst", "a.tar.gz", "a.GZ"] {
        let lower = name.to_ascii_lowercase();
        assert!(
            spec.archive_exts
                .iter()
                .any(|e| lower.ends_with(&format!(".{e}"))),
            "右键菜单应把 {name} 认作档案：{:?}",
            spec.archive_exts
        );
    }
}

// 面包屑派生：根 + 逐段累积路径（T-GUI 结构重组自 binding.rs 迁入）
#[test]
fn crumbs_root_and_segments() {
    let root = crumbs_from_current_dir("demo.zip", "");
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].label, "demo.zip");
    assert_eq!(root[0].seg, "");

    let deep = crumbs_from_current_dir("demo.zip", "docs/src");
    assert_eq!(deep.len(), 3);
    assert_eq!(deep[1].label, "docs");
    assert_eq!(deep[1].seg, "docs");
    assert_eq!(deep[2].label, "src");
    assert_eq!(deep[2].seg, "docs/src");
}

/// 关联候选扩展名 ↔ Linux MIME 表的一致性（**平台层与格式层的互锁**）。
///
/// `hzip-platform` 不依赖 `hzip-format`（红线 4），故两边各自维护一份扩展名
/// 清单：候选来自 `FORMAT_EXTS`（唯一来源），Linux 的 `.desktop`/`mimeapps.list`
/// 依赖 MIME 表。若候选里有 MIME 表漏掉的扩展名，Linux 上就是「能勾、点了不
/// 生效」的静默失效——本测试在编译期数据上把它钉死。
mod assoc_mime_consistency {
    use crate::ui::contracts::associable_extensions;

    #[test]
    fn every_association_candidate_has_a_linux_mime() {
        let engine = crate::viewmodel::testutil::engine();
        let missing: Vec<String> = associable_extensions(&engine)
            .into_iter()
            .filter(|e| !hzip_platform::assoc::mime_supports(e))
            .collect();
        assert!(
            missing.is_empty(),
            "这些候选扩展名在 hzip-platform 的 MIME 表里缺失（Linux 上会静默失效）: {missing:?}"
        );
    }
}
