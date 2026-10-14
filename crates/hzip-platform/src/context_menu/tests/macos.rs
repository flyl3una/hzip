use super::*;
use crate::context_menu::{ExtensionIntent, MenuEntry, MenuScope, MenuSpec};

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

/// 标识必须与发行包（`build/package/metadata.toml::bundle_id`）和 appex 的
/// Info.plist 一致——历史上这里写死 `com.hzip.mac`/`com.hzip.mac.findersyncextension`，
/// 与真实 bundle id 不符，注册永远匹配不上（实测缺陷）。此测试锁死这条契约。
#[test]
fn identifiers_match_shipped_bundle() {
    assert_eq!(APP_ID, "io.github.hzip.hzip");
    assert_eq!(EXTENSION_ID, "io.github.hzip.hzip.findersync");
    assert_eq!(APP_GROUP, "group.io.github.hzip.hzip");
    // App Group 名必须是 `group.` 前缀 + 宿主 bundle id（macOS 约定）。
    assert_eq!(APP_GROUP, format!("group.{APP_ID}"));
}

#[test]
fn plugin_state_parsing_covers_all_three_cases() {
    assert_eq!(
        parse_plugin_state("+    io.github.hzip.hzip.findersync(1.0)"),
        PluginState::Enabled
    );
    assert_eq!(
        parse_plugin_state("-    io.github.hzip.hzip.findersync(1.0)"),
        PluginState::Disabled
    );
    assert_eq!(parse_plugin_state(""), PluginState::Unknown);
    assert_eq!(parse_plugin_state("no match"), PluginState::Unknown);
    // 多行时取首个有效标记行
    assert_eq!(
        parse_plugin_state("-  a\n+  io.github.hzip.hzip.findersync"),
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

// ---- 扩展意图队列（appex/Services → 宿主）----

#[test]
fn parse_intents_reads_wellformed_lines() {
    let text = "{\"key\":\"extract_here\",\"paths\":[\"/a.zip\"]}\n\
                {\"key\":\"add_zip\",\"paths\":[\"/b\",\"/c\"]}\n";
    let intents = parse_intents(text);
    assert_eq!(
        intents,
        vec![
            ExtensionIntent {
                key: "extract_here".into(),
                paths: vec!["/a.zip".into()],
            },
            ExtensionIntent {
                key: "add_zip".into(),
                paths: vec!["/b".into(), "/c".into()],
            },
        ]
    );
}

#[test]
fn parse_intents_drops_bad_lines_not_whole_batch() {
    // 跨进程追加文件：一条坏行不该让其余意图陪葬。
    let text = "{\"key\":\"extract_here\",\"paths\":[\"/a.zip\"]}\n\
                not-json\n\
                {\"key\":\"add_zip\",\"paths\":[\"/b\"]}\n";
    let intents = parse_intents(text);
    assert_eq!(intents.len(), 2, "坏行应被丢弃、好行保留");
    assert_eq!(intents[0].key, "extract_here");
    assert_eq!(intents[1].key, "add_zip");
}

#[test]
fn parse_intents_ignores_blank_lines_and_empty_input() {
    assert!(parse_intents("").is_empty());
    assert!(parse_intents("\n\n").is_empty());
}

/// 消费语义：读取后清空内容**但保留文件**（删文件会破坏 appex 侧的文件句柄）。
#[test]
fn take_intents_truncates_but_keeps_file() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let q = dir.path().join("intents.jsonl");
    std::fs::write(
        &q,
        "{\"key\":\"extract_here\",\"paths\":[\"/a.zip\"]}\n",
    )
    .unwrap();

    let first = take_intents_at(&q);
    assert_eq!(first.len(), 1, "首次消费应读到 1 条");
    assert!(q.exists(), "文件须保留（不得 remove_file）");
    assert_eq!(std::fs::read_to_string(&q).unwrap(), "", "内容须清空");

    // 幂等：再消费得到空
    assert!(take_intents_at(&q).is_empty(), "二次消费应为空");

    // 追加一条后能再读到（文件句柄追加语义成立）
    append_intent_at(
        &q,
        &ExtensionIntent {
            key: "add_zip".into(),
            paths: vec!["/z".into()],
        },
    )
    .unwrap();
    let third = take_intents_at(&q);
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].key, "add_zip");
}

#[test]
fn take_intents_on_missing_file_is_empty() {
    let dir = tempfile::tempdir().expect("tmpdir");
    assert!(take_intents_at(&dir.path().join("nope.jsonl")).is_empty());
}

#[test]
fn group_container_dir_is_under_home_library() {
    let d = group_container_dir();
    assert!(d.ends_with(APP_GROUP), "{d:?}");
    assert!(d.is_absolute());
    assert!(
        d.parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == "Group Containers"),
        "App Group 容器须位于 ~/Library/Group Containers 下: {d:?}"
    );
}
