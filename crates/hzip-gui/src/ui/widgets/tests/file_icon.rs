//! 文件类型图标测试（T-ICON 波2）。
//!
//! 覆盖四类：① 纯函数（键归一 / 种类顺序）；② 缓存三态机（含负缓存「只探测
//! 一次」与开关关闭「零平台调用 + 清缓存」）；③ `type_name` 语言规则；④
//! headless 渲染烟测——首帧只有回退锚点，排空 + 重绘后换成系统位图锚点
//! （= 用户可见的「先回退后替换」）。
//!
//! 「平台调用次数」一律由计数 stub 断言：真实 `lookup` 会问 shell（慢且随
//! 机器关联变化），测试绝不能走它。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

use gpui_kit::{AppContext as _, IntoElement, Render, TestAppContext, Window};
use hzip_platform::file_icon::{IconKind, IconRequest, SystemIcon};

use super::test_hooks::{drain_pending_for_test, init_with_source, state_of_for_test};
use super::*;

/// 2×2 预乘 BGRA 位图（长度 = 2·2·4 = 16，正好通过 [`system_icon_entry`] 校验）。
fn bitmap() -> SystemIcon {
    SystemIcon {
        width: 2,
        height: 2,
        premultiplied_bgra: vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ],
        type_name: Some("测试类型".into()),
    }
}

/// 计数 stub source：`hits` 记平台调用次数；`ret = None` 模拟「拿不到」。
fn counting_source(hits: Arc<AtomicUsize>, ret: Option<SystemIcon>) -> IconSource {
    let ret = ret.map(Arc::new);
    Arc::new(move |_req: &IconRequest<'_>| {
        hits.fetch_add(1, Ordering::SeqCst);
        ret.as_ref().map(|icon| (**icon).clone())
    })
}

/// 无窗口装配：主题 Global（回退元素取色）+ 可注入 source 的缓存。
fn setup(cx: &mut App, source: IconSource) {
    gpui_kit::component::init(cx);
    crate::ui::theme::init(cx);
    init_with_source(cx, None, source);
}

/// 系统语言 → UI locale 的**规格**映射（测试侧独立实现，不复用被测代码；
/// 两者不一致时本用例才可能失败——那正是要发现的缺陷）。
fn expected_mapped_locale() -> &'static str {
    let is_english = hzip_platform::lang::system_language().is_some_and(|tag| {
        tag.split(['-', '_'])
            .next()
            .is_some_and(|main| main.eq_ignore_ascii_case("en"))
    });
    if is_english { "en" } else { "zh-CN" }
}

/// 规格里「映射结果」的反对侧（用于钉不一致时必须 `None`）。
static OPPOSITE_LOCALE: LazyLock<&'static str> = LazyLock::new(|| {
    if expected_mapped_locale() == "en" {
        "zh-CN"
    } else {
        "en"
    }
});

/// 键归一：取最后一段扩展名、小写；无扩展名/尾点/隐藏文件 → `None`；
/// `Container` 归一为 `File`，`Symlink` 保持独立。
#[test]
fn icon_key_normalizes_extension_and_kind() {
    let ext = |name: &str| icon_key(name, IconKind::File).1;
    assert_eq!(ext("a.TAR.GZ").as_deref(), Some("gz"), "取最后一段并小写");
    assert_eq!(ext("a.ZIP").as_deref(), Some("zip"));
    assert_eq!(ext("noext"), None, "无扩展名 → None（系统默认文件图标）");
    assert_eq!(ext("a."), None, "尾点不算扩展名");
    assert_eq!(ext(".bashrc"), None, "隐藏文件不算扩展名");
    assert_eq!(
        icon_key("dir", IconKind::Dir),
        (IconKind::Dir, None),
        "目录无扩展名"
    );
    assert_eq!(
        icon_key("inner.7z", IconKind::Container),
        (IconKind::File, Some("7z".into())),
        "容器归一为 File：同扩展名共用一次探测"
    );
    assert_eq!(
        icon_key("link.zip", IconKind::Symlink),
        (IconKind::Symlink, Some("zip".into())),
        "软链接独立（Windows 加箭头覆盖层）"
    );
}

/// 种类判定顺序：软链接 > 容器 > 目录 > 文件。
#[test]
fn kind_of_prefers_symlink_then_container_then_dir() {
    assert_eq!(kind_of(false, false, false), IconKind::File);
    assert_eq!(kind_of(true, false, false), IconKind::Dir);
    assert_eq!(kind_of(false, true, false), IconKind::Container);
    assert_eq!(kind_of(true, false, true), IconKind::Symlink, "软链接目录");
    assert_eq!(
        kind_of(true, true, true),
        IconKind::Symlink,
        "软链接优先于容器"
    );
}

