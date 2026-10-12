use super::*;
use crate::viewmodel::{JobRow, JobRowState};
use gpui_kit::px;

fn row(job_id: u64, state: JobRowState) -> JobRow {
    JobRow {
        job_id,
        kind: "extract",
        state,
        progress: Some(0.5),
        speed_bps: 123_476_288.0, // ≈117.7 MiB/s
        eta: Some(std::time::Duration::from_secs(72)),
        done_bytes: 5,
        total_bytes: 10,
        entries_done: 45,
        entries_total: 408,
        failures: Vec::new(),
        finished: None,
        src: "demo.zip".into(),
        dest: Some("D:/out".into()),
        format: "zip".into(),
        started_at: None,
        elapsed: None,
        current_entry: Some("docs/报告.txt".into()),
        final_line: None,
        recent_entries: std::collections::VecDeque::new(),
    }
}

// 终局自动关判定：默认（exit_on_finish=false）恒停留——输出区结果需用户
// 确认；开关开（= 设置页/复选框「完成后退出」勾上）时 Done/Cancelled 关、
// Failed/Partial 恒停留
#[test]
fn terminal_close_semantics() {
    // 默认：全部停留（exit_on_finish=false）
    for s in [
        JobRowState::Done,
        JobRowState::Cancelled,
        JobRowState::Failed,
        JobRowState::PartialDone,
    ] {
        assert!(!is_close_on_terminal(s, false), "{s:?} 默认停留");
    }
    // 开关开：Done/Cancelled 关；Failed/Partial 停留
    assert!(is_close_on_terminal(JobRowState::Done, true));
    assert!(is_close_on_terminal(JobRowState::Cancelled, true));
    assert!(
        !is_close_on_terminal(JobRowState::Failed, true),
        "失败恒停留"
    );
    assert!(
        !is_close_on_terminal(JobRowState::PartialDone, true),
        "部分失败恒停留"
    );
    assert!(!is_close_on_terminal(JobRowState::Running, true));
}

// 延迟关窗（用户实测回归：「智能解压 + 完成后退出」弹窗一闪而过）——
// 排定后**未到点不得关**，到点才关且注册表退场。
#[gpui_kit::test]
fn pending_close_fires_only_after_deadline(cx: &mut gpui_kit::TestAppContext) {
    let mut r = row(9, JobRowState::Done);
    r.progress = Some(1.0);
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        open_progress_window(cx, &r, |_, _| {}, |_, _| {});
    });
    assert_eq!(cx.update(|cx| cx.windows().len()), 1, "窗口已开");

    // 排定 + 未到点 → 窗口必须停留（旧缺陷：终局立即关窗 → 弹窗一闪）
    cx.update(|cx| {
        cx.global_mut::<ProgressGlobal>()
            .0
            .get_mut(&9)
            .expect("窗口已注册")
            .close_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
        tick_pending_closes(cx);
        assert_eq!(cx.windows().len(), 1, "未到点不得关窗");
    });

    // 到点 → 关窗 + 注册表退场
    cx.update(|cx| {
        cx.global_mut::<ProgressGlobal>()
            .0
            .get_mut(&9)
            .expect("窗口仍在")
            .close_at = Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
        tick_pending_closes(cx);
        assert!(cx.windows().is_empty(), "到点必须关窗");
        assert!(
            cx.global::<ProgressGlobal>().0.is_empty(),
            "关窗后注册表必须退场"
        );
    });
}

