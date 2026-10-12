use super::*;
use crate::context_menu::{MenuEntry, MenuSpec};
use std::path::PathBuf;

fn spec() -> MenuSpec {
    MenuSpec {
        exe_path: PathBuf::from("/opt/hzip/hzip-gui"),
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
        archive_exts: vec!["zip".into()],
        shell_dll: None,
    }
}

#[test]
fn extract_file_lists_archive_mimes_and_actions() {
    let s = render_file(&spec(), MenuScope::ArchiveFile, false).unwrap();
    assert!(s.contains("Type=Service"), "{s}");
    assert!(s.contains("MimeType=application/zip;"), "{s}");
    assert!(s.contains("Actions=extract_here;open_hzip;"), "{s}");
    assert!(s.contains("X-KDE-Submenu=hzip"), "{s}");
    assert!(
        !s.contains("ServiceTypes=KonqPopupMenu"),
        "新版不应写旧键: {s}"
    );
    assert!(s.contains("[Desktop Action extract_here]"), "{s}");
}

#[test]
fn compress_file_targets_all_files_and_directories() {
    let s = render_file(&spec(), MenuScope::Any, false).unwrap();
    assert!(
        s.contains("MimeType=inode/directory;application/octet-stream;"),
        "{s}"
    );
    assert!(s.contains("Actions=add_zip;"), "{s}");
    assert!(
        !s.contains("Actions=extract_here"),
        "解压项不得进任意文件菜单: {s}"
    );
}

#[test]
fn legacy_variant_adds_konq_popup_key() {
    let s = render_file(&spec(), MenuScope::ArchiveFile, true).unwrap();
    assert!(s.contains("ServiceTypes=KonqPopupMenu/Plugin"), "{s}");
}

#[test]
fn entry_scopes_are_respected_per_file() {
    // 解压类只出现在 ArchiveFile，压缩类只出现在 Any
    let with_both = spec();
    let ex = render_file(&with_both, MenuScope::ArchiveFile, false).unwrap();
    let cp = render_file(&with_both, MenuScope::Any, false).unwrap();
    for key in ["extract_here", "open_hzip"] {
        assert!(ex.contains(&format!("[Desktop Action {key}]")));
        assert!(!cp.contains(&format!("[Desktop Action {key}]")));
    }
    assert!(cp.contains("[Desktop Action add_zip]"));
    assert!(!ex.contains("[Desktop Action add_zip]"));
}

#[test]
fn exec_references_forwarding_script_with_percent_f() {
    let s = render_file(&spec(), MenuScope::ArchiveFile, false).unwrap();
    assert!(s.contains("Exec="), "{s}");
    assert!(s.contains("%F"), "需把选中项交给脚本: {s}");
    assert!(s.contains("hzip-extract_here"), "应指向该 key 的脚本: {s}");
}

#[test]
fn empty_scope_is_rejected_not_silently_written() {
    let empty = MenuSpec {
        exe_path: PathBuf::from("/x"),
        extract: vec![],
        compress: vec![MenuEntry {
            key: "add_zip",
            label: "z".into(),
        }],
        archive_exts: vec![],
        shell_dll: None,
    };
    let err = render_file(&empty, MenuScope::ArchiveFile, false).unwrap_err();
    assert_eq!(err.kind, MenuErrorKind::Spec);
}

#[test]
fn exec_quoting_handles_spaces_and_quotes() {
    assert_eq!(desktop_exec_quote("/opt/hzip-gui"), "/opt/hzip-gui");
    assert_eq!(desktop_exec_quote("/opt/a b/x"), "\"/opt/a b/x\"");
    assert_eq!(desktop_exec_quote("/opt/a\"b"), "\"/opt/a\\\"b\"");
}

#[test]
fn label_escaping_keeps_desktop_syntax_valid() {
    assert_eq!(desktop_escape("a\\b\nc"), "a\\\\b\\nc");
}