/// 槽位边长：`Inline` = 文字**字形盒**（`GLYPH_BOX_EM` × 基准字号，随 `[ui].font_size`
/// 缩放）；`Grid` = `ICON_CELL_ICON`（constant.rs 单一来源）。
///
/// 用户实报 2026-10-17（4K/Win11）：「图标比较小，并且比字体高」——此前 Inline 写死
/// 16px（= 基准字号 1.0em，而行框 ≈1.74em）。本用例钉住「1.35em 且随字号走」。
#[gpui_kit::test]
fn slot_px_follows_the_font_size(cx: &mut TestAppContext) {
    cx.update(|cx: &mut App| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let font = f32::from(cx.theme().font_size);
        assert_eq!(
            slot_px(Slot::Inline, cx),
            px((font * crate::ui::constant::GLYPH_BOX_EM).round()),
            "Inline = 字形盒（1.35em）"
        );
        assert_eq!(
            slot_px(Slot::Grid, cx),
            ICON_CELL_ICON,
            "Grid 与字号无关（单元格常量）"
        );
        // 出厂字号 16 ⇒ 22px（16 定档时代是 16px）：比字形墨迹略大、仍装得进行框
        assert_eq!(slot_px(Slot::Inline, cx), px(22.), "出厂字号下应为 22px");
    });
}

/// 状态机：`absent` → 渲染登记 `wanted` → 排空 → `ready`；
/// 同键只探测一次，同扩展名不同文件名共享同一键。
#[gpui_kit::test]
fn cache_walks_absent_wanted_ready_and_probes_once(cx: &mut TestAppContext) {
    let hits = Arc::new(AtomicUsize::new(0));
    cx.update(|cx| {
        setup(cx, counting_source(hits.clone(), Some(bitmap())));
        let key = icon_key("a.txt", IconKind::File);
        assert_eq!(
            state_of_for_test(cx, key.clone()),
            "absent",
            "未渲染 ⇒ 未登记"
        );

        // 首次渲染：登记 Wanted + 本次回退；平台调用仍为 0（渲染帧绝不探测）。
        let _ = icon("a.txt", IconKind::File, Slot::Inline, AppIcon::File, cx);
        assert_eq!(state_of_for_test(cx, key.clone()), "wanted");
        assert_eq!(hits.load(Ordering::SeqCst), 0, "渲染帧不得调平台 API");

        // 同键再渲染：不重复入队。
        let _ = icon("a.txt", IconKind::File, Slot::Inline, AppIcon::File, cx);
        assert_eq!(drain_pending_for_test(cx), 1, "同键只入队一次");
        assert_eq!(state_of_for_test(cx, key.clone()), "ready");
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // 同扩展名不同文件名 ⇒ 同一键 ⇒ 命中，不再探测。
        let _ = icon("b.TXT", IconKind::File, Slot::Grid, AppIcon::FileText, cx);
        assert_eq!(state_of_for_test(cx, key), "ready");
        assert_eq!(drain_pending_for_test(cx), 0);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "同扩展名只探测一次");
    });
}

/// 负缓存：平台拿不到 ⇒ `missing`，再次渲染不再入队、不再探测。
#[gpui_kit::test]
fn missing_is_negatively_cached_and_never_reprobed(cx: &mut TestAppContext) {
    let hits = Arc::new(AtomicUsize::new(0));
    cx.update(|cx| {
        setup(cx, counting_source(hits.clone(), None));
        let key = icon_key("a.unknownext", IconKind::File);
        let _ = icon(
            "a.unknownext",
            IconKind::File,
            Slot::Inline,
            AppIcon::File,
            cx,
        );
        assert_eq!(state_of_for_test(cx, key.clone()), "wanted");
        assert_eq!(drain_pending_for_test(cx), 1);
        assert_eq!(state_of_for_test(cx, key.clone()), "missing");

        let _ = icon(
            "b.unknownext",
            IconKind::File,
            Slot::Inline,
            AppIcon::File,
            cx,
        );
        assert_eq!(drain_pending_for_test(cx), 0, "负缓存不得再次入队");
        assert_eq!(state_of_for_test(cx, key), "missing");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "负缓存不得重复探测");
    });
}

