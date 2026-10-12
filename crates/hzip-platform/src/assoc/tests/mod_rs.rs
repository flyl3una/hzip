use super::*;

/// 命令串引号嵌套断言（全平台可跑）。
///
/// 右键菜单条目的 argv 不在此处——它们由 [`crate::context_menu::VERBS`] 定义
/// （DLL 用 `CreateProcessW`，Linux 用转发脚本），本模块只剩 `shell\open`
/// 一条命令串。
#[test]
fn command_quote_nesting() {
    let exe = r"C:\Program Files\hzip\hzip-gui.exe";
    assert_eq!(
        format_verb_command(exe, &["--open", "%1"]),
        "\"C:\\Program Files\\hzip\\hzip-gui.exe\" --open \"%1\""
    );
    assert_eq!(
        format_verb_command(exe, &["--extract-here", "--delete", "%1"]),
        "\"C:\\Program Files\\hzip\\hzip-gui.exe\" --extract-here --delete \"%1\""
    );
}

/// 状态三态：ours / pinned 的组合语义（界面按钮与文案据此分支）。
///
/// 守的是「界面对用户说真话」：把被 `UserChoice` 占着的格式报成「已生效」，
/// 用户就会以为双击归 hzip，实际打开的是别人的程序。
#[test]
fn state_tri_state_semantics() {
    // ① 归我们（不管 pinned 与否：UserChoice 指向我们 = 用户自己选过我们）
    let st = ExtAssocState {
        effective: Some(PROG_ID.to_string()),
        effective_name: Some("hzip 压缩档案".to_string()),
        ours: true,
        pinned: true,
    };
    assert!(st.is_effective() && !st.blocked_by_other());

    // ② 别人占着且改不动（Windows UserChoice=Bandizip）
    let st = ExtAssocState {
        effective: Some("Bandizip.zip".to_string()),
        effective_name: Some("Bandizip".to_string()),
        ours: false,
        pinned: true,
    };
    assert!(!st.is_effective() && st.blocked_by_other());
    assert_eq!(st.owner_name(), Some("Bandizip"));

    // ③ 别人是默认但程序改得动（Linux/macOS，或 Windows 无 UserChoice）
    let st = ExtAssocState {
        effective: Some("org.gnome.FileRoller.desktop".to_string()),
        effective_name: None,
        ours: false,
        pinned: false,
    };
    assert!(!st.is_effective() && !st.blocked_by_other());
    assert_eq!(st.owner_name(), Some("org.gnome.FileRoller.desktop"));

    // ④ 没有默认程序（系统会弹「打开方式」）
    let st = ExtAssocState::default();
    assert!(!st.is_effective() && !st.blocked_by_other());
    assert_eq!(st.owner_name(), None);
}
