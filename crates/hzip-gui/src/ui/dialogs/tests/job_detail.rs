//! 任务详情弹窗测试（用户口径 2026-10-09）。
//!
//! 两类断言：① [`detail_fields`] / [`detail_failures`] 的**纯函数**组装口径
//! （不依赖窗口）；② 真实点击路径——点行内详情钮开 modal、点背板关掉，
//! 以及行内「取消」钮把 `JobsCancel(id)` 送上行通道。

use super::{MAX_H, MIN_H, MIN_W, VIEWPORT_MARGIN_X, detail_failures, detail_fields, resized_by};
use crate::ui::contracts::{AppStateUiSnapshot, DialogEvent, ShellHandlers};
use crate::viewmodel::rows::FailureRow;
use crate::viewmodel::{JobRow, JobRowState};
use gpui_kit::component::WindowExt as _;
use gpui_kit::{AppContext as _, IntoElement, ParentElement as _, Styled as _, Window};

/// 基线行：运行中、有总量（字段按需覆盖）。
fn row() -> JobRow {
    JobRow {
        job_id: 7,
        kind: "extract",
        state: JobRowState::Running,
        progress: Some(0.42),
        speed_bps: 1024.0,
        eta: Some(std::time::Duration::from_secs(9)),
        done_bytes: 440,
        total_bytes: 1024,
        entries_done: 3,
        entries_total: 7,
        failures: Vec::new(),
        finished: None,
        src: "demo.zip".into(),
        current_entry: None,
        final_line: None,
        recent_entries: std::collections::VecDeque::new(),
        dest: Some("/tmp/out".into()),
        format: "zip".into(),
        started_at: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(3600)),
        elapsed: Some(std::time::Duration::from_secs(65)),
    }
}