/// 开关：关闭 ⇒ 清空缓存（含负缓存）且此后零平台调用；重开无需预热。
#[gpui_kit::test]
fn disabled_switch_clears_cache_and_yields_zero_platform_calls(cx: &mut TestAppContext) {
    let hits = Arc::new(AtomicUsize::new(0));
    cx.update(|cx| {
        setup(cx, counting_source(hits.clone(), Some(bitmap())));
        let key = icon_key("a.txt", IconKind::File);
        let _ = icon("a.txt", IconKind::File, Slot::Inline, AppIcon::File, cx);
        assert_eq!(drain_pending_for_test(cx), 1);
        assert_eq!(state_of_for_test(cx, key.clone()), "ready");

        // 关闭：清缓存 + 停探测。
        set_enabled(cx, false);
        assert_eq!(
            state_of_for_test(cx, key.clone()),
            "absent",
            "关闭必须清空缓存"
        );
        hits.store(0, Ordering::SeqCst); // 只统计关闭之后的调用
        let _ = icon("a.txt", IconKind::File, Slot::Inline, AppIcon::File, cx);
        assert_eq!(drain_pending_for_test(cx), 0, "关闭后不得入队");
        assert_eq!(hits.load(Ordering::SeqCst), 0, "关闭后平台调用恒 0");

        // 重开：下一次渲染重新登记（无需预热）。
        set_enabled(cx, true);
        let _ = icon("a.txt", IconKind::File, Slot::Inline, AppIcon::File, cx);
        assert_eq!(state_of_for_test(cx, key.clone()), "wanted");
        assert_eq!(drain_pending_for_test(cx), 1);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    });
}

/// `type_name` 语言规则：仅当 UI locale == 系统语言映射结果时采用；未命中、
/// 开关关闭、语言不一致一律 `None`。
#[gpui_kit::test]
fn type_name_only_when_locale_matches_system(cx: &mut TestAppContext) {
    // locale 是进程级全局：进 `locale_lock` 临界区串行（见 viewmodel::testutil）。
    let _locale = crate::viewmodel::testutil::locale_lock();
    let hits = Arc::new(AtomicUsize::new(0));
    cx.update(|cx| {
        setup(cx, counting_source(hits, Some(bitmap())));
        let _ = icon("a.zip", IconKind::File, Slot::Inline, AppIcon::Archive, cx);
        assert_eq!(drain_pending_for_test(cx), 1);
    });

    crate::init_locale(expected_mapped_locale());
    assert_eq!(
        cx.read(|cx| type_name("a.zip", IconKind::File, cx)),
        Some(SharedString::from("测试类型")),
        "语言一致 ⇒ 采用系统类型名"
    );
    assert_eq!(
        cx.read(|cx| type_name("b.rar", IconKind::File, cx)),
        None,
        "未命中 ⇒ 回退自有文案"
    );

    crate::init_locale(OPPOSITE_LOCALE.as_ref());
    assert_eq!(
        cx.read(|cx| type_name("a.zip", IconKind::File, cx)),
        None,
        "语言不一致 ⇒ 不采用（防中英混排）"
    );

    crate::init_locale(expected_mapped_locale());
    cx.update(|cx| set_enabled(cx, false));
    assert_eq!(
        cx.read(|cx| type_name("a.zip", IconKind::File, cx)),
        None,
        "开关关闭 ⇒ 一律 None"
    );
}

/// 真后台线程路径：`spawn_worker` 探测完成后必须写缓存并回投
/// `IconCacheFilled`（主线程泵据此只重绘）。
#[gpui_kit::test]
fn worker_probes_and_signals_redraw(cx: &mut TestAppContext) {
    let hits = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = smol::channel::unbounded::<VmSignal>();
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        init_with_source(
            cx,
            Some(Arc::new(tx)),
            counting_source(hits.clone(), Some(bitmap())),
        );
        let cache = cx
            .try_global::<FileIconCache>()
            .map(|cache| Arc::clone(&cache.inner));
        let Some(inner) = cache else {
            panic!("装配后应有缓存 Global");
        };
        // 测试钩子默认不起线程；这里显式起，走与生产同一条路径。
        spawn_worker(inner);
        let _ = icon("a.txt", IconKind::File, Slot::Inline, AppIcon::File, cx);
    });

    // 有界等待回投：挂死不如失败得响亮。
    let sig = smol::block_on(async {
        smol::future::or(async { rx.recv().await.ok() }, async {
            smol::Timer::after(std::time::Duration::from_secs(10)).await;
            None
        })
        .await
    });
    assert_eq!(
        sig,
        Some(VmSignal::IconCacheFilled),
        "worker 探测完成必须回投重绘信号"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "只探测一次");
    cx.read(|cx| {
        assert_eq!(
            state_of_for_test(cx, icon_key("a.txt", IconKind::File)),
            "ready",
            "worker 应把结果写进缓存"
        );
    });
}

