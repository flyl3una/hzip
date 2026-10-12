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

/// 槽位边长取自 `ui/constant.rs` 基准。
#[test]
fn slot_px_matches_constant_baselines() {
    // 基准 = kit 同款：Inline 16（Icon 默认）/ Small 14（kit `Size::Small` = size_3p5 = 14px）/
    // Grid = `ICON_CELL_ICON`（constant.rs 单一来源）。
    assert_eq!(slot_px(Slot::Inline), px(16.));
    assert_eq!(slot_px(Slot::Small), px(14.));
    assert_eq!(slot_px(Slot::Grid), ICON_CELL_ICON);
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
    assert_eq!(
        f32::from(bounds.size.width),
        f32::from(px(16.)),
        "列宽不足时图标宽度不得被挤压（应恒为 slot_px：Inline = 16）"
    );
    assert_eq!(
        f32::from(bounds.size.height),
        f32::from(px(16.)),
        "高度同理"
    );
}