// 「完成后自动关闭窗口」复选框：真的画出来 + 点击发出上行事件（回调带
// 新勾选值）。「面板画了开关但没接线」是本仓库踩过的坑——必须能被点。
#[gpui_kit::test]
fn exit_checkbox_renders_and_fires_callback(cx: &mut gpui_kit::TestAppContext) {
    let (host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        ProgressWindow {
            job_id: 3,
            title: "解压 demo.zip".into(),
            summary: ("demo.zip".into(), "D:/out".into()),
            row: row(3, JobRowState::Running),
            output: Some(file_list::OutputState::new()),
            output_tab: file_list::OutputTab::default(),
            on_cancel: Box::new(|_, _| {}),
            exit_on_finish: false,
            on_exit_toggle: Box::new(|_, _| {}),
            allow_quit: true,
        }
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let cell = vtcx
        .debug_bounds("pw-exit-on-finish")
        .expect("复选框应有渲染命中区");
    assert!(cell.size.width > px(0.) && cell.size.height > px(0.));
    drop(host);
}

// 复选框回调把新勾选值带给注入方（传值语义锚点——shell 层据此走
// apply_setting 五步流程；App 现场由 gpui 测试窗口提供）。
#[gpui_kit::test]
fn exit_toggle_callback_receives_new_value(cx: &mut gpui_kit::TestAppContext) {
    let seen = std::rc::Rc::new(std::cell::Cell::new(Option::<bool>::None));
    let sink = seen.clone();
    cx.update(|cx| {
        let on_toggle: ExitToggleHandler = Box::new(move |v, _| sink.set(Some(v)));
        on_toggle(true, cx);
        assert_eq!(seen.get(), Some(true), "回调必须收到新勾选值 true");
        on_toggle(false, cx);
        assert_eq!(seen.get(), Some(false), "回调必须收到新勾选值 false");
    });
}

// 统计行：速度/剩余/条数三段（fmt_bytes 复用——MiB/s 边界格式）
#[test]
fn stat_line_composes_speed_eta_count() {
    let line = stat_line(
        123_476_288.0,
        Some(std::time::Duration::from_secs(72)),
        45,
        408,
    );
    assert!(line.contains("MiB/s"), "速度段：{line}");
    assert!(line.contains("剩余 01:12"), "剩余段：{line}");
    assert!(line.contains("45/408 条"), "条数段：{line}");
    // 无速度/无 ETA → 占位
    let idle = stat_line(0.0, None, 0, 0);
    assert!(idle.contains('—') && idle.contains("--:--"), "占位：{idle}");
}

// 输出区自适应高度实证（R-GUI 走查 #1：flex_1 链必须吃满窗口剩余高度——
// 三个探针 = 内容列 / 输出区根 / 虚拟列表容器；失败则输出区塌回 min 120）
#[gpui_kit::test]
fn output_area_fills_remaining_window_height(cx: &mut gpui_kit::TestAppContext) {
    let (host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        ProgressWindow {
            job_id: 7,
            title: "解压 demo.zip".into(),
            summary: ("demo.zip".into(), "D:/out".into()),
            row: row(7, JobRowState::Running),
            output: Some(file_list::OutputState::new()),
            output_tab: file_list::OutputTab::default(),
            on_cancel: Box::new(|_, _| {}),
            exit_on_finish: false,
            on_exit_toggle: Box::new(|_, _| {}),
            allow_quit: true,
        }
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let content = vtcx.debug_bounds("pw-content").expect("内容列有界");
    let area = vtcx.debug_bounds("pw-output-area").expect("输出区根有界");
    let boxb = vtcx.debug_bounds("pw-output-box").expect("输出框有界");
    // 内容列应吃满窗口高（Root → content h_full）
    assert!(
        content.size.height > px(400.),
        "内容列高度 {:.0}px 应接近窗口高",
        content.size.height
    );
    // 输出区根 = 内容列剩余主体（减固定行 ≈ 230px 与 padding 32px）——
    // 裸 div 包装层会把 flex_1 链掐断（Display::Block 子元素 flex 无效）
    assert!(
        area.size.height > px(250.),
        "输出区根 {:.0}px 应吃满剩余高度",
        area.size.height
    );
    // 虚拟列表容器 ≈ 输出区根 - TabBar（~36px）- gap
    assert!(
        boxb.size.height > px(200.),
        "输出框 {:.0}px 应吃满剩余高度（min 120 底 = 链断）",
        boxb.size.height
    );
    drop(host);
}

// apply 行数据纯拷贝（含 current_entry/recent_entries 新字段）+
// 输出区行集增量推入（gpui test 窗口现场；根视图 = ProgressWindow）
#[gpui_kit::test]
fn apply_copies_row_snapshot_and_fills_output(cx: &mut gpui_kit::TestAppContext) {
    let (host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        let mut w = ProgressWindow {
            job_id: 1,
            title: "解压 demo.zip".into(),
            summary: ("demo.zip".into(), "D:/out".into()),
            row: row(1, JobRowState::Queued),
            output: Some(file_list::OutputState::new()),
            output_tab: file_list::OutputTab::default(),
            on_cancel: Box::new(|_, _| {}),
            exit_on_finish: false,
            on_exit_toggle: Box::new(|_, _| {}),
            allow_quit: true,
        };
        let mut updated = row(1, JobRowState::Running);
        updated
            .recent_entries
            .push_back(crate::viewmodel::RecentEntry {
                name: "docs/报告.txt".into(),
                bytes: 2048,
                ok: true,
                reason: String::new(),
                code: 0,
            });
        w.apply(&updated);
        w
    });
    vtcx.read(|cx| {
        let w = host.read(cx);
        let (current, done) = (w.row.current_entry.clone(), w.row.entries_done);
        assert_eq!(current.as_deref(), Some("docs/报告.txt"));
        assert_eq!(done, 45);
        // 输出区三页签行集：条目行已推入（虚拟列表数据面纯内存读）
        let out = w.output.as_ref().expect("output state 已建");
        assert_eq!(out.result.len(), 1, "结果页含条目行");
        assert_eq!(out.result[0].name, "docs/报告.txt");
        assert_eq!(out.ok.len(), 1, "成功页含条目行");
        assert_eq!(out.fail.len(), 0);
    });
}

// 语言切换 → 已展示的输出区文案随新 locale 重建（`Effect::Locale` 的
// UI 半场：locale 是 rust-i18n 进程级全局，已定稿的 `OutLine.tail` 必须
// 整表重算，否则窗口停留在旧语言）。
#[gpui_kit::test]
fn lang_switch_rebuilds_live_output_text(cx: &mut gpui_kit::TestAppContext) {
    // locale 是进程级全局 → 与 file_list 的文案用例串行（防互读对方语言）
    let _locale = crate::viewmodel::testutil::locale_lock();
    crate::init_locale("zh-CN");
    let mut r = row(1, JobRowState::Running);
    r.recent_entries.push_back(crate::viewmodel::RecentEntry {
        name: "bad.txt".into(),
        bytes: 0,
        ok: false,
        reason: "err.hz_1004".into(),
        code: 1004,
    });
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        open_progress_window(cx, &r, |_, _| {}, |_, _| {});
    });
    assert_eq!(failed_tail(cx, 1), "源不存在", "zh-CN：灌入时定稿为中文");

    cx.update(|cx| {
        crate::init_locale("en");
        relocalize(cx, &[r.clone()]);
    });
    assert_eq!(
        failed_tail(cx, 1),
        "Source not found",
        "换语言后既有行随新 locale 重建"
    );

    crate::init_locale("zh-CN"); // 复原全局（其余用例按中文断言）
}

