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
            label: "添加到\"压缩文件名.zip\"".into(),
        }],
        archive_exts: vec!["zip".into(), "7z".into()],
        shell_dll: None,
    }
}

#[test]
fn extension_embeds_spec_as_escaped_string_literal() {
    let py = render_extension(&spec());
    assert!(py.starts_with("# hzip context menu (generated"), "{py}");
    assert!(py.contains("import json"), "{py}");
    assert!(py.contains("class HzipMenuProvider"), "{py}");
    // 标签里的双引号必须转义，不能截断 Python 字面量：
    // SPEC 的字面量必须单行闭合，且内部引号全部转义。
    let line = py
        .lines()
        .find(|l| l.starts_with("SPEC = json.loads("))
        .expect("须有 SPEC 赋值行");
    let literal = line
        .strip_prefix("SPEC = json.loads(")
        .expect("须有 json.loads 前缀");
    let chars: Vec<char> = literal.chars().collect();
    assert_eq!(chars.first(), Some(&'"'), "字面量须以引号开头: {line}");
    assert_eq!(
        chars.get(chars.len() - 2),
        Some(&'"'),
        "字面量须在同行以引号闭合: {line}"
    );
    assert_eq!(chars.last(), Some(&')'), "json.loads 须同行闭合: {line}");
    assert!(literal.contains("\\\""), "内部双引号须转义为 \\\" : {line}");
    // 标签里的双引号经「JSON 转义（\"）→ Python 字面量转义（\\\"）」两层，
    // 故最终文本为 \\\" ——两层都不得漏
    assert!(
        py.contains(r#"\\\"压缩文件名.zip\\\""#),
        "JSON 与 Python 两层转义都须到位"
    );
    assert!(py.contains("Nautilus.MenuProvider"), "{py}");
    // 目标 API 是新签名（Nautilus >= 43）
    assert!(py.contains("def get_file_items(self, files):"), "{py}");
    assert!(
        py.contains("def get_background_items(self, current_folder):"),
        "{py}"
    );
}

#[test]
fn extension_hides_extract_items_for_non_archive_targets() {
    let py = render_extension(&spec());
    assert!(
        py.contains("SPEC[\"archive_exts\"]"),
        "需按扩展名过滤解压项: {py}"
    );
    assert!(py.contains("item[\"key\"].startswith(\"extract\")"), "{py}");
}

#[test]
fn python_literal_survives_all_quote_kinds() {
    let lit = py_string_literal(r#"{"a":"it's \"x\" \ end"}"#);
    assert!(lit.starts_with('"') && lit.ends_with('"'));
    assert!(!lit.contains('\n'));
    assert_eq!(lit.matches(r#"\""#).count() % 2, 0, "转义须成对: {lit}");
}

#[test]
fn sanitize_replaces_path_separators_and_quotes() {
    assert_eq!(sanitize("添加到\"a/b\".zip"), "添加到_a_b_.zip");
    assert_eq!(sanitize("普通文案"), "普通文案");
}
