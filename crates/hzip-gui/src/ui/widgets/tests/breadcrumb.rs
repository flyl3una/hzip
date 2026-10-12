use super::*;

/// 中段省略：长度受控、头尾都留、字符不丢（去掉 `…` 后是原串的子序列头+尾）。
#[test]
fn middle_ellipsis_keeps_head_and_tail() {
    let long = "initramfs-tools_0.142ubuntu25_all_amd64_very_long_name.cpio.gz";
    let out = middle_ellipsis(long, 24);
    assert_eq!(out.chars().count(), 24, "折叠后总长 = 上限（含 …）");
    assert!(out.contains('…'), "必须出现省略号");
    assert!(out.starts_with("initramfs"), "头部保留（区分度）：{out}");
    assert!(out.ends_with(".cpio.gz"), "尾部保留（扩展名）：{out}");
    // 头尾各自是原串的前缀/后缀
    let (head, tail) = out.split_once('…').expect("含省略号");
    assert!(long.starts_with(head));
    assert!(long.ends_with(tail));
}

/// 未超限的段**逐字不变**（不做任何改动——短名不显示省略号）。
#[test]
fn middle_ellipsis_leaves_short_names_intact() {
    for s in ["", "a", "src", "huge-init.cpio.gz", "中文目录名"] {
        assert_eq!(middle_ellipsis(s, CRUMB_MAX_CHARS), s);
    }
    // 恰好等于上限 → 原样
    let exact = "x".repeat(CRUMB_MAX_CHARS);
    assert_eq!(middle_ellipsis(&exact, CRUMB_MAX_CHARS), exact);
    // 中文按**字符**计数（不按字节）：不会因多字节被提前折叠
    // （24 个汉字 = 72 字节 —— 按字节折叠会把它切坏）
    let zh: String = "中文目录名字".repeat(4);
    assert_eq!(zh.chars().count(), CRUMB_MAX_CHARS);
    assert_eq!(zh.len(), CRUMB_MAX_CHARS * 3, "按字节是 3 倍（前置条件）");
    assert_eq!(middle_ellipsis(&zh, CRUMB_MAX_CHARS), zh);
}

/// 退化边界：`max < 2` 不做折叠（不 panic、不产生空串）。
#[test]
fn middle_ellipsis_degenerate_max() {
    assert_eq!(middle_ellipsis("abcdef", 1), "abcdef");
    assert_eq!(middle_ellipsis("abcdef", 0), "abcdef");
    // 极小上限（2、3）仍保证长度与省略号
    for max in [2usize, 3, 4] {
        let out = middle_ellipsis("abcdefghij", max);
        assert_eq!(out.chars().count(), max);
        assert!(out.contains('…'));
    }
}