fn value_of<'a>(fields: &'a [(&'static str, String)], label: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, v)| v.as_str())
}

/// 全量字段齐备 + 取值口径（id/行为/状态/输入输出/格式/耗时/大小/进度/条目）。
#[test]
fn detail_fields_cover_row_identity_and_metrics() {
    let mut r = row();
    r.current_entry = Some("a/b.txt".into());
    let fields = detail_fields(&r);

    // 名称作**数据项**列在首行（弹窗标题是固定文案「任务详情」，不含任务名）
    assert_eq!(
        fields.first().map(|(label, _)| *label),
        Some("jobs.col_title"),
        "名称行须排在首行"
    );
    assert_eq!(value_of(&fields, "jobs.col_title"), Some("解压 demo.zip"));
    assert_eq!(value_of(&fields, "jobs.detail_id"), Some("7"));
    assert_eq!(value_of(&fields, "jobs.detail_action"), Some("解压"));
    assert_eq!(value_of(&fields, "jobs.detail_state"), Some("运行中"));
    assert_eq!(value_of(&fields, "jobs.detail_input"), Some("demo.zip"));
    assert_eq!(value_of(&fields, "jobs.detail_output"), Some("/tmp/out"));
    assert_eq!(value_of(&fields, "jobs.detail_format"), Some("zip"));
    // 终局耗时定格值（65s → `1m 05s`）
    assert_eq!(value_of(&fields, "jobs.detail_elapsed"), Some("1m 05s"));
    assert_eq!(value_of(&fields, "jobs.detail_size"), Some("1.0 KiB"));
    assert_eq!(value_of(&fields, "jobs.detail_progress"), Some("42%"));
    assert_eq!(value_of(&fields, "jobs.detail_entries"), Some("3 / 7"));
    assert_eq!(
        value_of(&fields, "jobs.detail_current_entry"),
        Some("a/b.txt")
    );
}

/// 队列中（无总量、未开始）：进度「未知总量」、条目与耗时 `—`、无当前条目。
#[test]
fn detail_fields_unknown_totals_use_placeholders() {
    let r = JobRow {
        state: JobRowState::Queued,
        progress: None,
        entries_done: 0,
        entries_total: 0,
        started_at: None,
        elapsed: None,
        dest: None,
        current_entry: None,
        ..row()
    };
    let fields = detail_fields(&r);

    assert_eq!(
        value_of(&fields, "jobs.detail_progress"),
        Some("未知总量"),
        "无总量不得冒充 0%"
    );
    assert_eq!(value_of(&fields, "jobs.detail_entries"), Some("—"));
    assert_eq!(value_of(&fields, "jobs.detail_elapsed"), Some("—"));
    assert_eq!(value_of(&fields, "jobs.detail_output"), Some("—"));
    assert!(
        value_of(&fields, "jobs.detail_current_entry").is_none(),
        "无当前条目不应出现该行"
    );
}

/// 失败原因：终局原文 + 逐条目明细**不重复**（`jobs_row_ui` 在无明细时用终局
/// 摘要兜底填 `failures`——不去重就会同一条印两遍）。
#[test]
fn detail_failures_dedupes_terminal_summary() {
    let r = JobRow {
        state: JobRowState::Failed,
        final_line: Some(FailureRow {
            entry: String::new(),
            code: 3101,
            message: "hz-3101 数据损坏".into(),
        }),
        ..row()
    };
    let lines = detail_failures(&r);
    assert_eq!(
        lines,
        vec!["HZ-3101: hz-3101 数据损坏".to_string()],
        "终局原因只应出现一次（去重后）"
    );
}

/// 失败原因：逐条目明细各自成行（条目名前缀），终局原文排在最前。
#[test]
fn detail_failures_lists_entries_after_terminal_summary() {
    let r = JobRow {
        state: JobRowState::PartialDone,
        final_line: Some(FailureRow {
            entry: String::new(),
            code: 2100,
            message: "整档收尾失败".into(),
        }),
        failures: vec![
            FailureRow {
                entry: "a.txt".into(),
                code: 0,
                message: "CRC 不匹配".into(),
            },
            FailureRow {
                entry: "b/c.bin".into(),
                code: 0,
                message: "写盘失败".into(),
            },
        ],
        ..row()
    };
    let lines = detail_failures(&r);
    assert_eq!(
        lines,
        vec![
            "HZ-2100: 整档收尾失败".to_string(),
            "a.txt: CRC 不匹配".to_string(),
            "b/c.bin: 写盘失败".to_string(),
        ]
    );
}

/// 无失败 → 无失败行（弹窗不画空区块）。
#[test]
fn detail_failures_empty_when_clean() {
    assert!(detail_failures(&row()).is_empty());
}

/// 真实点击路径：点行内详情钮 → modal 打开（画的是本弹窗的 body）；
/// 点背板 → 关掉（用户口径「窗口要能支持关闭」）。
#[gpui_kit::test]
fn detail_button_opens_modal_and_backdrop_closes_it(cx: &mut gpui_kit::TestAppContext) {
    struct Host;
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            let snap = AppStateUiSnapshot {
                job_rows: std::sync::Arc::new(vec![row()]),
                ..Default::default()
            };
            gpui_kit::div()
                .size_full()
                .child(crate::ui::views::jobs::jobs_root(
                    cx,
                    window,
                    &snap,
                    &ShellHandlers::default(),
                    std::rc::Rc::new(|_, _| {}),
                    None,
                ))
            // 0.7.1：dialog 层无需根视图手工挂——`WindowState` 插件在
            // `gpui_base::Root::render` 内自动挂
            // （`Root` 的手工挂层函数整族已从 kit 删除，登记了自然会被画）。
        }
    }

    let (_host, vtcx) = cx.add_window_view(|window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        // 弹窗挂在窗口的 `Root` 上（kit `WindowExt::open_dialog` 需 Root 实体；
        // 生产路径由 `run_ui` 的 `Root::new(AppShell)` 提供）——测试同构装配。
        let host = cx.new(|_| Host);
        let view: gpui_kit::AnyView = host.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        !vtcx.update(|window, cx| window.has_active_dialog(cx)),
        "前置：不该有弹窗"
    );

    let hit = vtcx
        .debug_bounds("job-detail-7")
        .expect("任务行的详情钮")
        .center();
    vtcx.simulate_click(hit, gpui_kit::Modifiers::default());
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.update(|window, cx| window.has_active_dialog(cx)),
        "点详情钮应弹出 modal"
    );
    assert!(
        vtcx.debug_bounds("job-detail-body").is_some(),
        "弹出的应是任务详情弹窗本体"
    );

    // 点背板（弹窗之外、标题栏拖拽区之下）→ 关闭。
    // y 必须 > `TITLE_BAR_HEIGHT`：背板按下处理会忽略标题栏一带（防拖窗误关）。
    vtcx.simulate_click(
        gpui_kit::point(gpui_kit::px(2.), gpui_kit::px(500.)),
        gpui_kit::Modifiers::default(),
    );
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        !vtcx.update(|window, cx| window.has_active_dialog(cx)),
        "点背板应关掉弹窗"
    );
}

