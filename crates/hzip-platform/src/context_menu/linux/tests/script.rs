use super::*;

#[test]
fn script_quotes_executable_and_literals() {
    let s = render_script(
        Path::new("/opt/hzip hzip/hzip-gui"),
        "extract_here",
        "解压到当前文件夹",
        &["--extract-here", "%SEL"],
    );
    // 含空格的 exe 与汉字标签都必须被单引号包裹
    assert!(s.contains("exe='/opt/hzip hzip/hzip-gui'"), "{s}");
    assert!(s.contains("# hzip context menu: 解压到当前文件夹 (extract_here)"));
    assert!(s.contains("for _path in \"$@\"; do"));
    assert!(s.contains("\"$exe\" '--extract-here' \"$_path\""));
    assert!(s.contains("#!/bin/sh"));
}

#[test]
fn label_with_quote_stays_inside_comment_and_does_not_break_script() {
    // 标签只出现在注释行，其中的单引号不参与 shell 解析——只要不换行即可安全
    let s = render_script(
        Path::new("/x"),
        "add_zip",
        "it's zip",
        &["--compress", "%SELS"],
    );
    let comment = s
        .lines()
        .find(|l| l.starts_with("# hzip context menu:"))
        .expect("须有标签注释行");
    assert!(comment.contains("it's zip"), "{s}");
    // 引号不得逃出注释：命令体里不应出现未配对的裸标签
    assert!(s.contains("exec \"$exe\" '--compress' \"$@\""), "{s}");
}

#[test]
fn one_shot_mode_passes_all_paths_and_execs() {
    let s = render_script(
        Path::new("/usr/bin/hzip-gui"),
        "add_7z",
        "添加到 7z",
        &["--compress", "%SELS", "--format", "7z"],
    );
    assert!(
        s.contains("exec \"$exe\" '--compress' \"$@\" '--format' '7z'"),
        "{s}"
    );
    assert!(!s.contains("for _path"), "一次性模式不得逐文件循环");
}

#[test]
fn per_file_mode_never_uses_positional_passthrough() {
    let s = render_script(
        Path::new("/e"),
        "extract_dir_del",
        "del",
        &["--extract-dir", "--delete", "%SEL"],
    );
    assert!(s.contains("'--extract-dir' '--delete' \"$_path\""), "{s}");
}

#[test]
fn nautilus_env_fallback_present_in_all_modes() {
    for argv in [
        &["--extract-here", "%SEL"][..],
        &["--compress", "%SELS"][..],
    ] {
        let s = render_script(Path::new("/e"), "k", "l", argv);
        assert!(s.contains("$NAUTILUS_SCRIPT_SELECTED_FILE_PATHS"), "{s}");
        assert!(s.contains("if [ \"$#\" -eq 0 ]; then"), "{s}");
    }
}