/// headless 渲染烟测：首帧只有回退锚点；排空（= worker 已写缓存）+ 重绘后
/// 出现系统位图锚点、回退锚点消失。
#[gpui_kit::test]
fn first_frame_falls_back_then_system_bitmap_replaces(cx: &mut TestAppContext) {
    struct Probe;
    impl Render for Probe {
        fn render(&mut self, _: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            icon("a.txt", IconKind::File, Slot::Grid, AppIcon::FileText, cx)
        }
    }

    let hits = Arc::new(AtomicUsize::new(0));
    let source = counting_source(hits.clone(), Some(bitmap()));
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        init_with_source(cx, None, source);
        let view: gpui_kit::AnyView = cx.new(|_| Probe).into();
        gpui_kit::component::Root::new(view, window, cx)
    });

    // 首帧：只有回退（键刚登记，缓存还没有位图）。
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("file-icon-fallback").is_some(),
        "首帧必须是回退图标（不等图标）"
    );
    assert!(
        vtcx.debug_bounds("file-icon-system").is_none(),
        "首帧不得有位图（探测未回投）"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "渲染帧不得调平台 API");

    // 排空（= worker 探测完成）+ 重绘 ⇒ 位图替换回退。
    vtcx.update(|window, cx| {
        assert_eq!(drain_pending_for_test(cx), 1, "首次渲染应登记一个键");
        let _ = window.draw(cx);
    });
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let system = vtcx
        .debug_bounds("file-icon-system")
        .expect("缓存填充后应换成系统位图");
    assert!(
        vtcx.debug_bounds("file-icon-fallback").is_none(),
        "替换后不得残留回退图标"
    );
    // 32px 源按槽位定尺寸（Grid = ICON_CELL_ICON）：显式 w/h 必须胜过容器拉伸。
    assert_eq!(f32::from(system.size.width), f32::from(ICON_CELL_ICON));
    assert_eq!(f32::from(system.size.height), f32::from(ICON_CELL_ICON));
}

/// 回归（用户实测 2026-10-10）：列宽不足时**图标不得被 flex 挤压**——位图分支
/// 必须自带 `flex_shrink_0`（kit 的 `Icon` 回退分支本来就有，所以修复前只有
/// 「配到彩色位图」的行才会出现图标先被挤没的现象）。
///
/// 构造要点（第一版用例没有判别力，已按此修正）：必须让**图标之外**存在
/// 刚性内容（`flex_shrink_0` 的宽块）把赤字逼出来——若同行只有可收缩的文本，
/// 收紧压力落到文本上，图标本来也不会缩，用例就永远绿。
/// 修复前：`img` 默认 `flex_shrink=1` ⇒ 图标被压到远小于 `slot_px`；
/// 修复后：图标恒为 `slot_px`，多余内容溢出并被 `overflow_hidden` 裁掉。
#[gpui_kit::test]
fn narrow_cell_does_not_squeeze_system_icon(cx: &mut TestAppContext) {
    use gpui_kit::component::h_flex;
    use gpui_kit::div;

    struct Probe;
    impl Render for Probe {
        fn render(&mut self, _: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            h_flex()
                // 40px 单元格：16px 图标 + 刚性 100px 内容 ⇒ 赤字 76px 必须由
                // 可收缩项吸收（修复前只有 img 可缩 ⇒ 图标被挤没）。
                .w(px(40.))
                .min_w_0()
                .overflow_hidden()
                .gap(px(4.))
                .child(icon(
                    "a-very-long-name.txt",
                    IconKind::File,
                    Slot::Inline,
                    AppIcon::FileText,
                    cx,
                ))
                .child(
                    div()
                        .w(px(100.))
                        .flex_shrink_0()
                        .child("a-very-long-name-that-cannot-fit.txt"),
                )
        }
    }

    let hits = Arc::new(AtomicUsize::new(0));
    let source = counting_source(hits.clone(), Some(bitmap()));
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        init_with_source(cx, None, source);
        let view: gpui_kit::AnyView = cx.new(|_| Probe).into();
        gpui_kit::component::Root::new(view, window, cx)
    });

    // 排空（= worker 探测完成）+ 重绘 ⇒ 位图就位。
    vtcx.update(|window, cx| {
        assert_eq!(drain_pending_for_test(cx), 1, "首次渲染应登记一个键");
        let _ = window.draw(cx);
    });
    let bounds = vtcx
        .debug_bounds("file-icon-system")
        .expect("缓存填充后应是系统位图");
    // 期望值按**当前字号下的槽位值**取（1.35em）：断言的是「不被挤压」，不是某个写死的 px
    let want = vtcx.update(|_window, cx| slot_px(Slot::Inline, cx));
    assert_eq!(
        f32::from(bounds.size.width),
        f32::from(want),
        "列宽不足时图标宽度不得被挤压（应恒为 slot_px）"
    );
    assert_eq!(f32::from(bounds.size.height), f32::from(want), "高度同理");
}