/// 行内「取消」钮必须把 `JobsCancel(job_id)` 送上行通道（旧实现只画钮不接线，
/// 点了毫无反应——用户实报 2026-10-09）。
#[gpui_kit::test]
fn row_cancel_button_emits_jobs_cancel(cx: &mut gpui_kit::TestAppContext) {
    struct Host {
        seen: std::rc::Rc<std::cell::RefCell<Vec<DialogEvent>>>,
    }
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            let snap = AppStateUiSnapshot {
                job_rows: std::sync::Arc::new(vec![row()]),
                ..Default::default()
            };
            let seen = self.seen.clone();
            crate::ui::views::jobs::jobs_root(
                cx,
                window,
                &snap,
                &ShellHandlers::default(),
                std::rc::Rc::new(move |e, _| seen.borrow_mut().push(e)),
                None,
            )
        }
    }

    let seen: std::rc::Rc<std::cell::RefCell<Vec<DialogEvent>>> = Default::default();
    let (_host, vtcx) = cx.add_window_view({
        let seen = seen.clone();
        move |_window, cx| {
            gpui_kit::component::init(cx);
            crate::ui::theme::init(cx);
            Host { seen }
        }
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let hit = vtcx
        .debug_bounds("job-cancel-7")
        .expect("活跃任务行的取消钮")
        .center();
    vtcx.simulate_click(hit, gpui_kit::Modifiers::default());

    let seen = seen.borrow();
    assert_eq!(seen.len(), 1, "点一次取消应上行一次（实得 {seen:?}）");
    match seen.first() {
        Some(DialogEvent::JobsCancel(id)) => assert_eq!(*id, 7, "载荷应是本行 job_id"),
        other => panic!("应上行 JobsCancel(7)，实得 {other:?}"),
    }
}

/// 尺寸换算（拖动位移 → 新尺寸）：下限 / `MAX_H` / 视口留白三重钳制。
#[test]
fn resize_math_clamps_to_bounds_and_viewport() {
    let start = (420., 320.);
    // 正常位移：原样相加
    assert_eq!(resized_by(start, 60., 40., (1200., 800.)), (480., 360.));
    // 内侧收缩到负：收在下限（不得出现负尺寸）
    assert_eq!(
        resized_by(start, -10_000., -10_000., (1200., 800.)),
        (MIN_W, MIN_H)
    );
    // 高度上限：视口够大时取 `MAX_H`
    assert_eq!(resized_by(start, 0., 10_000., (1200., 2000.)).1, MAX_H);
    // 视口留白：宽度不得越过「视口宽 - 留白」
    assert_eq!(
        resized_by(start, 10_000., 0., (500., 800.)).0,
        500. - VIEWPORT_MARGIN_X
    );
    // 视口极小时上限退化到下限（`clamp` 的 min > max 不得 panic）
    assert_eq!(resized_by(start, 0., 0., (10., 10.)), (MIN_W, MIN_H));
}

/// 详情弹窗**可拖动右下抓手改尺寸**（用户口径 2026-10-09）；尺寸跨开窗记忆。
#[gpui_kit::test]
fn detail_modal_resizes_by_dragging_grip(cx: &mut gpui_kit::TestAppContext) {
    struct Host;
    impl gpui_kit::Render for Host {
        // 0.7.1：dialog 层由 kit `WindowState` 插件随 `Root::render` 自动挂，
        // 内容视图只交容器；参数不再被用，加下划线避免 `unused_variables`。
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            gpui_kit::div().size_full()
        }
    }

    let (_host, vtcx) = cx.add_window_view(|window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let host = cx.new(|_| Host);
        let view: gpui_kit::AnyView = host.into();
        gpui_kit::component::Root::new(view, window, cx)
    });

    vtcx.update(|window, cx| {
        crate::ui::dialogs::open_job_detail_dialog(window, cx, row());
        let _ = window.draw(cx);
    });
    let before = vtcx.debug_bounds("job-detail-body").expect("弹窗外框");
    let grip_box = vtcx.debug_bounds("job-detail-resize").expect("右下角抓手");
    // 抓手必须**落在外框内**（绝对定位贴角；若参与流式布局会被挤到外框之外）
    let body_right = before.origin.x + before.size.width;
    let body_bottom = before.origin.y + before.size.height;
    let grip_right = grip_box.origin.x + grip_box.size.width;
    let grip_bottom = grip_box.origin.y + grip_box.size.height;
    let near = |a: gpui_kit::Pixels, b: gpui_kit::Pixels| {
        a > b - gpui_kit::px(2.) && a < b + gpui_kit::px(2.)
    };
    assert!(
        grip_right <= body_right && grip_bottom <= body_bottom,
        "抓手须在外框内（抓手右下 {grip_right:?}/{grip_bottom:?}，外框右下 {body_right:?}/{body_bottom:?}）"
    );
    // 用户口径 2026-10-09：点阵/拖动面贴弹窗右下角，右侧与下方**不得再有内边距**
    //（有内边距就贴不到边框，鼠标到边框处不会变拖动光标）。
    assert!(
        near(grip_right, body_right) && near(grip_bottom, body_bottom),
        "拖动面须贴右下角（实得 {grip_right:?}/{grip_bottom:?} vs {body_right:?}/{body_bottom:?}）"
    );
    let grip = grip_box.center();

    // 真实拖动：按下（记基准点）→ 移动两步（跨过拖拽阈值）→ 抬起
    let mid = gpui_kit::point(grip.x + gpui_kit::px(30.), grip.y + gpui_kit::px(20.));
    let end = gpui_kit::point(grip.x + gpui_kit::px(60.), grip.y + gpui_kit::px(40.));
    vtcx.simulate_mouse_down(
        grip,
        gpui_kit::MouseButton::Left,
        gpui_kit::Modifiers::default(),
    );
    vtcx.simulate_mouse_move(
        mid,
        gpui_kit::MouseButton::Left,
        gpui_kit::Modifiers::default(),
    );
    vtcx.simulate_mouse_move(
        end,
        gpui_kit::MouseButton::Left,
        gpui_kit::Modifiers::default(),
    );
    vtcx.simulate_mouse_up(
        end,
        gpui_kit::MouseButton::Left,
        gpui_kit::Modifiers::default(),
    );
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let after = vtcx.debug_bounds("job-detail-body").expect("弹窗外框");
    let dw = after.size.width - before.size.width;
    let dh = after.size.height - before.size.height;
    assert!(
        dw > gpui_kit::px(58.) && dw < gpui_kit::px(62.),
        "宽度应随拖动增加 ~60px（实得 {dw:?}）"
    );
    assert!(
        dh > gpui_kit::px(38.) && dh < gpui_kit::px(42.),
        "高度应随拖动增加 ~40px（实得 {dh:?}）"
    );

    // 记忆：关掉再开，尺寸保持（进程内 `Global`）
    let sized = (after.size.width, after.size.height);
    vtcx.update(|window, cx| {
        window.close_dialog(cx);
        let _ = window.draw(cx);
    });
    vtcx.update(|window, cx| {
        crate::ui::dialogs::open_job_detail_dialog(window, cx, row());
        let _ = window.draw(cx);
    });
    let reopened = vtcx.debug_bounds("job-detail-body").expect("弹窗外框");
    assert_eq!(
        (reopened.size.width, reopened.size.height),
        sized,
        "尺寸应跨开窗记忆"
    );
}
