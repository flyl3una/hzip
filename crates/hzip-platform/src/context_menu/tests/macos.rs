use super::*;
use crate::context_menu::{MenuEntry, MenuScope, MenuSpec};

fn spec() -> MenuSpec {
    MenuSpec {
        exe_path: PathBuf::from("/Applications/hzip.app/Contents/MacOS/hzip-gui"),
        extract: vec![MenuEntry {
            key: "extract_here",
            label: "解压到当前文件夹".into(),
        }],
        compress: vec![MenuEntry {
            key: "add_zip",
            label: "压缩为 zip".into(),
        }],
        archive_exts: vec!["zip".into()],
        shell_dll: None,
    }
}

#[test]
fn app_bundle_is_found_by_walking_up_from_exe() {
    let exe = Path::new("/Applications/hzip.app/Contents/MacOS/hzip-gui");
    assert_eq!(
        app_bundle_path(exe),
        Some(PathBuf::from("/Applications/hzip.app"))
    );
    // 不在 bundle 内（开发期 cargo run）→ None
    assert_eq!(
        app_bundle_path(Path::new("/Users/x/target/debug/hzip-gui")),
        None
    );
}

#[test]
fn appex_path_is_bundle_relative() {
    let b = Path::new("/Applications/hzip.app");
    assert_eq!(
        appex_path(b),
        PathBuf::from("/Applications/hzip.app/Contents/PlugIns/hzipFinderSync.appex")
    );
}

#[test]
fn plugin_state_parsing_covers_all_three_cases() {
    assert_eq!(
        parse_plugin_state("+    com.hzip.mac.findersyncextension(1.0)"),
        PluginState::Enabled
    );
    assert_eq!(
        parse_plugin_state("-    com.hzip.mac.findersyncextension(1.0)"),
        PluginState::Disabled
    );
    assert_eq!(parse_plugin_state(""), PluginState::Unknown);
    assert_eq!(parse_plugin_state("no match"), PluginState::Unknown);
    // 多行时取首个有效标记行
    assert_eq!(
        parse_plugin_state("-  a\n+  com.hzip.mac.findersyncextension"),
        PluginState::Disabled
    );
}

#[test]
fn plugin_state_ignores_bare_markers() {
    // 仅一个 "+" 而无内容不应误判为启用
    assert_eq!(parse_plugin_state("+"), PluginState::Unknown);
    assert_eq!(parse_plugin_state("-\n"), PluginState::Unknown);
}

#[test]
fn monitored_folders_default_is_absolute() {
    let f = default_monitored_folders();
    assert!(Path::new(&f).is_absolute(), "监听目录须为绝对路径: {f}");
}

#[test]
fn service_identifiers_are_non_empty_and_unique() {
    let ids = service_identifiers();
    assert!(!ids.is_empty());
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "服务名不得重复");
}

#[test]
fn spec_entries_carry_scopes_for_appex_side_filtering() {
    // appex 需要知道哪些条目只对档案出现
    let def = crate::context_menu::entry_for("extract_here").unwrap();
    assert!(def.scopes.contains(&MenuScope::ArchiveFile));
    let def = crate::context_menu::entry_for("add_zip").unwrap();
    assert!(def.scopes.contains(&MenuScope::Any));
    // spec 本身可解析（写偏好前的前置校验）
    assert_eq!(spec().resolved().count(), 2);
}
