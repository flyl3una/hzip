use super::*;

/// 明细行（用户口径 2026-09-24「增强可观测性」）：阶段 + 已处理字节，
/// 且有/无总量两种句式；什么都没写时不占行。
#[test]
fn detail_line_reports_phase_and_bytes() {
    let d = OpenDetail::default();
    assert_eq!(d.line(), None, "无阶段无字节 → 不占行");
    d.set_phase(crate::tr("openprogress.phase_index_inner"));
    assert_eq!(d.line().as_deref(), Some("正在索引内层档案…"), "只有阶段");
    d.set_total(3 * 1024 * 1024);
    d.add_done(1024 * 1024);
    assert_eq!(
        d.line().as_deref(),
        Some("正在索引内层档案… · 已处理 1.0 MiB / 3.0 MiB"),
        "阶段 · 已处理/总量（字节数走 fmt_bytes）"
    );
    // 总量未知 → 只报已处理量（7z 数据加密这类「大小不可预知」的场景）
    let d2 = OpenDetail::default();
    d2.add_done(2048);
    assert_eq!(d2.line().as_deref(), Some("已处理 2.0 KiB"), "总量未知");
}

/// 计数读：`Read` 语义原样转发（字节/EOF 都照旧），只把读到的量累加进明细
/// ——「解出内层档案」的进度就来自它。
#[test]
fn counting_reader_accumulates_read_bytes() {
    use std::io::Read as _;
    let d = OpenDetail::default();
    let mut r = CountingReader::new(std::io::Cursor::new(vec![7u8; 5000]), d.clone());
    let mut buf = [0u8; 2000];
    assert_eq!(r.read(&mut buf).unwrap(), 2000);
    assert_eq!(d.done(), 2000);
    let mut rest = Vec::new();
    assert_eq!(r.read_to_end(&mut rest).unwrap(), 3000, "EOF 行为不变");
    assert_eq!(rest.len(), 3000);
    assert_eq!(d.done(), 5000, "累计 = 整条内容长度");
}

/// 失败不自动关窗、成功自动关窗（用户口径 2026-09-24；窗口生命周期判定）。
#[test]
fn close_on_finish_only_when_succeeded() {
    assert!(is_close_on_finish(false), "成功 → 到点自动关");
    assert!(!is_close_on_finish(true), "失败 → 不关，等用户处置");
}

/// 失败后窗口**留在屏幕上**、成功按延迟关窗（真实窗口：注册表排期 + tick 行为）。
#[gpui_kit::test]
fn failed_window_is_not_closed_automatically(cx: &mut gpui_kit::TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);

        // 失败：无关闭排期，tick 也不关
        let key = open(
            cx,
            "big.cpio.gz".into(),
            OpenDetail::default(),
            OpLabels::default(),
        );
        finish(cx, key, Some("HZ-1002: IO 错误: 磁盘空间不足。".into()));
        let slot = snapshot(cx).into_iter().find(|(k, ..)| *k == key);
        let slot = slot.expect("失败后窗口必须仍在注册表（不自动关）");
        assert!(slot.3.is_none(), "失败不得排定自动关窗");
        tick(cx);
        assert!(
            snapshot(cx).iter().any(|(k, ..)| *k == key),
            "tick 之后失败窗口仍必须留在屏幕上（等用户处置）"
        );
        close_slot(cx, key);

        // 对照：成功 → 排定延迟关窗（防「两边都不关」）
        let key_ok = open(
            cx,
            "ok.zip".into(),
            OpenDetail::default(),
            OpLabels::default(),
        );
        finish(cx, key_ok, None);
        let slot_ok = snapshot(cx).into_iter().find(|(k, ..)| *k == key_ok);
        assert!(
            slot_ok.expect("成功窗口在注册表").3.is_some(),
            "成功仍按 [jobs].open_close_delay_ms 自动关"
        );
        close_slot(cx, key_ok);
    });
}

/// 按展示名关窗（用户实测 2026-09-24：打开 A 后再打开 B，A 的「正在打开」窗
/// 还在屏幕上转 ⇒ 换会话顶替时按旧目标名关掉它）。同名窗口只关它自己，
/// 别的操作的窗不受影响。
#[gpui_kit::test]
fn close_by_label_only_closes_matching_window(cx: &mut gpui_kit::TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let a = open(
            cx,
            "cpio.gz".into(),
            OpenDetail::default(),
            OpLabels::default(),
        );
        let b = open(
            cx,
            "qcow2.gz".into(),
            OpenDetail::default(),
            OpLabels::default(),
        );
        assert_eq!(snapshot(cx).len(), 2, "两窗并存");
        close_by_label(cx, "cpio.gz");
        let left = snapshot(cx);
        assert_eq!(left.len(), 1, "只关掉同名那一扇");
        assert_eq!(left[0].1, "qcow2.gz", "留下的是另一个操作");
        // 不存在的名字：no-op（不 panic、不动别人的窗）
        close_by_label(cx, "nope.zip");
        assert_eq!(snapshot(cx).len(), 1);
        close_slot(cx, b);
        let _ = a;
    });
}

/// 长名零宽断点：**不丢字符**（去掉断点后与原串逐字符相同）、按间隔插点。
#[test]
fn breakable_only_inserts_zero_width_breaks() {
    let name = "initramfs-tools_0.142ubuntu25_all.cpio.gz";
    let broken = breakable(name, 8);
    assert_eq!(broken.replace('\u{200b}', ""), name, "断点不得改变任何字符");
    assert!(broken.contains('\u{200b}'), "长名应至少有一个断点");
    assert_eq!(
        broken.matches('\u{200b}').count(),
        name.chars().count() / 8,
        "每 8 个字符一个断点"
    );
    assert_eq!(breakable("a.gz", 8), "a.gz", "短名不加断点");
    assert_eq!(breakable("", 8), "", "空名原样");
}

/// 长档案名**折行**而不是被裁掉（用户口径 2026-09-24）：把窗口放在一个窄宿主里
/// 渲染，长名的标签盒必须**高于一行**（= 折了行），且不超出宿主宽度。
#[gpui_kit::test]
fn long_label_wraps_within_window(cx: &mut gpui_kit::TestAppContext) {
    struct Host(gpui_kit::Entity<OpenProgressWindow>);
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            _window: &mut gpui_kit::Window,
            _cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            // 窄宿主 = 320px 宽的窗口（比 OPEN_PROGRESS_WINDOW_W 更窄，
            // 长名必然要折；单行文本在此宽度下不可能容纳 70+ 字符）
            v_flex()
                .w(gpui_kit::px(320.))
                .h(gpui_kit::px(180.))
                .child(self.0.clone())
        }
    }

    let long = "initramfs-tools_0.142ubuntu25_all_amd64_very_long_archive_name_for_wrap.cpio.gz";
    let (_host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let win = cx.new(|_| OpenProgressWindow {
            detail: OpenDetail::default(),
            labels: OpLabels::default(),
            label: long.to_string(),
            started: Instant::now(),
            finished_at: None,
            done: false,
            error: None,
        });
        Host(win)
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let b = vtcx.debug_bounds("op-label").expect("标签锚点");
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    assert!(h > 30.0, "长名应折成多行（一行约 20px）：实际 {w}x{h}");
    assert!(w <= 320.0 + 0.5, "折行后不得超出窗口宽：{w}");
}