// ── 可见墨迹居中（用户口径 2026-10-17：「图标没有垂直居中（有点靠上了）」）──

/// 造一张 `size×size` 的背景色位图（背景之外可再写像素）。
fn canvas(size: u32, bg: [u8; 4]) -> SystemIcon {
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for _ in 0..size * size {
        px.extend_from_slice(&bg);
    }
    SystemIcon {
        width: size,
        height: size,
        premultiplied_bgra: px,
        type_name: None,
    }
}

/// 在 `canvas` 上画一个矩形（预乘 BGRA：不透明色按原值写）。
fn fill(icon: &mut SystemIcon, x0: u32, y0: u32, x1: u32, y1: u32, color: [u8; 4]) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = ((y * icon.width + x) * 4) as usize;
            icon.premultiplied_bgra[i..i + 4].copy_from_slice(&color);
        }
    }
}

/// **浅色主题的文档图标**：近白「纸面」不可见，只看得见上部的彩色内容 ⇒ 必须下移
/// （用户报障的正是这一类：纸面在白色行背景上看不见，眼睛只看到靠上的内容）。
#[test]
fn invisible_paper_with_high_ink_needs_downward_shift() {
    let mut icon = canvas(32, [255, 255, 255, 255]); // 白纸（不可见）
    fill(&mut icon, 4, 2, 27, 9, [60, 120, 220, 255]); // 靠上的蓝色内容
    let dy = visible_ink_dy_ratio(&icon);
    assert!(dy > 0.05, "可见墨迹在纸面上部 ⇒ 应下移（正值），实得 {dy}");
    assert!(dy <= INK_DY_MAX + 1e-6, "位移必须被夹住：{dy}");
}

/// **深色主题的同类图标**：纸面本身是深色（可见）⇒ 全幅计入 ⇒ 不应再额外位移。
#[test]
fn dark_paper_counts_as_ink() {
    let mut icon = canvas(32, [0, 0, 0, 0]); // 透明画布
    fill(&mut icon, 3, 3, 28, 28, [30, 30, 34, 255]); // 深色纸面（整幅）
    let dy = visible_ink_dy_ratio(&icon);
    assert!(dy.abs() < 0.02, "整幅可见 ⇒ 偏移应≈0，实得 {dy}");
}

/// 内容居中（常见文件夹/压缩包图标）⇒ 不位移。
#[test]
fn centered_ink_keeps_the_slot_center() {
    let mut icon = canvas(32, [0, 0, 0, 0]);
    fill(&mut icon, 2, 6, 29, 25, [240, 200, 60, 255]); // 居中块
    let dy = visible_ink_dy_ratio(&icon);
    assert!(dy.abs() < 0.02, "居中内容不应位移，实得 {dy}");
}

/// 极端形态（墨迹全在顶/底）⇒ 夹到 ±[`INK_DY_MAX`]，绝不把图标推出槽位。
#[test]
fn ink_shift_is_clamped_at_both_ends() {
    let mut top = canvas(32, [0, 0, 0, 0]);
    fill(&mut top, 0, 0, 31, 1, [200, 0, 0, 255]);
    assert!((visible_ink_dy_ratio(&top) - INK_DY_MAX).abs() < 1e-6);
    let mut bottom = canvas(32, [0, 0, 0, 0]);
    fill(&mut bottom, 0, 30, 31, 31, [200, 0, 0, 255]);
    assert!((visible_ink_dy_ratio(&bottom) + INK_DY_MAX).abs() < 1e-6);
}

