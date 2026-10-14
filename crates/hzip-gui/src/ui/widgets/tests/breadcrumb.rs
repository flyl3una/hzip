use super::*;

use gpui_kit::Window;

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

/// 分隔符 `/` 与段名**同字号、同行高** ⇒ 与段名/整行同中心线（用户口径 2026-10-14）。
///
/// # 为什么这样断言，而不是直接比「字形墨迹」
///
/// `debug_bounds` 给的是**盒**。图标时代真正的问题是「盒居中，而 chevron 的 SVG 墨迹
/// 在盒内偏上」——盒几何看不出这点偏差，那正是它当初溜过测试的原因。换成文本后两侧
/// 字号与行高逐项相同 ⇒ 共用同一基线，于是「盒中心相等」等价于「墨迹对齐」。故这里把
/// 实现契约钉成几何：
///   - 分隔符高度 = `text_sm` 字号 × `line_height(1.25)`（与 kit `Button` `Size::Small`
///     同一套，见 gpui-component `sizing.rs::button_text_size` / `button.rs`）；
///   - 每个分隔符的中心线 = 整行中心线（`items_center` 下的段名按钮也在这条线上）。
/// 谁把分隔符换回图标、或改了任一侧字号/行高，第一条断言立刻失败。
/// 观感层面的最终验收仍是肉眼/截图（墨迹本身无法用盒几何表达）。
#[gpui_kit::test]
fn separator_shares_segment_line(cx: &mut gpui_kit::TestAppContext) {
    struct Host {
        shell: WeakEntity<crate::ui::shell::AppShell>,
    }

    impl gpui_kit::Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            // 三段（demo.zip / docs / src）⇒ 两个分隔符
            let crumbs = crate::ui::contracts::crumbs_from_current_dir("demo.zip", "docs/src");
            breadcrumb(cx, &crumbs, &self.shell)
        }
    }

    let (_host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = cx.new(|cx| crate::ui::shell::AppShell::new(cx, ShellHandlers::default()));
        Host {
            shell: shell.downgrade(),
        }
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let center_y = |b: gpui_kit::Bounds<gpui_kit::Pixels>| {
        f32::from(b.origin.y) + f32::from(b.size.height) / 2.0
    };
    let row = vtcx.debug_bounds("crumb-row").expect("面包屑行锚点");
    let rem = f32::from(vtcx.update(|window, _| window.rem_size()));

    // 两条断言：几何（同字号+同行高）与位置（同中心线）
    let expected_h = rem * 0.875 * 1.25; // text_sm 字号（0.875rem）× 行高 1.25
    // 锚点是静态串（`debug_bounds` 要 `&'static str`）：三段 ⇒ 恰好 sep-0 / sep-1
    for anchor in ["crumb-sep-0", "crumb-sep-1"] {
        let sep = vtcx
            .debug_bounds(anchor)
            .unwrap_or_else(|| panic!("分隔符锚点 {anchor} 缺失（应画在非末段之后）"));
        let h = f32::from(sep.size.height);
        assert!(
            (h - expected_h).abs() <= 0.5,
            "{anchor} 高度应 = text_sm 字号 × 1.25 = {expected_h}（与段名同一行盒），实得 {h}"
        );
        assert!(
            (center_y(sep) - center_y(row)).abs() <= 0.5,
            "{anchor} 中心应与整行中心同高：sep={}, row={}",
            center_y(sep),
            center_y(row)
        );
    }
    // 末段之后不得有分隔符（根目录单段时更是一个都没有）
    assert!(
        vtcx.debug_bounds("crumb-sep-2").is_none(),
        "末段之后不得再画分隔符"
    );
}