// 泵生效分派（`Effect::Locale` 的 UI 半场，端到端）：语言信号 → 泵函数
// → rust-i18n 全局 locale 切换 + 已定稿文案重建。
//
// 这是「设置面板改语言没反应」报告的直接断言面：把 `runtime::apply_ui_effects`
// 当作泵本体调用（泵循环内联在 `run_ui` 的异步闭包里，测试不可达；本轮已把
// 生效应用抽成该函数以便覆盖）。
#[gpui_kit::test]
fn pump_effects_switch_locale_and_rebuild_finalized_text(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::{apply_ui_effects, batch_requested};
    use crate::viewmodel::{SettingId, VmSignal};
    // locale 是进程级全局 → 与其它文案用例串行
    let _locale = crate::viewmodel::testutil::locale_lock();
    crate::init_locale("zh-CN");
    let mut r = row(1, JobRowState::Running);
    r.recent_entries.push_back(crate::viewmodel::RecentEntry {
        name: "bad.txt".into(),
        bytes: 0,
        ok: false,
        reason: "err.hz_1004".into(),
        code: 1004,
    });
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        open_progress_window(cx, &r, |_, _| {}, |_, _| {});
    });
    assert_eq!(failed_tail(cx, 1), "源不存在", "起始：中文定稿");

    // 面板选「English」→ `apply_setting` 落库后发出的信号（下标 2 = en）
    let batch = vec![VmSignal::SettingApplied(SettingId::Lang)];
    let rows = vec![r.clone()];
    cx.update(|cx| {
        assert!(batch_requested(&batch, SettingId::Lang), "批内应识别语言项");
        apply_ui_effects(cx, None, Some(2), None, &rows);
    });

    // ① 全局 locale 真的切了（后续 `t!` 走英文）
    assert_eq!(rust_i18n::locale().to_string(), "en", "locale 已切 en");
    assert_eq!(rust_i18n::t!("err.hz_1004"), "Source not found");
    // ② 已定稿文本被重建（增量 apply 无法回译，必须整表重算）
    assert_eq!(
        failed_tail(cx, 1),
        "Source not found",
        "泵应用后窗口文案随新 locale 重建（否则界面「没反应」）"
    );

    // ③ 未选语言时不动作（防止每批都重设 locale）
    cx.update(|cx| apply_ui_effects(cx, None, None, None, &[]));
    assert_eq!(rust_i18n::locale().to_string(), "en", "无语言项不改 locale");

    crate::init_locale("zh-CN"); // 复原全局（其余用例按中文断言）
}

