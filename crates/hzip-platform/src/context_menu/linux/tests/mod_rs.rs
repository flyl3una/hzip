use super::*;

#[test]
fn detects_the_three_supported_desktops() {
    assert_eq!(detect_from("KDE", ""), Some(Desktop::Kde));
    assert_eq!(detect_from("", "plasma"), Some(Desktop::Kde));
    assert_eq!(detect_from("XFCE", ""), Some(Desktop::Xfce));
    assert_eq!(detect_from("", "xfce"), Some(Desktop::Xfce));
    assert_eq!(detect_from("GNOME", ""), Some(Desktop::Gnome));
    assert_eq!(detect_from("ubuntu:GNOME", ""), Some(Desktop::Gnome));
}

#[test]
fn detection_is_case_insensitive() {
    assert_eq!(detect_from("kde", ""), Some(Desktop::Kde));
    assert_eq!(detect_from("Gnome", ""), Some(Desktop::Gnome));
    assert_eq!(detect_from("", "XFCE"), Some(Desktop::Xfce));
}

#[test]
fn kde_wins_when_multiple_markers_present() {
    // 某些发行版把 KDE 与 GNOME 词都塞进变量；KDE 标识更独特，优先
    assert_eq!(detect_from("KDE:GNOME", ""), Some(Desktop::Kde));
}

#[test]
fn unknown_desktop_is_not_guessed() {
    assert_eq!(detect_from("sway", "sway"), None);
    assert_eq!(detect_from("", ""), None);
}

#[test]
fn unsupported_reason_mentions_supported_set() {
    let r = unsupported_reason(Some("sway"));
    assert!(r.contains("GNOME"), "{r}");
    assert!(r.contains("KDE"), "{r}");
    assert!(r.contains("XFCE"), "{r}");
    // 环境变量未设置（`None`）时展示提示语，而不是空的 `XDG_CURRENT_DESKTOP=`
    assert!(unsupported_reason(None).contains("（未设置）"));
}
