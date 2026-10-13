use super::*;
use crate::context_menu::{MenuEntry, MenuSpec};
use std::path::PathBuf;

fn spec() -> MenuSpec {
    MenuSpec {
        exe_path: PathBuf::from("/opt/hzip/hzip-gui"),
        extract: vec![MenuEntry {
            key: "extract_here",
            label: "解压到当前文件夹".into(),
        }],
        compress: vec![MenuEntry {
            key: "add_zip",
            label: "添加到 zip".into(),
        }],
        archive_exts: vec!["zip".into(), "7z".into()],
        shell_dll: None,
    }
}

const USER_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<actions>
  <action>
<icon>utilities-terminal</icon>
<name>Open Terminal Here</name>
<unique-id>user-terminal-1</unique-id>
<command>exo-open --launch TerminalEmulator %f</command>
<directories/>
  </action>
</actions>
"#;

#[test]
fn merge_preserves_user_actions_verbatim() {
    let merged = merge(USER_TEMPLATE, &spec()).unwrap();
    assert!(
        merged.contains("user-terminal-1"),
        "用户动作必须保留: {merged}"
    );
    assert!(merged.contains("exo-open --launch TerminalEmulator %f"));
    assert!(merged.contains("<unique-id>hzip-extract_here</unique-id>"));
    assert!(merged.contains("<unique-id>hzip-add_zip</unique-id>"));
    // 用户块**逐字**保留（含缩进原貌）。比对的就是模板里的原文——`merge` 只做
    // 「在 </actions> 前插入」，从不重排用户缩进（thunar.rs:104 的注释即此契约，
    // 隔壁 `install_then_uninstall_roundtrips_to_original_user_content` 也依赖它）。
    //
    // 这里原先写的是 `"  <action>\n    <icon>utilities-terminal</icon>"`——那要求
    // merge 把用户块**重新缩进**成 4 空格，与模板（`<icon>` 在列 0）和上面那条契约
    // 都对不上。本模块是 `#[cfg(target_os = "linux")]`，开发机（Windows）根本不编译它，
    // 于是这条错断言一路带到 Linux CI 才红（2026-10-13 在 WSL 上复现：98 passed / 1 failed）。
    let user_block = USER_TEMPLATE
        .trim_start_matches("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n")
        .trim_end_matches("</actions>\n");
    assert!(
        merged.contains(user_block),
        "用户块必须逐字保留（含缩进原貌）\n期望包含:\n{user_block}\n实际:\n{merged}"
    );
}

#[test]
fn merge_is_idempotent_no_duplicate_actions() {
    let once = merge(USER_TEMPLATE, &spec()).unwrap();
    let twice = merge(&once, &spec()).unwrap();
    assert_eq!(once, twice, "重复 install 不得累积条目");
    assert_eq!(
        twice
            .matches("<unique-id>hzip-extract_here</unique-id>")
            .count(),
        1
    );
}

#[test]
fn install_then_uninstall_roundtrips_to_original_user_content() {
    let merged = merge(USER_TEMPLATE, &spec()).unwrap();
    let stripped = strip_ours(&merged);
    assert!(!stripped.contains("hzip-"), "卸载须清干净: {stripped}");
    assert!(stripped.contains("user-terminal-1"));
    assert!(stripped.contains("</actions>"));
}

#[test]
fn merge_into_empty_file_creates_valid_skeleton() {
    let out = merge("", &spec()).unwrap();
    assert!(out.starts_with("<?xml"), "{out}");
    assert!(out.contains("<actions>") && out.contains("</actions>"));
    assert!(out.contains("<unique-id>hzip-extract_here</unique-id>"));
}

#[test]
fn unknown_file_is_rejected_rather_than_clobbered() {
    let err = merge("not an xml file at all", &spec()).unwrap_err();
    assert_eq!(err.kind, MenuErrorKind::Spec);
}

#[test]
fn archive_actions_filter_by_pattern_and_compress_by_directory() {
    let xml = render_actions(&spec()).unwrap();
    // 解压项块：档案 glob + other-files（不写 directories）
    let ex_block = block_of(&xml, "hzip-extract_here");
    assert!(
        ex_block.contains("<patterns>*.zip;*.7z</patterns>"),
        "{ex_block}"
    );
    assert!(ex_block.contains("<other-files/>"), "{ex_block}");
    assert!(
        !ex_block.contains("<directories/>"),
        "解压项不应只对目录出现: {ex_block}"
    );
    // 压缩项块：目录 + 任意文件（不写 patterns）
    let cp_block = block_of(&xml, "hzip-add_zip");
    assert!(cp_block.contains("<directories/>"), "{cp_block}");
    assert!(cp_block.contains("<other-files/>"), "{cp_block}");
    assert!(
        !cp_block.contains("<patterns>"),
        "压缩项不按扩展名过滤: {cp_block}"
    );
}

/// 取某 unique-id 所在的 `<action>` 块原文。
fn block_of(xml: &str, id: &str) -> String {
    let needle = format!("<unique-id>{id}</unique-id>");
    let at = xml.find(&needle).expect("须存在该动作");
    let start = xml[..at].rfind("<action>").expect("须有动作起始");
    let end = xml[at..].find("</action>").expect("须有动作结束") + at + "</action>".len();
    xml[start..end].to_string()
}

#[test]
fn xml_special_chars_in_labels_are_escaped() {
    let s = MenuSpec {
        exe_path: PathBuf::from("/x"),
        extract: vec![],
        compress: vec![MenuEntry {
            key: "add_zip",
            label: "a & b <c> \"d\"".into(),
        }],
        archive_exts: vec![],
        shell_dll: None,
    };
    let xml = render_actions(&s).unwrap();
    assert!(xml.contains("a &amp; b &lt;c&gt; &quot;d&quot;"), "{xml}");
}

#[test]
fn command_quoting_handles_spaces_in_path() {
    assert_eq!(shell_quote("/opt/hzip-gui"), "/opt/hzip-gui");
    assert_eq!(shell_quote("/opt/a b"), "'/opt/a b'");
    assert_eq!(shell_quote("/opt/a'b"), r"'/opt/a'\''b'");
}

#[test]
fn patterns_fall_back_to_wildcard_when_no_archives_associated() {
    let s = MenuSpec {
        exe_path: PathBuf::from("/x"),
        extract: vec![MenuEntry {
            key: "extract_here",
            label: "e".into(),
        }],
        compress: vec![],
        archive_exts: vec![],
        shell_dll: None,
    };
    assert!(
        render_actions(&s)
            .unwrap()
            .contains("<patterns>*</patterns>")
    );
}