/// 布局密度的 **UI 半场**：`apply_ui_effects(density_ix)` 必须把刻度写进
/// 全局（`sp()`/`row_band()` 在元素构建期读它，拿不到 `cx`）。
///
/// 缺这一步的症状：设置页把密度改成「紧凑」、界面纹丝不动（数据面已落盘、
/// 面板还显示紧凑——观感即「设置没生效」）。
#[gpui_kit::test]
fn apply_ui_effects_writes_density_scale(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::apply_ui_effects;
    use crate::viewmodel::density::Density;

    crate::ui::spacing::set_density(Density::Standard); // 起点明确（线程局部）
    cx.update(|cx| {
        apply_ui_effects(cx, None, None, Some(Density::Loose.index()), &[]);
    });
    assert_eq!(
        crate::ui::spacing::density(),
        Density::Loose,
        "泵应写入宽松档刻度"
    );
    let want = 0.5 * Density::Loose.scale(); // sp(2.) = 2/4 rem × 系数
    assert!(
        (crate::ui::spacing::sp(2.).0 - want).abs() < 1e-6,
        "宽松档 sp(2.) 应为 {want}rem（2/4 × {}），实得 {}",
        Density::Loose.scale(),
        crate::ui::spacing::sp(2.).0
    );

    // 批内没有密度项 → 不动刻度（防止每批无差别写全局）
    cx.update(|cx| apply_ui_effects(cx, None, None, None, &[]));
    assert_eq!(
        crate::ui::spacing::density(),
        Density::Loose,
        "无密度项不得改刻度"
    );

    crate::ui::spacing::set_density(Density::Standard); // 复原
}

/// 活窗口「结果页」首个失败行的尾注（locale 相关文案断言面）。
fn failed_tail(cx: &gpui_kit::TestAppContext, job_id: u64) -> String {
    cx.read(|cx| {
        let slot = cx
            .global::<ProgressGlobal>()
            .0
            .get(&job_id)
            .expect("进度窗口槽已登记");
        let w = slot.entity.read(cx);
        let out = w.output.as_ref().expect("输出区已建");
        out.lines_of(file_list::OutputTab::Result)
            .iter()
            .find(|l| l.failed)
            .map(|l| l.tail.clone())
            .expect("失败行在结果页")
    })
}

mod active_state_line_tests {
    use super::active_state_line;
    use crate::viewmodel::JobRowState;

    /// 回归：Running 无进度数据不得显示「等待开始」（窗口与实际解压情况必须一致）。
    #[test]
    fn running_without_progress_is_preparing_not_waiting() {
        crate::init_locale("zh-CN");
        let running = active_state_line(JobRowState::Running);
        let queued = active_state_line(JobRowState::Queued);
        assert!(
            running.contains("准备中"),
            "Running 应显示准备中: {running}"
        );
        assert!(
            !running.contains("等待开始"),
            "Running 不得说等待开始: {running}"
        );
        assert!(queued.contains("等待开始"), "Queued 才是等待开始: {queued}");
        assert_ne!(running, queued, "两种处境文案必须不同");
    }
}