/// 全透明位图（平台脏数据）⇒ 0，不 panic（渲染侧据此不做位移）。
#[test]
fn fully_transparent_bitmap_has_no_shift() {
    let icon = canvas(8, [0, 0, 0, 0]);
    assert_eq!(visible_ink_dy_ratio(&icon), 0.);
}

/// **位图分支的可见墨迹居中确实落到了布局上**（用户口径 2026-10-17：「图标没有
/// 垂直居中（有点靠上了）」）。
///
/// 两个独立窗口、同一个探测源（按扩展名给图）：
/// - `flat.bin` ⇒ 满幅深色位图（可见墨迹 = 整幅 ⇒ 不位移）；
/// - `paper.png` ⇒ 「近白纸面 + 靠上内容」（浅色背景上只看得见上部内容 ⇒ 需下移）。
/// 断言：白纸图标的锚点**相对所在行中心**比满幅图标低 ≈ `INK_DY_MAX × 槽位`——即修正
/// 作用到了渲染几何上，而不是只算了个数。
#[gpui_kit::test]
fn visible_ink_shift_moves_the_bitmap_anchor(cx: &mut TestAppContext) {
    fn paper_icon() -> SystemIcon {
        let mut px = vec![255u8; 32 * 32 * 4]; // 白纸（浅色背景上看不见）
        for y in 2..=9u32 {
            for x in 4..=27u32 {
                let i = ((y * 32 + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[40, 90, 210, 255]); // 靠上的内容
            }
        }
        SystemIcon {
            width: 32,
            height: 32,
            premultiplied_bgra: px,
            type_name: None,
        }
    }
    fn full_bleed_icon() -> SystemIcon {
        SystemIcon {
            width: 32,
            height: 32,
            premultiplied_bgra: [45u8, 45, 45, 255].repeat(32 * 32),
            type_name: None,
        }
    }
    struct Probe {
        name: &'static str,
    }
    impl Render for Probe {
        fn render(&mut self, _: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            div()
                .debug_selector(|| "icon-row".into())
                .h(px(60.))
                .w(px(120.))
                .flex()
                .items_center()
                .child(icon(
                    self.name,
                    IconKind::File,
                    Slot::Inline,
                    AppIcon::FileText,
                    cx,
                ))
        }
    }
    let source: IconSource = Arc::new(|req: &IconRequest<'_>| match req.ext {
        Some("bin") => Some(full_bleed_icon()),
        Some("png") => Some(paper_icon()),
        _ => None,
    });
    let (rows, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        init_with_source(cx, None, source);
        let view: gpui_kit::AnyView = cx.new(|_| Probe { name: "flat.bin" }).into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    for _ in 0..2 {
        vtcx.update(|window, cx| {
            let _ = drain_pending_for_test(cx);
            let _ = window.draw(cx);
        });
    }
    let flat_row = vtcx.debug_bounds("icon-row").expect("满幅行");
    let flat_icon = vtcx.debug_bounds("file-icon-system").expect("满幅图标");
    let flat_rel = mid(flat_icon) - mid(flat_row);
    drop(rows);

    let (_rows2, vtcx2) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let view: gpui_kit::AnyView = cx.new(|_| Probe { name: "paper.png" }).into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    for _ in 0..2 {
        vtcx2.update(|window, cx| {
            let _ = drain_pending_for_test(cx);
            let _ = window.draw(cx);
        });
    }
    let paper_row = vtcx2.debug_bounds("icon-row").expect("白纸行");
    let paper_icon = vtcx2.debug_bounds("file-icon-system").expect("白纸图标");
    let paper_rel = mid(paper_icon) - mid(paper_row);

    let slot = vtcx2.update(|_w, cx| slot_px(Slot::Inline, cx));
    let want = f32::from(slot) * INK_DY_MAX;
    assert!(
        (flat_rel - 1.5).abs() <= 0.8,
        "满幅图标应只带光学下沉（+1.5），实得 {flat_rel:+.2}"
    );
    assert!(
        paper_rel - flat_rel >= want - 0.8,
        "白纸图标应再下移约 {want:.2}px（满幅 {flat_rel:+.2} / 白纸 {paper_rel:+.2}）"
    );

    fn mid(b: gpui_kit::Bounds<gpui_kit::Pixels>) -> f32 {
        f32::from(b.origin.y) + f32::from(b.size.height) / 2.
    }
}
