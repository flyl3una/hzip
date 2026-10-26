use super::clipboard_ops::{collect_paste_entries, expand_paste_sources};
use super::open_flow::IMAGE_VIEWER_EXTS;
use super::*;
use crate::ui::contracts::{AppStateUiSnapshot, JobPill, MainTab, ShellHandlers, ToolbarAction};
use crate::viewmodel::JobRow;
use gpui_kit::prelude::*;
use hzip_core::engine::{Engine, EngineConfig};
use std::sync::Arc;

/// 指定程序选择：图片扩展名 → image_viewer；其余 → editor；未配置 → 空。
///
/// 与 `open_selection_entry` 内的选择段同源（用户口径 2026-09-28：内置
/// 查看器停用后，双击一律走「指定程序 → 系统默认」）。
#[test]
fn custom_open_program_by_extension() {
    let pick = |iv: &str, ed: &str, name: &str| {
        let p = std::path::Path::new(name);
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let img = IMAGE_VIEWER_EXTS.contains(&ext.as_str());
        if img && !iv.is_empty() {
            iv.to_string()
        } else if !img && !ed.is_empty() {
            ed.to_string()
        } else {
            String::new()
        }
    };
    // 图片 → 查看器；文本/二进制 → 编辑器；未配置 → 系统默认
    assert_eq!(pick("C:/iv.exe", "C:/ed.exe", "a.png"), "C:/iv.exe");
    assert_eq!(pick("C:/iv.exe", "C:/ed.exe", "b.JPG"), "C:/iv.exe");
    assert_eq!(pick("C:/iv.exe", "C:/ed.exe", "c.txt"), "C:/ed.exe");
    assert_eq!(pick("C:/iv.exe", "C:/ed.exe", "d.bin"), "C:/ed.exe");
    assert_eq!(pick("", "", "a.png"), "");
    assert_eq!(pick("", "C:/ed.exe", "a.png"), "", "查看器未配置不借编辑器");
    // 无扩展名 → 非图片 → editor（若配置）
    assert_eq!(pick("C:/iv.exe", "C:/ed.exe", "noext"), "C:/ed.exe");
}

/// 允许测试调度器 park —— 本模块里**凡经 `run_open_flow` 起真实后台线程**
/// （`hzip-open`）的用例都要先调一次。
///
/// 为什么需要：那条线程完成时用 smol 通道唤醒任务，唤醒发生在**非测试线程**上；
/// gpui 的确定性测试调度器（`gpui_pre::executor::BackgroundExecutor`）会因此记一条
/// 「Detected activity on thread "hzip-open" … Your test is not deterministic」并在
/// **测试结束时 panic**。它是竞态：测试线程先 park（内部会开一次许可）就绿，后台线程
/// 先醒就红——所以负载高时偶发（实测同一用例连跑 1/2 次结果不同）。
/// `allow_parking()` 正是该调度器给「与真实 I/O 混跑」留的开关
/// （`gpui-pre-0.3.8/src/executor.rs:220` 的文档原文）。
fn allow_parking_for_real_io(vtcx: &mut gpui_kit::VisualTestContext) {
    vtcx.update(|_window, cx| cx.background_executor().allow_parking());
}

/// [`run_open_flow`] 的 `work` 必须跑在**另一个线程**上（用户口径 2026-09-24：
/// 「在后台新线程加载」）。跑在 UI 线程上 = 大档案/内层容器把界面冻住，正是
/// 这次要消灭的东西；这条断言就是那个契约的守门人。
#[gpui_kit::test]
fn run_open_flow_work_runs_off_ui_thread(cx: &mut gpui_kit::TestAppContext) {
    // 真实后台线程唤醒任务的许可（见 `allow_parking_for_real_io`）
    cx.update(|cx| cx.background_executor().allow_parking());
    let ui_thread = std::thread::current().id();
    let (tx, rx) = std::sync::mpsc::channel();
    cx.update(|cx| {
        let data: AppDataHandle = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
            Arc::new(crate::viewmodel::testutil::engine()),
        )));
        // 任务本身不驱动（测试里不跑前台执行器）：只要 `work` 已经在自己的
        // 线程上跑起来，就说明重活不依赖 UI 线程的调度——这正是本测试要证的。
        let _task = run_open_flow(
            cx,
            data.clone(),
            OpenFlow {
                label: "big.cpio.gz".into(),
                path: "F:/x/big.cpio.gz::init.cpio".into(),
                show_window: false,
                labels: crate::ui::views::open_progress::OpLabels::default(),
                epoch: 0,
                toast_on_error: false,
                // 门放行重跑走 true（跳过防闪窗延迟）；本测试只证「重活不在 UI 线程」
                immediate_show: true,
            },
            move |detail: &crate::ui::views::open_progress::OpenDetail| {
                // 启动相位：主线程在起线程前就写好了（窗口可能在工人线程首个
                // 相位之前渲染——`immediate_show` 时必然如此），别让窗口空白
                let initial = detail.phase();
                detail.set_phase("测试阶段".into());
                let _ = tx.send((std::thread::current().id(), initial, detail.phase()));
                Ok(())
            },
        );
    });
    let (worker, initial, phase) = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("work 必须在后台线程跑起来（10 s 内）");
    assert_ne!(worker, ui_thread, "work 不得跑在 UI 线程上（会冻界面）");
    // 启动相位：主线程在起线程前写好「准备中…」——窗口可能先于工人线程首个相位
    // 渲染（密码门放行后的重跑窗口立即弹出时必然如此），不能给空白窗
    let want = crate::tr("openprogress.phase_prepare");
    assert_eq!(
        initial.as_deref(),
        Some(want.as_str()),
        "打开流程启动即写启动相位"
    );
    assert_eq!(
        phase.as_deref(),
        Some("测试阶段"),
        "明细是同一个对象（工作线程写、渲染侧读）"
    );
}

fn snap(tab: Option<MainTab>, busy: bool, name: &str) -> AppStateUiSnapshot {
    AppStateUiSnapshot {
        tab,
        busy,
        archive_name: name.into(),
        ..Default::default()
    }
}

fn row(id: u64, kind: &'static str, state: crate::viewmodel::JobRowState) -> JobRow {
    JobRow {
        job_id: id,
        kind,
        state,
        progress: None,
        speed_bps: 0.0,
        eta: None,
        done_bytes: 0,
        total_bytes: 0,
        entries_done: 0,
        entries_total: 0,
        current_entry: None,
        final_line: None,
        recent_entries: std::collections::VecDeque::new(),
        failures: Vec::new(),
        finished: None,
        src: "demo.zip".into(),
        dest: None,
        format: "zip".into(),
        started_at: None,
        elapsed: None,
    }
}

// ShellState 迁移：快照应用（tab/busy/archive_name 变化被检测）
#[test]
fn shell_state_apply_detects_changes() {
    let mut st = ShellState::new();
    assert_eq!(st.tab, MainTab::Browse);
    assert!(
        st.apply(&snap(None, true, "demo.zip")),
        "busy+name 变化应命中"
    );
    assert!(st.busy);
    assert_eq!(st.archive_name, "demo.zip");
    // 同快照重复应用 → 无变化
    assert!(!st.apply(&snap(None, true, "demo.zip")));
    // 显式 tab 切换（任务中心可见时——默认 `show_jobs=false` 下切 Jobs
    // 会被修回 Browse，见 `jobs_tab_requires_show_jobs`）
    let mut s = snap(Some(MainTab::Jobs), true, "demo.zip");
    s.show_jobs = true;
    assert!(st.apply(&s));
    assert_eq!(st.tab, MainTab::Jobs);
}

// 任务中心关闭（ui.show_jobs=false，默认）→ 当前页签修回浏览：
// 页签不存在，停留会让内容分支落在隐藏页上。
#[test]
fn jobs_tab_requires_show_jobs() {
    let mut st = ShellState::new();
    // 关闭态：快照要求 Jobs → 修回 Browse（变化仍上报，供 UI 重绘）
    assert!(st.apply(&snap(Some(MainTab::Jobs), false, "")));
    assert_eq!(st.tab, MainTab::Browse, "关闭时 Jobs 页签不可达");
    // 打开态：Jobs 可达
    let mut s = snap(Some(MainTab::Jobs), false, "");
    s.show_jobs = true;
    st.apply(&s);
    assert_eq!(st.tab, MainTab::Jobs);
    // 运行中把任务中心关掉 → 回浏览
    let mut off = snap(Some(MainTab::Jobs), false, "");
    off.show_jobs = false;
    st.apply(&off);
    assert_eq!(st.tab, MainTab::Browse, "关掉任务中心后回浏览页");
    assert!(!st.show_jobs);
}

// 页签栏渲染口径（2026-09-23 改为**派生**）：无档案 + 未开任务中心/设置 → 不
// 渲染；打开档案 → 出「浏览」页签；`show_jobs` → 追加「任务中心」页签。
#[gpui_kit::test]
fn tab_strip_follows_present_tabs(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::ShellHandlers;
    use crate::ui::shell::AppShell;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let data_w = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(cx, ShellHandlers::default(), Some(data_w.clone()), None);
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    // 刚启动（无档案 + show_jobs 关 + 未开设置）：一个页签都没有
    shell.update(vtcx, |s, cx| {
        s.update_from_state(cx, &snap(None, false, ""));
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("main-tabs-wrap").is_none(),
        "无任何页签时不应渲染页签栏"
    );

    // 打开档案 → 「浏览」页签出现（关闭钮锚点在 = 页签存在）
    shell.update(vtcx, |s, cx| {
        s.update_from_state(cx, &snap(None, false, "demo.zip"));
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("main-tabs-wrap").is_some(),
        "有档案时页签栏必须渲染"
    );
    assert!(
        vtcx.debug_bounds("tab-close-browse").is_some(),
        "打开档案应出现「浏览」页签"
    );
    assert!(
        vtcx.debug_bounds("tab-close-jobs").is_none(),
        "任务中心未开时不该有它的页签"
    );

    // 打开任务中心 → 追加「任务中心」页签
    shell.update(vtcx, |s, cx| {
        let mut s2 = snap(None, false, "demo.zip");
        s2.show_jobs = true;
        s.update_from_state(cx, &s2);
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("tab-close-jobs").is_some(),
        "任务中心打开时应出现其页签"
    );

    // 版式契约（用户口径）：页签与工具按钮**同一行**，且页签在按钮**右侧**
    // ——把页签挪回独立一行 / 挪到左边，这两条立刻红。
    let btn = vtcx.debug_bounds("tb-open").expect("工具栏按钮锚点");
    let tab = vtcx.debug_bounds("tab-close-browse").expect("浏览页签锚点");
    let (btn_top, btn_bottom) = (
        f32::from(btn.origin.y),
        f32::from(btn.origin.y + btn.size.height),
    );
    let (tab_top, tab_bottom) = (
        f32::from(tab.origin.y),
        f32::from(tab.origin.y + tab.size.height),
    );
    assert!(
        tab_top < btn_bottom && btn_top < tab_bottom,
        "页签必须与工具按钮同行：按钮 y[{btn_top:.1},{btn_bottom:.1}] 页签 y[{tab_top:.1},{tab_bottom:.1}]"
    );
    assert!(
        f32::from(tab.origin.x) > f32::from(btn.origin.x + btn.size.width),
        "页签应在工具按钮右侧：按钮右缘 {:.1} 页签左缘 {:.1}",
        f32::from(btn.origin.x + btn.size.width),
        f32::from(tab.origin.x)
    );
}

/// 用户报的缺陷（2026-09-23）：**没有打开档案**时，仅在设置里打开「显示
/// 任务中心」，任务中心页签就该出现在页签栏里（它是「开关」语义的页签，
/// 不依赖档案在场）。此前 `present_tabs` 的 Jobs 分支虽不依赖档案，但
/// 用例只覆盖了「有档案 + show_jobs」的组合，无档案这一格是空白 ⇒ 回归
/// 无从发现。
#[gpui_kit::test]
fn jobs_tab_shows_without_archive(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::ShellHandlers;
    use crate::ui::shell::AppShell;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let data_w = data.clone();
    let slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(cx, ShellHandlers::default(), Some(data_w.clone()), None);
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = slot.borrow().clone().expect("shell 实体");

    // 设置页里打开「显示任务中心」：走面板同款数据面链路
    data.lock().expect("锁").apply_setting(
        crate::viewmodel::SettingId::ShowJobs,
        &crate::app::settings_registry::SettingValue::Bool(true),
    );
    let snap_now = data.lock().expect("锁").export_snapshot();
    assert!(snap_now.show_jobs, "数据面：设置项应已置真");
    assert!(snap_now.archive_name.is_empty(), "前置：没有打开档案");
    shell.update(vtcx, |s, cx| s.update_from_state(cx, &snap_now));
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("main-tabs-wrap").is_some(),
        "无档案但开了任务中心：页签栏必须渲染"
    );
    assert!(
        vtcx.debug_bounds("tab-close-jobs").is_some(),
        "无档案时任务中心页签必须可见"
    );
    assert!(
        vtcx.debug_bounds("tab-close-browse").is_none(),
        "没有档案就不该有浏览页签"
    );
}

/// 打开档案后应**聚焦浏览页**（用户报的缺陷 2026-09-23）：此前若用户先在
/// 任务中心/设置页，打开压缩包后页签停在原处，看不到刚打开的内容。
#[test]
fn opening_archive_focuses_browse_tab() {
    let mut st = ShellState::new();
    // `show_jobs` 的事实源是快照（`apply` 逐帧收），故每个快照都带上 true
    let with_jobs = |tab: Option<MainTab>, name: &str| {
        let mut s = snap(tab, false, name);
        s.show_jobs = true;
        s
    };
    st.apply(&with_jobs(Some(MainTab::Jobs), ""));
    assert_eq!(st.tab, MainTab::Jobs, "前置：停在任务中心");
    // 打开档案（名字空 → 非空）= 打开事件
    st.apply(&with_jobs(None, "demo.zip"));
    assert_eq!(st.tab, MainTab::Browse, "打开档案应聚焦浏览页");
    // 之后用户手动切回任务中心，快照逐帧不变也不得被拽回
    st.apply(&with_jobs(Some(MainTab::Jobs), "demo.zip"));
    assert_eq!(st.tab, MainTab::Jobs);
    for _ in 0..3 {
        st.apply(&with_jobs(None, "demo.zip"));
        assert_eq!(st.tab, MainTab::Jobs, "同一档案的快照不得反复复位页签");
    }
    // 换另一个档案 → 再次聚焦浏览
    st.apply(&with_jobs(None, "other.zip"));
    assert_eq!(st.tab, MainTab::Browse, "换档案同样聚焦浏览页");
}

/// 主窗口页签栏随布局密度（用户要求「tabs 的 tab 栏也要一起调整间隔」）。
///
/// 页签栏走 `gpui-base` 无样式原语（`ui/bar/tab_bar.rs`），行高/内边距/字号与
/// 工具按钮同一套映射（`gpui-component` `button.rs`：Medium `h_8`=32px、
/// Small `h_6`=24px），间距也随档收（Medium 16 → Small 12）。断言面 = 页签栏整条
/// 的高度（`main-tabs-wrap` 包着页签行，其高度即页签栏高）。
#[gpui_kit::test]
fn main_tab_bar_follows_density(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::ShellHandlers;
    use crate::ui::shell::AppShell;
    use crate::viewmodel::density::Density;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let data_w = data.clone();
    let slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(cx, ShellHandlers::default(), Some(data_w.clone()), None);
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = slot.borrow().clone().expect("shell 实体");

    let mut bars = Vec::new();
    for density in [Density::Compact, Density::Standard, Density::Loose] {
        crate::ui::spacing::set_density(density);
        vtcx.update(|window, cx| {
            let mut s = snap(None, false, "demo.zip");
            s.show_jobs = true;
            shell.update(cx, |shell, cx| shell.update_from_state(cx, &s));
            let _ = window.draw(cx);
        });
        let b = vtcx
            .debug_bounds("main-tabs-wrap")
            .expect("任务中心开启时页签栏必须渲染");
        bars.push(f32::from(b.size.height));
    }
    crate::ui::spacing::set_density(Density::Standard);

    let (cmp_h, std_h, los_h) = (bars[0], bars[1], bars[2]);
    assert!(
        cmp_h < std_h,
        "紧凑档页签栏应更矮（Size::Small：行高 32→24px）：{cmp_h} vs {std_h}"
    );
    assert!(
        (los_h - std_h).abs() < 0.51,
        "宽松档不放大控件固有尺寸（避免字级跳档）：{los_h} vs {std_h}"
    );
}

// ShellState 迁移：失败任务计数 → 任务页签徽标
#[test]
fn shell_state_jobs_badge_counts_failures() {
    let mut st = ShellState::new();
    let mut s = snap(None, false, "");
    s.job_rows = std::sync::Arc::new(vec![
        row(1, "extract", crate::viewmodel::JobRowState::Failed),
        row(2, "extract", crate::viewmodel::JobRowState::Done),
    ]);
    st.apply(&s);
    assert_eq!(st.jobs_badge, 1, "仅 Failed 行计入徽标");
}

// 快照往返：`apply` 接收的字段必须在 `to_snapshot` 回投（视图渲染输入）。
//
// 防「字段被丢弃」整类缺陷——`show_dotdot`/`columns`/`current_dir` 三个字段
// 均曾因缺 `ShellState` 字段或漏投影而恒为默认值，表现为「设置改完没用」
// 「`..` 跳根」「左树不跟随」。新增视图消费字段时必须同时改这两处。
#[test]
fn snapshot_round_trips_through_shell_state() {
    let mut st = ShellState::new();
    let columns = crate::ui::contracts::ColumnsConfig {
        order: {
            let mut o = crate::ui::contracts::BrowseColumnId::ALL.to_vec();
            o.rotate_left(3);
            o
        },
        hidden: vec![crate::ui::contracts::BrowseColumnId::Encoding],
    };
    let full = AppStateUiSnapshot {
        tab: Some(MainTab::Jobs),
        busy: true,
        busy_text: "打开中…".into(),
        archive_name: "demo.zip".into(),
        archive_path: "E:/a/demo.zip".into(),
        current_dir: "docs/src".into(),
        crumbs: crate::ui::contracts::crumbs_from_current_dir("demo.zip", "docs/src"),
        nav_rows: std::sync::Arc::new(vec![tree_nav_row("docs/src")]),
        dir_rows: std::sync::Arc::new(vec![tree_nav_row("docs/src/a.txt")]),
        entry_count: 42,
        selection_count: 3,
        selection: vec![0, 2],
        sel_stats: crate::ui::contracts::SelectionStats {
            files: 2,
            dirs: 1,
            size_raw: 4096,
            size_packed: 2048,
        },
        suspect_count: 1,
        job_rows: std::sync::Arc::new(vec![row(
            7,
            "extract",
            crate::viewmodel::JobRowState::Failed,
        )]),
        job_pill: Some(JobPill {
            job_id: 7,
            summary: "解压 demo.zip · 1/2 条".into(),
            progress: 0.5,
        }),
        show_dotdot: true,
        show_tree: true,
        show_jobs: true,
        toolbar_style_ix: 1,
        view_style_ix: 2,
        columns: columns.clone(),
        // 手动写回：挂起条数（过滤框左侧「保存 (n)」按钮的显隐依据）
        pending_count: 3,
        ..Default::default()
    };
    // 两趟：本快照**同时**携带「档案名从空变为 demo.zip」与 `tab=Jobs`——
    // 前者是「打开档案 → 聚焦浏览页」的触发条件（用户口径 2026-09-24），故第一
    // 趟按规则落在浏览页；第二趟档案名不变，`tab` 载荷才如常生效。
    // （真实链路里 `export_snapshot` 恒投 `tab: None`——页签归 ShellState 独占，
    //  这个组合只在本用例里出现，用于逐一验证字段往返。）
    assert!(st.apply(&full), "全字段变更应命中");
    assert!(st.apply(&full), "第二趟：档案名未变 → tab 载荷生效");
    let out = st.to_snapshot();

    // 视图实际消费的字段（逐项 = 一次历史断链）
    assert_eq!(out.tab, Some(MainTab::Jobs));
    assert!(out.busy);
    assert_eq!(out.busy_text, "打开中…");
    assert_eq!(out.archive_name, "demo.zip");
    assert_eq!(out.archive_path, "E:/a/demo.zip");
    assert_eq!(out.current_dir, "docs/src", "`..` 目标/左树跟进的推导源");
    assert_eq!(out.crumbs.len(), 3, "面包屑段");
    assert_eq!(out.nav_rows.len(), 1);
    assert_eq!(out.dir_rows.len(), 1);
    assert_eq!(out.job_pill.as_ref().map(|p| p.job_id), Some(7));
    assert_eq!(
        out.pending_count, 3,
        "挂起条数往返（漏投 ⇒ 保存按钮永不出现）"
    );
    assert_eq!(
        out.selection,
        vec![0, 2],
        "选中 ID 集往返（T-PERF2 断链修复：图标/列表高亮数据源）"
    );
    assert_eq!(
        out.sel_stats,
        crate::ui::contracts::SelectionStats {
            files: 2,
            dirs: 1,
            size_raw: 4096,
            size_packed: 2048
        },
        "选中统计往返（T-SEL2 状态栏数据源）"
    );
    assert!(out.show_tree, "树栏显隐往返");
    assert_eq!(out.toolbar_style_ix, 1, "工具栏三态往返");
    assert_eq!(out.view_style_ix, 2, "视图样式往返");

    assert!(out.show_dotdot, "`..` 行显隐");
    assert_eq!(out.columns, columns, "浏览列序/显隐");

    // 同快照重复应用 → 无变化（变化检测覆盖新字段）
    assert!(!st.apply(&full), "重复应用不应报变化");
    // 单字段变更逐个命中（防「纳入迁移但漏进变化检测」）
    let mut only_dotdot = full.clone();
    only_dotdot.show_dotdot = false;
    let mut probe = ShellState::new();
    probe.apply(&full);
    assert!(probe.apply(&only_dotdot), "show_dotdot 变化须命中");
    let mut only_dir = full.clone();
    only_dir.current_dir = "docs".into();
    assert!(probe.apply(&only_dir), "current_dir 变化须命中");
    let mut only_cols = full.clone();
    only_cols.columns.hidden.clear();
    assert!(probe.apply(&only_cols), "columns 变化须命中");
    // T-PERF2 新增检测面：selection（高亮数据源）/ busy_text——
    // notify 门控后漏检字段 = 界面静默不刷新
    let mut only_sel = full.clone();
    only_sel.selection = vec![1];
    assert!(probe.apply(&only_sel), "selection 变化须命中");
    // T-SEL2 新增检测面：sel_stats（状态栏选中统计——漏检 = 选择变化
    // 后状态栏静默不刷新）
    let mut only_stats = full.clone();
    only_stats.sel_stats.files = 3;
    assert!(probe.apply(&only_stats), "sel_stats 变化须命中");
    let mut only_busy_text = full.clone();
    only_busy_text.busy_text = "压缩中…".into();
    assert!(probe.apply(&only_busy_text), "busy_text 变化须命中");
}

fn tree_nav_row(path: &str) -> crate::viewmodel::rows::TreeRow {
    crate::viewmodel::rows::TreeRow {
        volume: None,
        path: path.into(),
        name: path.rsplit('/').next().unwrap_or(path).into(),
        depth: 0,
        is_dir: !path.ends_with(".txt"),
        entry_id: Some(1),
        expanded: false,
        has_children: true,
        is_container: false,
        encrypted: false,
        size_raw: 0,
        size_packed: 0,
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,

        container_format: None,
    }
}

// JobPill 可 Clone（快照派生面）
#[test]
fn job_pill_snapshot_clone() {
    let pill = JobPill {
        job_id: 7,
        summary: "解压 demo.zip · 12/408 条".into(),
        progress: 0.42,
    };
    let clone = pill.clone();
    assert_eq!(clone.job_id, 7);
    assert_eq!(clone.progress.to_bits(), 0.42f32.to_bits());
}

// R-GUI4 #3 实测复现：真实 AppShell（带 data/signal 通道，run_ui 同构）
// 上 dispatch_toolbar(Settings) → 设置对话框注册进 Root。
// 打开流程端到端（R-GUI2-06）：`open_archive_task` 走的是 argv `--open` /
// 工具栏 / 最近文件共用那条路——后台 browse 落账、会话装上、快照可导出；
// 随后 `open_container` 把内层压缩包当目录进（压栈换会话），`leave_container`
// 退层并清掉临时档案。
//
// 守的是本次两处结构改动的实际效果：① 重活移出 UI 线程后**账还是要落对**
// （会话/路径/嵌套标记）；② 双击容器真的能进能出（进不去 = 空目录，出不来 =
// 父层位置丢失，两者都是用户直接可感的坏体验）。
//
// 另守「最近打开」的两条用户口径（R-GUI4 后走查）：③ 任何入口打开档案都要
// 进最近记录——本用例走的正是 Explorer（argv `--open`）那条路；④ **档案内的
// 子压缩包不算一次打开**：进容器后记录里仍只有外层档案一条。
#[gpui_kit::test]
fn open_flow_installs_session_then_enters_and_leaves_container(cx: &mut gpui_kit::TestAppContext) {
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine,
    )));
    {
        let mut d = data.lock().expect("lock");
        d.signal_tx = Some(tx.clone());
        // 生产装配在 `run_ui` 的 `make_data` 里注入（嵌套打开要解内层档案）
        d.temp_area = Some(Arc::new(
            hzip_platform::temp_area::TempArea::new_session(None).expect("temp area"),
        ));
    }
    let data_for_window = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let app_shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_for_window.clone()),
            Some(tx.clone()),
        );
        let app_shell = cx.new(|_| app_shell);
        *slot2.borrow_mut() = Some(app_shell.clone());
        let view: gpui_kit::AnyView = app_shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    // 外层档案：子目录 + 内层压缩包（内层用真实 tar 字节嵌进去）
    let dir = tempfile::TempDir::new().expect("tempdir");
    let inner = dir.path().join("child.tar");
    crate::viewmodel::testutil::write_ustar(&inner, &[("deep/inner.txt", b"x")]);
    let inner_bytes = std::fs::read(&inner).expect("读内层");
    let outer = dir.path().join("outer.tar");
    crate::viewmodel::testutil::write_ustar(
        &outer,
        &[("sub/keep.txt", b"k"), ("sub/child.tar", &inner_bytes)],
    );

    // ① 打开（与 argv `--open` 同一条路；show_window=false 免测试里弹窗）
    allow_parking_for_real_io(vtcx);
    let task = vtcx.update(|_, cx| {
        crate::ui::shell::open_archive_task(cx, data.clone(), outer.clone(), false)
    });
    vtcx.update(|_, cx| {
        cx.spawn(async move |_| {
            let _ = task.await;
        })
        .detach();
    });
    let wait_open =
        |vtcx: &mut gpui_kit::VisualTestContext, what: &str, done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !done() {
                assert!(std::time::Instant::now() < deadline, "{what}：10s 内未完成");
                std::thread::sleep(std::time::Duration::from_millis(10));
                vtcx.run_until_parked();
            }
        };
    {
        let data_probe = data.clone();
        wait_open(vtcx, "打开外层档案", &|| {
            data_probe.lock().map(|d| d.main.is_open()).unwrap_or(false)
        });
    }
    let snap = data.lock().unwrap().export_snapshot();
    assert_eq!(snap.archive_name, "outer.tar");
    assert!(!snap.nested, "顶层会话不是嵌套会话");
    // ③ 外部入口（Explorer 双击/右键；本用例 = argv `--open` 那条路）打开的
    // 档案必须进「最近打开」——用户在菜单里要能直接再打开它。
    //
    // 只断言「置顶 + 去重」而非整表相等：列表启动时来自本机真实配置
    // （`AppData::new` 读 `ui.recent_archives`），整表比对会把开发机的历史
    // 记录写进断言。
    let outer_str = outer.to_string_lossy().into_owned();
    let mru_after_open = {
        let mru = data.lock().unwrap().recent_archives.clone();
        assert_eq!(
            mru.first().map(String::as_str),
            Some(outer_str.as_str()),
            "打开成功后该档案应置顶「最近打开」，实为 {mru:?}"
        );
        assert_eq!(
            mru.iter().filter(|p| **p == outer_str).count(),
            1,
            "同一次打开只应有一条记录（去重）"
        );
        mru
    };
    assert!(
        data.lock()
            .unwrap()
            .main
            .entry_id_of_path("sub/child.tar")
            .is_some(),
        "容器条目应可定位"
    );

    // 用户先进入 sub/（正常路径：右栏双击目录行），再双击其中的容器条目
    data.lock().unwrap().main.enter_dir("sub");

    // ② 双击容器 → 进入下一层会话（压栈）
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.open_container("sub/child.tar", cx));
    });
    {
        let data_probe = data.clone();
        wait_open(vtcx, "进入容器", &|| {
            data_probe
                .lock()
                .map(|d| d.export_snapshot().nested)
                .unwrap_or(false)
        });
    }
    let snap = data.lock().unwrap().export_snapshot();
    assert_eq!(snap.archive_name, "child.tar", "标题/根段 = 内层档案");
    // ④ 子压缩包不算一次「打开档案」：它只是会话压栈，最近记录**逐字不变**。
    assert_eq!(
        data.lock().unwrap().recent_archives,
        mru_after_open,
        "进入档案内的子压缩包不应写进「最近打开」"
    );
    assert_eq!(
        snap.crumbs
            .iter()
            .map(|c| (c.label.as_str(), c.level))
            .collect::<Vec<_>>(),
        vec![("outer.tar", 1), ("sub", 1), ("child.tar", 0)],
        "面包屑 = 外层 › 内层所在目录 › 内层档案"
    );

    // ③ 退层 → 回到外层会话的同一位置，临时档案清掉
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.leave_container(cx));
    });
    let d = data.lock().unwrap();
    let snap = d.export_snapshot();
    assert_eq!(snap.archive_name, "outer.tar", "退层后回到外层档案");
    assert_eq!(d.main.current_dir(), "sub", "父层位置必须保留");
    assert!(!snap.nested);
    drop(d);
    let temps: Vec<_> = std::fs::read_dir(
        data.lock()
            .unwrap()
            .temp_area
            .as_ref()
            .expect("temp area")
            .session_dir(),
    )
    .map(|it| it.filter_map(|e| e.ok()).map(|e| e.path()).collect())
    .unwrap_or_default();
    assert!(
        temps.iter().all(|p| !p.ends_with("child.tar")),
        "退层应清掉内层临时档案，实存: {temps:?}"
    );
}

/// 组合档（`.tar.gz`/`.tgz`）**退回外层再进**：进到的必须还是那一层（用户实测
/// 2026-10-14，测试包 `bh_auto_ops.itr2.x86_64.tgz`）。
///
/// 复现路径：① 打开 `.tgz`（延迟展开 = 单行内层容器 `….tar`）→ ② 双击那一行
/// 进内层 tar 会话 → ③ 从内层 `..` 退回外层 → ④ **再双击那一行**。
///
/// 修前：② 把外层帧的浏览器摘走后**再没放回**（`is_open()` 为假、
/// `deferred_layer()` 为 `None`），④ 于是不走延迟展开、走「按条目物化」兜底——
/// 合成行的 `id = 0` 撞上重开 reader 的**内层容器条目空间**，解出来的是那个 tar
/// 的**首个条目**（用户包里正是 `ops.tgz`）⇒ 父层直接显示子压缩包的 `ops/`
/// 层级，父层自己的结构反倒看不见也进不去。
///
/// 断言面：③ 后外层仍是「有浏览态 + 延迟层」；④ 进到的是那一层**自身**的内容
/// （`mid.tar` + `keep.txt`），而不是它某个条目的内容（`ops/`）。
#[gpui_kit::test]
fn composite_parent_reenter_keeps_its_own_layer(cx: &mut gpui_kit::TestAppContext) {
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine,
    )));
    {
        let mut d = data.lock().expect("lock");
        d.signal_tx = Some(tx.clone());
        d.temp_area = Some(Arc::new(
            hzip_platform::temp_area::TempArea::new_session(None).expect("temp area"),
        ));
    }
    let data_for_window = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let app_shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_for_window.clone()),
            Some(tx.clone()),
        );
        let app_shell = cx.new(|_| app_shell);
        *slot2.borrow_mut() = Some(app_shell.clone());
        let view: gpui_kit::AnyView = app_shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");
    let wait = |vtcx: &mut gpui_kit::VisualTestContext, what: &str, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}：10s 内未完成");
            std::thread::sleep(std::time::Duration::from_millis(10));
            vtcx.run_until_parked();
        }
    };
    let gz = |raw: &[u8]| -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, raw).expect("gzip 写");
        enc.finish().expect("gzip 收尾")
    };
    let row_names = |d: &crate::app::state::AppData| -> Vec<String> {
        let mut names: Vec<String> = d.dir_rows.iter().map(|r| r.name.to_string()).collect();
        names.sort();
        names
    };
    // 夹具形状照抄用户包：`outer.tar.gz` = gzip(tar)，tar 的**首个条目**是内层
    // 压缩包（`mid.tar` 里是 `ops/x.txt`）——首条目正是修前被误当成那一行的东西。
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mid = dir.path().join("mid.tar");
    crate::viewmodel::testutil::write_ustar(&mid, &[("ops/x.txt", b"x")]);
    let mid_bytes = std::fs::read(&mid).expect("读 mid");
    let outer_tar = dir.path().join("outer.tar");
    crate::viewmodel::testutil::write_ustar(
        &outer_tar,
        &[("mid.tar", &mid_bytes), ("keep.txt", b"k")],
    );
    let outer = dir.path().join("outer.tar.gz");
    std::fs::write(
        &outer,
        gz(&std::fs::read(&outer_tar).expect("读 outer.tar")),
    )
    .expect("写 .tar.gz");

    // ① 打开外层（`--open` 那条路；组合档延迟展开 → 单行 `outer.tar`）
    allow_parking_for_real_io(vtcx);
    let task = vtcx.update(|_, cx| {
        crate::ui::shell::open_archive_task(cx, data.clone(), outer.clone(), false)
    });
    vtcx.update(|_, cx| {
        cx.spawn(async move |_| {
            let _ = task.await;
        })
        .detach();
    });
    {
        let probe = data.clone();
        wait(vtcx, "打开外层档案", &|| {
            probe.lock().map(|d| d.main.is_open()).unwrap_or(false)
        });
    }
    vtcx.run_until_parked();
    {
        let mut d = data.lock().unwrap();
        d.rebuild_browse_rows();
        assert_eq!(
            row_names(&d),
            vec!["outer.tar".to_string()],
            "延迟展开：列表只有那一行内层容器"
        );
        assert_eq!(d.main.deferred_layer(), Some("outer.tar"));
    }

    // ② 双击那一行 → 进内层 tar 会话（外层帧压栈）
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.open_container("outer.tar", cx));
    });
    {
        let probe = data.clone();
        wait(vtcx, "进入延迟层", &|| {
            probe
                .lock()
                .map(|d| d.export_snapshot().nested)
                .unwrap_or(false)
        });
    }
    {
        let mut d = data.lock().unwrap();
        d.rebuild_browse_rows();
        assert_eq!(
            row_names(&d),
            vec!["keep.txt".to_string(), "mid.tar".to_string()],
            "内层会话 = 这个 tar 自己的条目"
        );
        // 就地展开出的这一层没有自己的文件 —— 面包屑根必须显示**这一层**的名字
        // （`outer.tar.gz › outer.tar`），不是把外层档案名重复一遍
        let snap = d.export_snapshot();
        assert_eq!(snap.archive_name, "outer.tar");
        assert_eq!(
            snap.crumbs
                .iter()
                .map(|c| (c.label.as_str(), c.level))
                .collect::<Vec<_>>(),
            vec![("outer.tar.gz", 1), ("outer.tar", 0)],
            "面包屑 = 外层组合档 › 内层容器（与「双击内层压缩包」同构）"
        );
    }

    // ③ 退回外层：外层帧必须是**活的**（有浏览态 + 仍是延迟层）
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.leave_container(cx));
    });
    vtcx.run_until_parked();
    {
        let mut d = data.lock().unwrap();
        assert!(!d.export_snapshot().nested, "已退回外层");
        assert!(
            d.main.is_open(),
            "退回后外层必须有浏览态（修前：浏览器被摘走再没放回）"
        );
        assert_eq!(
            d.main.deferred_layer(),
            Some("outer.tar"),
            "退回后外层仍停在延迟层（同一行要能再进）"
        );
        d.rebuild_browse_rows();
        assert_eq!(row_names(&d), vec!["outer.tar".to_string()]);
    }

    // ④ 再双击那一行：必须进到「这一层」，而不是它某个条目的内容
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.open_container("outer.tar", cx));
    });
    {
        let probe = data.clone();
        wait(vtcx, "再次进入延迟层", &|| {
            probe
                .lock()
                .map(|d| d.export_snapshot().nested)
                .unwrap_or(false)
        });
    }
    {
        let mut d = data.lock().unwrap();
        d.rebuild_browse_rows();
        let names = row_names(&d);
        assert_eq!(
            names,
            vec!["keep.txt".to_string(), "mid.tar".to_string()],
            "再进 = 那一层自身的内容（修前：解出首条目 mid.tar 的内容 = `ops/`）"
        );
        assert!(
            !d.dir_rows.iter().any(|r| r.path == "ops"),
            "父层绝不该显示子压缩包内部的 `ops/`"
        );
    }
}

/// 工具栏/菜单「设置」→ **设置页签**（2026-09-23：设置由弹层改为页签）。
///
/// 断言面：① 页签打开态 + 当前页签 = Settings + 面板实体已装配；② **不再
/// 注册对话框**（弹层路径已删——这条守的就是「少一个弹窗」这个用户口径）；
/// ③ 重复点幂等（实体复用）；④ 关闭页签丢实体。
/// 设置面板里打开「显示任务中心」→ **不等任何快照/信号**，页签集立刻多出任务中心。
///
/// 用户实测缺陷（2026-09-24）：「在设置打开了任务中心，但还没打开文件的情况下，
/// 任务中心的 Tab 页没有显示出来」——`show_jobs` 平时只靠异步信号链
/// （AppData → 泵批次 → 快照 → `ShellState.apply`）回流，而面板自己的重画不带
/// 页签。修法：`dispatch_toolbar(Settings)` 装配面板时挂实体订阅，面板一 notify
/// （设置项变更即 notify）就把该值从 AppData 同步进 `ShellState`。
///
/// 本用例**刻意不发任何信号、不喂任何快照**——正是缺陷的复现条件。
#[gpui_kit::test]
fn settings_panel_event_syncs_jobs_tab_without_snapshot(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::settings_registry::SettingValue;
    use crate::ui::contracts::DialogEvent;
    use crate::viewmodel::SettingId;

    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine.clone(),
    )));
    if let Ok(mut d) = data.lock() {
        d.signal_tx = Some(tx.clone());
    }
    let data_for_window = data.clone();
    let tx_for_window = tx.clone();
    let shell_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let app_shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_for_window.clone()),
            Some(tx_for_window.clone()),
        );
        let app_shell = cx.new(|_| app_shell);
        app_shell.update(cx, |shell, cx| shell.observe_state(cx));
        *slot2.borrow_mut() = Some(app_shell.clone());
        let view: gpui_kit::AnyView = app_shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    // 前置：没有档案、任务中心关着
    let (show_jobs, tabs) = vtcx.update(|_, cx| {
        let st = shell.read(cx).state.read(cx);
        (st.show_jobs, st.present_tabs())
    });
    assert!(!show_jobs && tabs.is_empty(), "前置：无档案 + 无页签");

    // 打开设置页签（惰建面板 + 挂订阅）
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, ToolbarAction::Settings)
        });
    });

    // 面板上打开「显示任务中心」= Switch 同款两步：①事件落 AppData ②面板重画
    data.lock()
        .expect("锁")
        .consume_dialog_event(&DialogEvent::SettingChanged {
            id: SettingId::ShowJobs,
            value: SettingValue::Bool(true),
        });
    assert!(
        data.lock().expect("锁").settings.show_jobs,
        "前置：数据面已置真"
    );
    vtcx.update(|_, cx| crate::ui::views::settings_layout::reload_open_panel(cx));

    let (show_jobs, tabs) = vtcx.update(|_, cx| {
        let st = shell.read(cx).state.read(cx);
        (st.show_jobs, st.present_tabs())
    });
    assert!(show_jobs, "面板事件应即时同步 show_jobs（本地先行）");
    assert!(
        tabs.contains(&MainTab::Jobs),
        "无档案也应出现任务中心页签：{tabs:?}"
    );
    assert!(
        !tabs.contains(&MainTab::Browse),
        "没有档案不该有浏览页签：{tabs:?}"
    );

    // 关闭设置页签 → 退订（面板实体丢弃）
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.close_tab(cx, MainTab::Settings));
    });
    assert!(
        vtcx.update(|_, cx| shell.read(cx)._observe_settings_panel.is_none()),
        "关掉设置页签应同时退订"
    );
}

#[gpui_kit::test]
fn dispatch_settings_opens_tab_on_real_shell(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::WindowExt as _;
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine.clone(),
    )));
    if let Ok(mut d) = data.lock() {
        d.signal_tx = Some(tx.clone());
    }
    let data_for_window = data.clone();
    let tx_for_window = tx.clone();
    let shell_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let app_shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_for_window.clone()),
            Some(tx_for_window.clone()),
        );
        let app_shell = cx.new(|_| app_shell);
        app_shell.update(cx, |shell, cx| shell.observe_state(cx));
        *slot2.borrow_mut() = Some(app_shell.clone());
        let view: gpui_kit::AnyView = app_shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, ToolbarAction::Settings)
        });
    });
    let (open, tab, has_panel) = vtcx.update(|_, cx| {
        let s = shell.read(cx);
        (
            s.state.read(cx).settings_open,
            s.state.read(cx).tab,
            s.settings_panel.is_some(),
        )
    });
    assert!(open, "设置页签应打开");
    assert_eq!(tab, MainTab::Settings, "应切到设置页签");
    assert!(has_panel, "设置面板实体应已装配（页签内容）");
    let has_dialog = vtcx.update(|window, cx| window.has_active_dialog(cx));
    assert!(!has_dialog, "设置不应再注册弹层（已改为页签）");
    // 幂等：再点一次只切页签，不重建实体
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, ToolbarAction::Settings)
        });
    });
    assert!(
        vtcx.update(|_, cx| shell.read(cx).settings_panel.is_some()),
        "重复点「设置」应复用已有面板实体"
    );
    // 关闭页签 → 页签消失 + 实体丢弃（下次开重建）
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.close_tab(cx, MainTab::Settings));
    });
    let (open, has_panel) = vtcx.update(|_, cx| {
        let s = shell.read(cx);
        (s.state.read(cx).settings_open, s.settings_panel.is_some())
    });
    assert!(!open, "关闭后设置页签应消失");
    assert!(!has_panel, "关闭后面板实体应丢弃");
}

/// 关掉「文件浏览」页签后，档案类工具栏动作（解压/压缩/预览/刷新）必须把
/// 浏览页拉回前台——否则关掉页签后没有别的路回到档案页（只有再开一次档案）。
#[gpui_kit::test]
fn archive_actions_restore_closed_browse_tab(cx: &mut gpui_kit::TestAppContext) {
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine.clone(),
    )));
    if let Ok(mut d) = data.lock() {
        d.signal_tx = Some(tx.clone());
    }
    let shell_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let data_w = data.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let s = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_w.clone()),
            Some(tx.clone()),
        );
        let s = cx.new(|_| s);
        *slot2.borrow_mut() = Some(s.clone());
        let view: gpui_kit::AnyView = s.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");
    // 打开档案 → 浏览页签出现；用户把它关掉（暂隐）
    shell.update(vtcx, |s, cx| {
        s.update_from_state(cx, &snap(None, false, "demo.zip"));
    });
    shell.update(vtcx, |s, cx| {
        s.close_tab(cx, MainTab::Browse);
    });
    assert!(
        vtcx.update(|_, cx| {
            let st = shell.read(cx);
            st.state.read(cx).browse_hidden && !st.state.read(cx).tab_present(MainTab::Browse)
        }),
        "关闭后浏览页签应暂隐"
    );
    // 档案类动作 → 回到浏览页
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, ToolbarAction::Refresh)
        });
    });
    let (hidden, tab) = vtcx.update(|_, cx| {
        let st = shell.read(cx);
        (st.state.read(cx).browse_hidden, st.state.read(cx).tab)
    });
    assert!(!hidden, "档案类动作应复位浏览页签");
    assert_eq!(tab, MainTab::Browse, "并切回浏览页");
}

/// 关掉页签后必须能**再触发回来**（用户实测缺陷 2026-09-23：关掉后再点
/// 文件浏览/任务中心/设置都不显示）。
///
/// 三条各自的「再触发」路径：设置 = 再点设置动作；浏览 = 再来一批带档案的
/// 快照（打开/重开档案都会）；任务中心 = 视图菜单勾选 `ui.show_jobs`。
#[gpui_kit::test]
fn closed_tabs_can_be_reopened(cx: &mut gpui_kit::TestAppContext) {
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine.clone(),
    )));
    if let Ok(mut d) = data.lock() {
        d.signal_tx = Some(tx.clone());
    }
    let shell_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let data_w = data.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let s = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_w.clone()),
            Some(tx.clone()),
        );
        let s = cx.new(|_| s);
        *slot2.borrow_mut() = Some(s.clone());
        let view: gpui_kit::AnyView = s.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    // —— 浏览页签：有档案即出现；关掉后新快照必须让它回来 ——
    shell.update(vtcx, |s, cx| {
        s.update_from_state(cx, &snap(None, false, "demo.zip"));
    });
    assert!(vtcx.update(|_, cx| shell.read(cx).state.read(cx).tab_present(MainTab::Browse)));
    shell.update(vtcx, |s, cx| {
        s.close_tab(cx, MainTab::Browse);
    });
    assert!(!vtcx.update(|_, cx| shell.read(cx).state.read(cx).tab_present(MainTab::Browse)));
    shell.update(vtcx, |s, cx| {
        s.update_from_state(cx, &snap(None, false, "demo.zip"));
    });
    assert!(
        vtcx.update(|_, cx| shell.read(cx).state.read(cx).tab_present(MainTab::Browse)),
        "重开/再触发档案后「浏览」页签必须回来"
    );

    // —— 任务中心页签：勾选开关即出现；关掉后再勾必须回来 ——
    let set_jobs = |on: bool, data: &crate::ui::shell::AppDataHandle| {
        let mut d = data.lock().expect("锁");
        d.apply_setting(
            crate::viewmodel::SettingId::ShowJobs,
            &crate::app::settings_registry::SettingValue::Bool(on),
        );
        d.export_snapshot()
    };
    let s = set_jobs(true, &data);
    shell.update(vtcx, |sh, cx| sh.update_from_state(cx, &s));
    assert!(vtcx.update(|_, cx| shell.read(cx).state.read(cx).tab_present(MainTab::Jobs)));
    shell.update(vtcx, |s, cx| {
        s.close_tab(cx, MainTab::Jobs);
    });
    assert!(!vtcx.update(|_, cx| shell.read(cx).state.read(cx).tab_present(MainTab::Jobs)));
    let s = set_jobs(true, &data);
    shell.update(vtcx, |sh, cx| sh.update_from_state(cx, &s));
    assert!(
        vtcx.update(|_, cx| shell.read(cx).state.read(cx).tab_present(MainTab::Jobs)),
        "再勾选「任务中心」后它的页签必须回来"
    );

    // —— 设置页签：动作即开；关掉后再点动作必须回来 ——
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, ToolbarAction::Settings)
        });
    });
    assert!(vtcx.update(|_, cx| shell.read(cx).state.read(cx).settings_open));
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.close_tab(cx, MainTab::Settings));
    });
    assert!(!vtcx.update(|_, cx| shell.read(cx).state.read(cx).settings_open));
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, ToolbarAction::Settings)
        });
    });
    assert!(
        vtcx.update(|_, cx| shell.read(cx).state.read(cx).settings_open),
        "再点「设置」后设置页签必须回来"
    );

    // —— 真实点击路径：点 × 关掉页签，再触发必须回来（用户就是点 × 关的）——
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let hit = vtcx
        .debug_bounds("tab-close-settings")
        .expect("设置页签关闭钮")
        .center();
    vtcx.simulate_click(hit, gpui_kit::Modifiers::default());
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        !vtcx.update(|_, cx| shell.read(cx).state.read(cx).settings_open),
        "点 × 应关掉设置页签"
    );
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, ToolbarAction::Settings)
        });
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("tab-close-settings").is_some(),
        "点 × 关掉后再点「设置」，页签必须重新显示出来"
    );
}

/// 用户实测缺陷（2026-09-23）：关掉页签后「再打开同一档案」页签不回来。
///
/// 根因 = 派生修态不在变化检测里：快照逐字不变 → `apply` 返回 false →
/// `update_from_state` 不 notify → 界面不重画。本用例把「同一份快照再来一次」
/// 钉死为**必须报变化**。
#[test]
fn apply_reports_tab_derivation_changes() {
    let mut st = ShellState::new();
    let snap_v = snap(None, false, "demo.zip");
    assert!(st.apply(&snap_v), "首次带档案的快照应报变化");
    assert!(st.tab_present(MainTab::Browse));
    // 用户点 × 关掉浏览页签（本地态，不进快照）
    st.browse_hidden = true;
    assert!(!st.tab_present(MainTab::Browse));
    assert!(
        st.apply(&snap_v),
        "同一档案的快照再来一次也必须报变化（browse_hidden 复位改变了渲染结果）"
    );
    assert!(
        st.tab_present(MainTab::Browse),
        "开/重开档案后浏览页签必须回到页签栏"
    );
}

/// 工具栏按钮集（用户口径 2026-09-23：移除刷新/测试/设置）+ 设置页留白。
///
/// 断言面都是真实渲染边界：按钮锚点在不在；设置面板相对其宿主是否四周内缩。
#[gpui_kit::test]
fn toolbar_trimmed_and_settings_page_padded(cx: &mut gpui_kit::TestAppContext) {
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine.clone(),
    )));
    if let Ok(mut d) = data.lock() {
        d.signal_tx = Some(tx.clone());
    }
    let shell_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let data_w = data.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let s = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_w.clone()),
            Some(tx.clone()),
        );
        let s = cx.new(|_| s);
        *slot2.borrow_mut() = Some(s.clone());
        let view: gpui_kit::AnyView = s.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, ToolbarAction::Settings)
        });
    });
    for _ in 0..2 {
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    // ① 移除的按钮不再渲染；保留的仍在（动作分支保留——菜单还走它）
    for gone in ["tb-refresh", "tb-test", "tb-settings"] {
        assert!(
            vtcx.debug_bounds(gone).is_none(),
            "工具栏不该再有 {gone}（用户要求移除）"
        );
    }
    // T-BAR（用户口径 2026-09-29）：七钮 + 三下拉；「解压全部」也移除了。
    for gone in ["tb-extract-all", "tb-compress"] {
        assert!(
            vtcx.debug_bounds(gone).is_none(),
            "工具栏不该再有 {gone}（用户要求移除/改语义）"
        );
    }
    for kept in [
        "tb-open",
        "tb-open-menu",
        "tb-extract",
        "tb-extract-menu",
        "tb-add",
        "tb-delete",
        "tb-preview",
        "tb-columns",
    ] {
        assert!(vtcx.debug_bounds(kept).is_some(), "工具栏应保留 {kept}");
    }

    // ② 设置页四周留白：面板根相对宿主四周内缩 ≥ padding
    let pad =
        vtcx.update(|window, _| f32::from(crate::ui::spacing::sp(4.).to_pixels(window.rem_size())));
    let host = vtcx.debug_bounds("settings-host").expect("设置宿主锚点");
    let panel = vtcx
        .debug_bounds("settings-panel-root")
        .expect("面板根锚点");
    let (l, t) = (
        f32::from(panel.origin.x) - f32::from(host.origin.x),
        f32::from(panel.origin.y) - f32::from(host.origin.y),
    );
    let (r, b) = (
        f32::from(host.origin.x + host.size.width) - f32::from(panel.origin.x + panel.size.width),
        f32::from(host.origin.y + host.size.height) - f32::from(panel.origin.y + panel.size.height),
    );
    for (side, got) in [("左", l), ("上", t), ("右", r), ("下", b)] {
        assert!(
            got >= pad - 1.01,
            "设置页{side}留白 {got:.1}px 小于 padding {pad:.1}px（用户要求四周留白）"
        );
    }
}

/// 用户实测缺陷（2026-09-23）：**频繁**关闭/打开三条页签后，菜单
/// 「选项 ▸ 设置…」不再打开设置页签。
///
/// 本用例按用户流程在**渲染帧**上跑多轮（动作派发走真实 `Settings` action —
/// 菜单项与工具栏按钮最终都汇到 `dispatch_toolbar`），每轮都断言设置页签的
/// 关闭钮锚点在/不在，最后再断言「关掉后还能被菜单打开」。
#[gpui_kit::test]
fn rapid_tab_cycles_keep_settings_openable(cx: &mut gpui_kit::TestAppContext) {
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine.clone(),
    )));
    if let Ok(mut d) = data.lock() {
        d.signal_tx = Some(tx.clone());
    }
    let shell_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let data_w = data.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let s = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_w.clone()),
            Some(tx.clone()),
        );
        let s = cx.new(|_| s);
        s.update(cx, |s, cx| s.observe_state(cx));
        *slot2.borrow_mut() = Some(s.clone());
        let view: gpui_kit::AnyView = s.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");
    let draw = |vtcx: &mut gpui_kit::VisualTestContext| {
        for _ in 0..2 {
            vtcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
    };
    // 真实动作派发（= 菜单项路径）：`Window::dispatch_action` 内部是 defer 的，
    // 必须再画一帧才落地。
    let dispatch_settings = |vtcx: &mut gpui_kit::VisualTestContext| {
        vtcx.update(|window, cx| {
            window.dispatch_action(Box::new(Settings), cx);
        });
        // `Window::dispatch_action` 内部 `cx.defer`：必须让效果队列跑完，
        // 否则测的是「没落地」而不是真实链路。
        vtcx.run_until_parked();
        draw(vtcx);
    };
    let set_jobs = |on: bool| {
        let mut d = data.lock().expect("锁");
        d.apply_setting(
            crate::viewmodel::SettingId::ShowJobs,
            &crate::app::settings_registry::SettingValue::Bool(on),
        );
        d.export_snapshot()
    };

    for round in 0..3 {
        // ① 打开档案（浏览页签）
        shell.update(vtcx, |s, cx| {
            s.update_from_state(cx, &snap(None, false, "demo.zip"));
        });
        draw(vtcx);
        // ② 菜单「设置…」→ 设置页签出现
        dispatch_settings(vtcx);
        assert!(
            vtcx.debug_bounds("tab-close-settings").is_some(),
            "第 {round} 轮：菜单设置应打开设置页签"
        );
        assert!(vtcx.update(|_, cx| shell.read(cx).state.read(cx).settings_open));
        // ③ 点 × 关掉设置（真实点击）
        let hit = vtcx
            .debug_bounds("tab-close-settings")
            .expect("设置关闭钮")
            .center();
        vtcx.simulate_click(hit, gpui_kit::Modifiers::default());
        draw(vtcx);
        assert!(
            vtcx.debug_bounds("tab-close-settings").is_none(),
            "第 {round} 轮：点 × 后设置页签必须消失"
        );
        // ④ 任务中心开/关一轮（页签集抖动）
        let s = set_jobs(true);
        shell.update(vtcx, |sh, cx| sh.update_from_state(cx, &s));
        draw(vtcx);
        let s = set_jobs(false);
        shell.update(vtcx, |sh, cx| sh.update_from_state(cx, &s));
        draw(vtcx);
    }

    // 收尾：模拟用户最后那一下「菜单 ▸ 设置」
    dispatch_settings(vtcx);
    assert!(
        vtcx.debug_bounds("tab-close-settings").is_some(),
        "频繁开关后，菜单设置仍必须能打开设置页签"
    );
    let (open, panel) = vtcx.update(|_, cx| {
        let s = shell.read(cx);
        (s.state.read(cx).settings_open, s.settings_panel.is_some())
    });
    assert!(open, "设置页签应处于打开态");
    assert!(
        panel,
        "设置面板实体应存在（关闭时必须已从内存移除、此处新建）"
    );
}

/// 页签集是**派生的**（`present_tabs`）：刚启动全空；开档案出浏览页签；
/// `show_jobs` 出任务中心；`settings_open` 出设置页签——顺序固定。
#[test]
fn present_tabs_derives_from_live_state() {
    let mut st = ShellState::new();
    assert!(st.present_tabs().is_empty(), "刚启动一个页签都没有");
    st.archive_name = "demo.zip".into();
    assert_eq!(st.present_tabs(), vec![MainTab::Browse]);
    assert!(st.tab_present(MainTab::Browse));
    st.show_jobs = true;
    assert_eq!(st.present_tabs(), vec![MainTab::Browse, MainTab::Jobs]);
    st.settings_open = true;
    assert_eq!(
        st.present_tabs(),
        vec![MainTab::Browse, MainTab::Jobs, MainTab::Settings]
    );
    // 用户关掉浏览页签（档案仍开着）→ 暂隐
    st.browse_hidden = true;
    assert_eq!(st.present_tabs(), vec![MainTab::Jobs, MainTab::Settings]);
    assert!(!st.tab_present(MainTab::Browse));
    // 新档案打开 → 浏览页签复位（否则「打开了却看不到」）
    let snap_v = snap(None, true, "other.zip");
    assert!(st.apply(&snap_v));
    assert!(!st.browse_hidden, "开新档案应复位浏览页签");
    assert!(st.tab_present(MainTab::Browse));
}

/// 当前页签被关（快照仍带旧值）时，`apply` 必须把它收到**存在**的页签上
/// ——否则内容分支会落在隐藏页。
#[test]
fn apply_clamps_tab_to_present_kinds() {
    let mut st = ShellState::new();
    st.archive_name = "demo.zip".into();
    st.tab = MainTab::Settings;
    // 设置页签已关（UI 本地态），而快照里的 tab 还是旧的 Settings
    st.settings_open = false;
    assert!(st.apply(&snap(Some(MainTab::Settings), false, "demo.zip")));
    assert_eq!(st.tab, MainTab::Browse, "不存在的页签必须回落到存在的页");
}

// R-GUI4：任务行流经快照 → ShellState（任务中心恒空回归锚点——旧实现
// ShellState 无 job_rows 字段，快照行落在 apply 的 Default 里被丢弃）
#[test]
fn shell_state_applies_job_rows() {
    let mut st = ShellState::new();
    let mut snap = snap(None, false, "demo.zip");
    snap.job_rows = std::sync::Arc::new(vec![
        row(1, "extract", crate::viewmodel::JobRowState::Running),
        row(2, "extract", crate::viewmodel::JobRowState::Done),
    ]);
    assert!(st.apply(&snap));
    assert_eq!(st.job_rows.len(), 2, "任务行必须落到 ShellState");
    // 变化检测：进度推进触发 apply 变化（T-PERF4：Arc 行集 → make_mut 改写）
    let mut snap2 = snap.clone();
    snap2.job_rows = {
        let mut rows = snap2.job_rows.as_ref().clone();
        rows[0].progress = Some(0.5);
        std::sync::Arc::new(rows)
    };
    assert!(st.apply(&snap2), "进度变化应命中变化检测");
    assert_eq!(st.jobs_badge, 0, "无失败任务不进徽标");
}

// 双击加密条目 → 密码门（用户口径 2026-09-28）：
// ① 加密条目 + 无缓存 → 武装 `OpenSelection` 门，快照投影密码弹窗；
// ② 提交**正确**密码 → 后台 `preflight_password` 通过 → 缓存 + 门放行
//    （动作进待执行槽 → 泵重跑时缓存命中，不再询问 = 「解密中」态）；
// ③ 提交**错误**密码 → attempts+1，门保持武装（可重问），且不落缓存。
#[gpui_kit::test]
fn encrypted_entry_open_goes_through_password_gate(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::{DialogEvent, UiDialogPrompt};
    use crate::viewmodel::GateAction;
    use hzip_format::{AesKey, CreateOptions, EncryptionRequest, EntryMeta, PasswordRef, Sink};

    let (tx, rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine.clone(),
    )));
    {
        let mut d = data.lock().expect("lock");
        d.signal_tx = Some(tx.clone());
    }
    let data_for_window = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let app_shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_for_window.clone()),
            Some(tx.clone()),
        );
        let app_shell = cx.new(|_| app_shell);
        *slot2.borrow_mut() = Some(app_shell.clone());
        let view: gpui_kit::AnyView = app_shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    // 加密档案：AES-256 单条目（写侧经 registry 的 zip handler）
    let dir = tempfile::TempDir::new().expect("tempdir");
    let zip = dir.path().join("enc.zip");
    {
        let pwz = zeroize::Zeroizing::new("pw-a".to_string());
        let handler = engine.registry().handler("zip").expect("zip handler");
        let mut w = handler
            .create_writer(
                Sink::File(zip.clone()),
                &CreateOptions {
                    encryption: Some(EncryptionRequest::Aes(AesKey::Bits256)),
                    ..Default::default()
                },
            )
            .expect("create writer");
        let meta = EntryMeta {
            name: "secret.txt".into(),
            size_raw: Some(11),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::new(&pwz)).expect("entry");
        std::io::Write::write_all(&mut s, b"gate secret").expect("write");
        s.finish().expect("finish entry");
        w.finish().expect("finish archive");
    }

    let wait_for = |vtcx: &mut gpui_kit::VisualTestContext, what: &str, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}：10s 内未完成");
            std::thread::sleep(std::time::Duration::from_millis(10));
            vtcx.run_until_parked();
        }
    };

    // 打开档案（与用户双击档案同一条路）
    allow_parking_for_real_io(vtcx);
    let task = vtcx
        .update(|_, cx| crate::ui::shell::open_archive_task(cx, data.clone(), zip.clone(), false));
    vtcx.update(|_, cx| {
        cx.spawn(async move |_| {
            let _ = task.await;
        })
        .detach();
    });
    {
        let probe = data.clone();
        wait_for(vtcx, "打开加密档案", &|| {
            probe.lock().map(|d| d.main.is_open()).unwrap_or(false)
        });
    }
    // 选中加密条目（双击前置 SelectRow 的等价态）
    let id = data
        .lock()
        .unwrap()
        .main
        .entry_id_of_path("secret.txt")
        .expect("条目存在");
    data.lock().unwrap().main.select_only(id);

    // ① 双击打开 → 武装门 + 投影密码弹窗（而不是直接开）
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.open_selection_entry(cx));
    });
    assert_eq!(
        data.lock().unwrap().router.lock().unwrap().gate_action(),
        Some(&GateAction::OpenSelection {
            id,
            name: "secret.txt".into(),
            form: crate::viewmodel::OpenForm::Configured
        }),
        "加密条目 + 无缓存 → 必须先过密码门"
    );
    match data.lock().unwrap().export_snapshot().dialog {
        Some(UiDialogPrompt::Password { attempts, .. }) => assert_eq!(attempts, 0),
        other => panic!("应投影密码弹窗，实际 {other:?}"),
    }

    // ② 提交正确密码 → 后台预检通过 → 缓存 + 门放行
    vtcx.update(|_, _| {
        super::prompt::handle_dialog_event(&data, &DialogEvent::PasswordSubmit("pw-a".into()));
    });
    {
        let probe = data.clone();
        let zip_c = zip.clone();
        wait_for(vtcx, "密码预检完成", &|| {
            probe
                .lock()
                .map(|d| d.cached_password(&zip_c).is_some())
                .unwrap_or(false)
        });
    }
    {
        let d = data.lock().unwrap();
        let r = d.router.lock().unwrap();
        assert!(r.gate_attempts().is_none(), "验证通过即解门");
        assert_eq!(r.gate_action(), None);
    }
    // 放行信号必须**无条件**发出（泵据此重跑，不依赖快照批次条件）
    let mut seen_ready = false;
    while let Ok(sig) = rx.try_recv() {
        if matches!(sig, crate::viewmodel::VmSignal::GateActionReady) {
            seen_ready = true;
        }
    }
    assert!(
        seen_ready,
        "门放行须发 GateActionReady（用户口径 2026-09-28）"
    );
    assert_eq!(
        data.lock().unwrap().take_pending_gate_action(),
        Some(GateAction::OpenSelection {
            id,
            name: "secret.txt".into(),
            form: crate::viewmodel::OpenForm::Configured
        }),
        "放行的动作进待执行槽（泵取出重跑原操作）"
    );

    // 生产序列复现（用户口径 2026-09-28 实测「输入密码后不继续」的另一半）：
    // 门武装后泵会先刷快照（`update_from_state` → 行集/树重喂），这一步**不得**
    // 清掉选择集——否则放行后的重跑会因「非恰一选中」直接 no-op。
    {
        let snap = data.lock().unwrap().export_snapshot();
        vtcx.update(|_, cx| {
            shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
        });
        vtcx.run_until_parked();
        assert_eq!(
            data.lock().unwrap().main.selection(),
            vec![id],
            "快照刷新不得清掉选择集"
        );
    }

    // ③ 错密码 + 加密条目：忘掉缓存后再触发 → 门重开；提交错密码 →
    //    attempts+1、不落缓存，且**不关窗**（用户口径 2026-09-28：窗内显示
    //    「第 n/max 次」并可原地重输）。
    data.lock().unwrap().forget_password(&zip);
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| {
            s.open_selected_entry(crate::viewmodel::OpenForm::Configured, cx)
        });
    });
    assert_eq!(
        data.lock().unwrap().router.lock().unwrap().gate_action(),
        Some(&GateAction::OpenSelection {
            id,
            name: "secret.txt".into(),
            form: crate::viewmodel::OpenForm::Configured
        }),
        "「打开方式…」的门动作必须携带所选程序（放行后按它打开、不再重问）"
    );
    vtcx.update(|_, _| {
        super::prompt::handle_dialog_event(&data, &DialogEvent::PasswordSubmit("wrong".into()));
    });
    {
        let probe = data.clone();
        wait_for(vtcx, "错密码重问计数", &|| {
            probe.lock().unwrap().router.lock().unwrap().gate_attempts() == Some(1)
        });
    }
    let d = data.lock().unwrap();
    assert!(
        d.cached_password(&zip).is_none(),
        "错密码绝不进缓存（不给后续操作喂坏密码）"
    );
    assert_eq!(
        d.router.lock().unwrap().gate_action(),
        Some(&GateAction::OpenSelection {
            id,
            name: "secret.txt".into(),
            form: crate::viewmodel::OpenForm::Configured
        }),
        "错密码后门保持武装（可在窗内重输；所选程序随原触发保留）"
    );
    drop(d);

    // ③b 连错到上限（用户口径 2026-09-28）：门被解 + **弹错误窗** + **任务中心留痕**。
    //     上面已提交 1 次错密码（attempts=1），再提交 `MAX-1` 次触上限。
    for _ in 1..crate::viewmodel::router::PASSWORD_MAX_RETRIES {
        vtcx.update(|_, _| {
            super::prompt::handle_dialog_event(&data, &DialogEvent::PasswordSubmit("wrong".into()));
        });
        let probe = data.clone();
        wait_for(vtcx, "错密码计数推进", &|| {
            probe
                .lock()
                .map(|d| {
                    let r = d.router.lock().unwrap();
                    r.gate_action().is_none() || r.gate_attempts() != Some(1)
                })
                .unwrap_or(false)
        });
    }
    {
        let probe = data.clone();
        wait_for(vtcx, "达上限后解门", &|| {
            probe
                .lock()
                .unwrap()
                .router
                .lock()
                .unwrap()
                .gate_action()
                .is_none()
        });
    }
    let mut seen_err = false;
    while let Ok(sig) = rx.try_recv() {
        if let crate::viewmodel::VmSignal::ShowMessage(dialog) = &sig {
            let text = format!("{} {}", dialog.title, dialog.detail);
            assert!(text.contains("密码"), "错误窗文案应说明密码问题：{text}");
            seen_err = true;
        }
    }
    assert!(
        seen_err,
        "连续输错达上限必须发错误弹窗信号（不能再静默关窗）"
    );
    {
        let d = data.lock().unwrap();
        let jobs = d.jobs.lock().unwrap();
        assert!(
            jobs.rows().iter().any(|r| r.kind == "open"
                && matches!(
                    r.state,
                    crate::viewmodel::JobRowState::Failed
                        | crate::viewmodel::JobRowState::PartialDone
                )),
            "密码连续错误必须在任务中心留痕（失败行），实得 {:?}",
            jobs.rows()
                .iter()
                .map(|r| (r.kind, r.state))
                .collect::<Vec<_>>()
        );
    }

    // ④ 重新武装门（上限后门已解）→ 正确密码 → 放行后续走打开流程（用户口径 2026-09-28 实测：门不能拦断
    //    后续流程，不该要求用户再双击一次）。窗内改用**正确**密码重输 →
    //    预检通过 → 放行 → 泵取出待执行动作重跑 → 物化 → 尝试外部打开。
    //    指定一个不存在的编辑器，让「打开」以失败收场（记录仍必须出现——
    //    `record_op` 成功/失败都记），据此可观测整条续走链。
    data.lock().unwrap().settings.editor = "Z:\\hzip-nonexistent\\nope.exe".into();
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| {
            s.open_selected_entry(crate::viewmodel::OpenForm::Configured, cx)
        });
    });
    vtcx.update(|_, _| {
        super::prompt::handle_dialog_event(&data, &DialogEvent::PasswordSubmit("pw-a".into()));
    });
    {
        let probe = data.clone();
        let zip_c = zip.clone();
        wait_for(vtcx, "重输正确密码后放行", &|| {
            probe
                .lock()
                .map(|d| d.cached_password(&zip_c).is_some())
                .unwrap_or(false)
        });
    }
    assert_eq!(
        data.lock().unwrap().take_pending_gate_action(),
        Some(GateAction::OpenSelection {
            id,
            name: "secret.txt".into(),
            form: crate::viewmodel::OpenForm::Configured
        }),
        "放行后动作进待执行槽，等泵重跑（所选程序随原触发保留）"
    );
    // 泵的那一行（生产：主线程泵在每批信号后调用）
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.retry_pending_gate_action(cx));
    });
    {
        let probe = data.clone();
        wait_for(vtcx, "门放行后自动打开", &|| {
            probe
                .lock()
                .map(|d| {
                    d.jobs
                        .lock()
                        .map(|j| j.rows().iter().any(|r| r.kind == "open"))
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        });
    }
}

// —— 粘贴源展开（T-MENU8 双向粘贴 P0；纯函数面） ——

/// 目录源 walkdir 展开文件 + 文件源直收；目标名 = current_dir 前缀 + 源
/// 相对结构（源目录名作为一层进目标名），与 append_changeset 拼装闭环。
#[test]
fn paste_expansion_collects_dir_tree_with_current_dir_prefix() {
    let dir = tempfile::TempDir::new().unwrap();
    let pics = dir.path().join("pics");
    std::fs::create_dir_all(pics.join("sub")).unwrap();
    std::fs::write(pics.join("a.png"), b"A").unwrap();
    std::fs::write(pics.join("sub").join("b.png"), b"B").unwrap();
    // 并排一个无关节目：确保只收 pics 子树
    std::fs::write(dir.path().join("stray.txt"), b"S").unwrap();

    let expanded = expand_paste_sources(std::slice::from_ref(&pics));
    assert_eq!(expanded.len(), 1, "目录源产一个展开");
    let e = &expanded[0];
    assert_eq!(e.files.len(), 2, "两个文件（空目录壳不产文件）");
    // 相对名相对源目录父目录：首段 = 源目录名
    let want_rels: std::collections::BTreeSet<_> = ["pics/a.png", "pics/sub/b.png"]
        .into_iter()
        .map(String::from)
        .collect();
    let got_rels: std::collections::BTreeSet<_> = e.rels.iter().cloned().collect();
    assert_eq!(got_rels, want_rels);
    // 文件路径存在且落在源子树内
    for f in &e.files {
        assert!(f.starts_with(&pics));
        assert!(f.is_file());
    }

    // 目标名组装：current_dir 前缀（VM 口径 `/` 分隔）
    let entries = collect_paste_entries("docs", &expanded);
    let want_names: std::collections::BTreeSet<_> = [
        "docs/pics/a.png".to_string(),
        "docs/pics/sub/b.png".to_string(),
    ]
    .into_iter()
    .collect();
    let got_names: std::collections::BTreeSet<_> = entries.iter().map(|(_, n)| n.clone()).collect();
    assert_eq!(got_names, want_names, "目标名 = current_dir + 源相对结构");
    assert!(entries.iter().all(|(f, _)| f.is_file()));
}

/// 文件源相对名 = 文件名；current_dir 为根时目标名无前缀。
#[test]
fn paste_expansion_file_source_target_at_root() {
    let dir = tempfile::TempDir::new().unwrap();
    let f1 = dir.path().join("x.txt");
    let f2 = dir.path().join("y.bin");
    std::fs::write(&f1, b"1").unwrap();
    std::fs::write(&f2, b"2").unwrap();

    let expanded = expand_paste_sources(&[f1, f2]);
    assert_eq!(expanded.len(), 2, "文件源各自独立展开");
    assert!(
        expanded
            .iter()
            .all(|e| e.files.len() == 1 && e.rels.len() == 1)
    );
    assert_eq!(
        expanded
            .iter()
            .map(|e| e.rels[0].as_str())
            .collect::<Vec<_>>(),
        vec!["x.txt", "y.bin"]
    );

    let entries = collect_paste_entries("", &expanded);
    let names: std::collections::BTreeSet<_> = entries.iter().map(|(_, n)| n.as_str()).collect();
    assert_eq!(
        names,
        ["x.txt", "y.bin"].into_iter().collect(),
        "根 = 相对名本体"
    );
}

/// 空目录（无文件后代）跳过（可接受降级）；不存在路径不产出。
#[test]
fn paste_expansion_skips_empty_dirs_and_missing_paths() {
    let dir = tempfile::TempDir::new().unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let ghost = dir.path().join("ghost.txt");

    let expanded = expand_paste_sources(&[empty, ghost]);
    assert!(expanded.is_empty(), "空目录与不存在路径都不产出");
}

mod new_window_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// T-MENU10：`NewWindow` 动作调用注入的窗口工厂；未注入时静默 no-op。
    ///
    /// 验证的是**接线**（动作 → 工厂）——工厂本体（开窗 + 注册表登记）在
    /// `run_ui` 内，其注册表/退出语义由 `ui::shell::windows` 单测覆盖。
    #[gpui_kit::test]
    fn new_window_action_invokes_injected_factory(cx: &mut gpui_kit::TestAppContext) {
        let calls = std::rc::Rc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let shell_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let slot2 = shell_slot.clone();
        let (_root, vtcx) = cx.add_window_view(move |window, cx| {
            gpui_kit::component::init(cx);
            let shell = cx.new(|cx| AppShell::new(cx, ShellHandlers::default()));
            let c = calls2.clone();
            shell.update(cx, |s, _| {
                s.set_new_window_factory(std::rc::Rc::new(move |_cx| {
                    c.fetch_add(1, Ordering::Relaxed);
                }));
            });
            *slot2.borrow_mut() = Some(shell.clone());
            let view: gpui_kit::AnyView = shell.into();
            gpui_kit::component::Root::new(view, window, cx)
        });
        let shell = shell_slot.borrow().clone().expect("shell 实体");
        assert_eq!(calls.load(Ordering::Relaxed), 0, "未触发前零调用");
        vtcx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                s.dispatch_action_for_test(window, cx, &NewWindow);
            });
        });
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "NewWindow → 工厂被调用一次"
        );
    }

    /// 未注入工厂（headless/测试装配）→ NewWindow 静默不 panic。
    #[gpui_kit::test]
    fn new_window_without_factory_is_noop(cx: &mut gpui_kit::TestAppContext) {
        let shell_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let slot2 = shell_slot.clone();
        let (_root, vtcx) = cx.add_window_view(move |window, cx| {
            gpui_kit::component::init(cx);
            let shell = cx.new(|cx| AppShell::new(cx, ShellHandlers::default()));
            *slot2.borrow_mut() = Some(shell.clone());
            let view: gpui_kit::AnyView = shell.into();
            gpui_kit::component::Root::new(view, window, cx)
        });
        let shell = shell_slot.borrow().clone().expect("shell 实体");
        vtcx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                s.dispatch_action_for_test(window, cx, &NewWindow);
            });
        });
    }
}

/// T-SEL2：Ctrl+A 全选 = 右栏**列表当前那一屏**（不是整档）。
///
/// 走真实链路：真实窗口 + `Main` 上下文里的 `ctrl-a` 绑定（`cx.bind_keys`）→
/// AppShell 的 `on_action(SelectAll)` → `AppShell::select_all_visible` →
/// `AppData.dir_rows`（用户看到的那一屏）→ VM 双通道选择 → `EntriesChanged`
/// 回投 → 快照回流 → 浏览委托选中集镜像。
///
/// 三条断言：① 根层只选根层两条、隐式目录 `sub` 进目录通道；② 进 `sub/` 再按
/// 只选该层两条（整档语义在这一步会仍把根层选上）；③ 回投信号确实发出
/// （高亮/状态栏/选中统计靠它当批刷新——旧 Action 入口漏发，用户观感
/// 「按了 Ctrl+A 没反应」）。
#[gpui_kit::test]
fn ctrl_a_selects_only_visible_rows(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell as sh;
    let (tx, rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine,
    )));
    {
        let mut d = data.lock().expect("lock");
        d.signal_tx = Some(tx.clone());
        d.temp_area = Some(Arc::new(
            hzip_platform::temp_area::TempArea::new_session(None).expect("temp area"),
        ));
    }
    let data_w = data.clone();
    let slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        // 生产装配在 `run_ui`（`ui/shell/runtime.rs`）——测试里按同一上下文
        // （`Main`）注册同一条绑定，走真实键位分发链
        cx.bind_keys([gpui_kit::KeyBinding::new(
            "ctrl-a",
            sh::SelectAll,
            Some("Main"),
        )]);
        let shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_w.clone()),
            Some(tx.clone()),
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = slot.borrow().clone().expect("shell 实体");

    // 夹具：根 2 文件 + sub/ 内 2 文件（共 4 条目）
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ar = dir.path().join("demo.tar");
    crate::viewmodel::testutil::write_ustar(
        &ar,
        &[
            ("a.txt", b"a"),
            ("b.txt", b"b"),
            ("sub/c.txt", b"c"),
            ("sub/d.txt", b"d"),
        ],
    );
    allow_parking_for_real_io(vtcx);
    let task = vtcx
        .update(|_, cx| crate::ui::shell::open_archive_task(cx, data.clone(), ar.clone(), false));
    vtcx.update(|_, cx| {
        cx.spawn(async move |_| {
            let _ = task.await;
        })
        .detach();
    });
    let wait = |vtcx: &mut gpui_kit::VisualTestContext, what: &str, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}：10s 未完成");
            std::thread::sleep(std::time::Duration::from_millis(10));
            vtcx.run_until_parked();
        }
    };
    {
        let probe = data.clone();
        wait(vtcx, "打开", &|| {
            probe.lock().map(|d| d.main.is_open()).unwrap_or(false)
        });
    }
    vtcx.run_until_parked();
    // 当前 VM 实体选择对应的条目名（按 id 查 `entries`；原始通道，不经
    // `selection_names` 的目录子树展开）
    let sel_names = |d: &crate::app::state::AppData| -> Vec<String> {
        d.main
            .selection()
            .iter()
            .filter_map(|id| d.main.entry(*id).map(|e| e.name.clone()))
            .collect()
    };
    // 行集里的实体 id（假定顺序即升序——行集按名序，tar 夹具下 id 与名序一致）
    let row_entity_ids = |d: &crate::app::state::AppData| -> Vec<usize> {
        d.dir_rows.iter().filter_map(|r| r.entry_id).collect()
    };
    // 快照回流（泵的常规路径）+ 重绘 → 浏览委托选中集镜像
    let delegate_mirror = |vtcx: &mut gpui_kit::VisualTestContext| {
        (
            vtcx.update(|_, cx| {
                crate::ui::views::browse::filter_hold_in(cx)
                    .map(|h| h.read(cx).table.read(cx).delegate().selected_ids.clone())
            }),
            vtcx.update(|_, cx| {
                crate::ui::views::browse::filter_hold_in(cx)
                    .map(|h| h.read(cx).table.read(cx).delegate().selected_dirs.clone())
            }),
        )
    };
    let sync_snapshot = |vtcx: &mut gpui_kit::VisualTestContext| {
        let snap = data.lock().unwrap().export_snapshot();
        vtcx.update(|window, cx| {
            shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
            window.draw(cx).clear(cx);
        });
    };

    // 泵的常规路径：后台线程重建当前层行集（测试里手工触发）
    {
        let mut d = data.lock().unwrap();
        d.rebuild_browse_rows();
        assert_eq!(
            d.dir_rows
                .iter()
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            vec!["sub", "a.txt", "b.txt"],
            "根层行集 = 当前目录直接子项（`..` 合成行不在其中）"
        );
    }
    // 排空开档期信号：下面只认 ctrl-a 自己发的
    while rx.try_recv().is_ok() {}

    // —— ① 根层：无焦点直按 Ctrl+A（用户路径）——
    vtcx.simulate_keystrokes("ctrl-a");
    vtcx.run_until_parked();
    {
        let d = data.lock().unwrap();
        assert_eq!(
            sel_names(&d),
            vec!["a.txt".to_string(), "b.txt".to_string()],
            "根层 Ctrl+A 只选根层两条（sub/c.txt、sub/d.txt 不进实体通道）"
        );
        assert_eq!(
            d.main.dir_selection(),
            vec!["sub".to_string()],
            "隐式目录行 sub（tar 无目录条目）进目录通道"
        );
        assert_eq!(
            d.main.selection(),
            row_entity_ids(&d),
            "选择集 == 行集实体 id（一屏全选）"
        );
    }
    // —— ② 回投信号：高亮/状态栏靠它当批刷新 ——
    let mut entries_changed = false;
    while let Ok(sig) = rx.try_recv() {
        if matches!(sig, crate::viewmodel::VmSignal::EntriesChanged) {
            entries_changed = true;
        }
    }
    assert!(
        entries_changed,
        "Ctrl+A 必须回投 EntriesChanged（旧 Action 入口漏发 = 用户观感「没反应」）"
    );
    // —— ③ 快照回流 → 浏览委托镜像（icon/list 高亮/菜单可用性与快照同源）——
    sync_snapshot(vtcx);
    let expect_root: std::collections::HashSet<u64> = data
        .lock()
        .unwrap()
        .dir_rows
        .iter()
        .filter_map(|r| r.entry_id)
        .map(|id| id as u64)
        .collect();
    let (mirror_ids, mirror_dirs) = delegate_mirror(vtcx);
    assert_eq!(
        mirror_ids,
        Some(expect_root),
        "快照回流后委托 selected_ids == 行集实体 id"
    );
    assert_eq!(
        mirror_dirs,
        Some(["sub".to_string()].into_iter().collect()),
        "隐式目录通道同样进委托镜像"
    );

    // —— ④ 进 sub/ 再按：只选该层两条（整档语义在这一步会仍选上根层）——
    {
        let mut d = data.lock().unwrap();
        d.main.enter_dir("sub");
        d.rebuild_browse_rows();
        assert_eq!(
            d.dir_rows
                .iter()
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            vec!["sub/c.txt", "sub/d.txt"],
            "sub/ 行集"
        );
    }
    while rx.try_recv().is_ok() {}
    vtcx.simulate_keystrokes("ctrl-a");
    vtcx.run_until_parked();
    {
        let d = data.lock().unwrap();
        assert_eq!(
            sel_names(&d),
            vec!["sub/c.txt".to_string(), "sub/d.txt".to_string()],
            "sub/ 内 Ctrl+A 只选该层两条（不含根层任何 id）"
        );
        assert!(d.main.dir_selection().is_empty(), "该层无隐式目录行");
        assert_eq!(
            d.main.selection(),
            row_entity_ids(&d),
            "选择集 == 行集实体 id"
        );
    }
    // 委托镜像跟随当前层
    sync_snapshot(vtcx);
    let expect_sub: std::collections::HashSet<u64> = data
        .lock()
        .unwrap()
        .dir_rows
        .iter()
        .filter_map(|r| r.entry_id)
        .map(|id| id as u64)
        .collect();
    let (mirror_ids, _) = delegate_mirror(vtcx);
    assert_eq!(
        mirror_ids,
        Some(expect_sub),
        "委托镜像跟随当前层行集（不含任何非本层 id）"
    );
}

/// T-SEL2 作用域门：**非浏览页**（任务中心/设置）上按 Ctrl+A 不吞动作——
/// 既不改选择集、也不回投 `EntriesChanged`（`AppShell::visible_rows_action`
/// 走 `cx.propagate()`，把动作让给上层；将来别的页面想接这个键位也接得住）。
#[gpui_kit::test]
fn ctrl_a_on_non_browse_tab_is_not_swallowed(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell as sh;
    let (tx, rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine,
    )));
    {
        let mut d = data.lock().expect("lock");
        d.signal_tx = Some(tx.clone());
        // 已知的非空选择集：不依赖档案——目录通道按路径选（`select_rows` 是
        // 双通道整组替换的现成出口），这样「未变」是可观测的强断言
        d.main.select_rows(&[], &["kept".to_string()]);
    }
    let data_w = data.clone();
    let slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        cx.bind_keys([gpui_kit::KeyBinding::new(
            "ctrl-a",
            sh::SelectAll,
            Some("Main"),
        )]);
        let shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_w.clone()),
            Some(tx.clone()),
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = slot.borrow().clone().expect("shell 实体");

    // 离开浏览页（任务中心未开 → `switch_tab` 顺带打开它并切过去）
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.switch_tab(cx, MainTab::Jobs));
    });
    vtcx.update(|_, cx| {
        assert_eq!(
            shell.read(cx).state.read(cx).tab,
            MainTab::Jobs,
            "已离开浏览页（用例前提）"
        );
    });
    vtcx.run_until_parked();
    // 排空切页签/设置落地产生的信号，下面只认按 Ctrl+A 引起的
    while rx.try_recv().is_ok() {}

    vtcx.simulate_keystrokes("ctrl-a");
    vtcx.run_until_parked();
    {
        let d = data.lock().unwrap();
        assert_eq!(
            d.main.dir_selection(),
            vec!["kept".to_string()],
            "非浏览页 Ctrl+A 不得改选择集"
        );
        assert_eq!(d.main.selection_count(), 1, "选择集原样");
    }
    let mut entries_changed = false;
    while let Ok(sig) = rx.try_recv() {
        if matches!(sig, crate::viewmodel::VmSignal::EntriesChanged) {
            entries_changed = true;
        }
    }
    assert!(
        !entries_changed,
        "非浏览页不得回投 EntriesChanged（动作已 propagate 出去，未被浏览页吞掉）"
    );
}

/// 集成冒烟（T-KEYS 收口）：**配置 → 镜像 → 装配 → 真实按键分发**整条链。
///
/// 三段：
/// ① 默认键位 `ctrl-a` 触发全选；
/// ② 改键 `select_all = "f2"` → 旧键失效、新键生效；
/// ③ 禁用 `select_all = "none"` → 新旧键都不触发。
///
/// 为什么手动走「`apply_setting` → `keymap::install(cx, &镜像)`」而不是等泵：泵只
/// 活在 `run_ui` 的进程主循环里（测试不经过它）。这两支箭正是泵在
/// `runtime.rs` 里做的（`SettingId::Keymap` 批次 → 取镜像 → 重装）。
/// 设置页自身的上行/渲染由 `ui::views::tests::settings_layout::keys_*` 覆盖。
#[gpui_kit::test]
fn rebind_select_all_takes_effect_end_to_end(cx: &mut gpui_kit::TestAppContext) {
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine,
    )));
    {
        let mut d = data.lock().expect("lock");
        d.signal_tx = Some(tx.clone());
        d.temp_area = Some(Arc::new(
            hzip_platform::temp_area::TempArea::new_session(None).expect("temp area"),
        ));
    }
    let data_w = data.clone();
    let slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_w.clone()),
            Some(tx.clone()),
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let _shell = slot.borrow().clone().expect("shell 实体");

    let dir = tempfile::TempDir::new().expect("tempdir");
    let ar = dir.path().join("demo.tar");
    crate::viewmodel::testutil::write_ustar(&ar, &[("a.txt", b"a"), ("b.txt", b"b")]);
    allow_parking_for_real_io(vtcx);
    let task = vtcx
        .update(|_, cx| crate::ui::shell::open_archive_task(cx, data.clone(), ar.clone(), false));
    vtcx.update(|_, cx| {
        cx.spawn(async move |_| {
            let _ = task.await;
        })
        .detach();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !data.lock().map(|d| d.main.is_open()).unwrap_or(false) {
        assert!(std::time::Instant::now() < deadline, "打开档案超时");
        std::thread::sleep(std::time::Duration::from_millis(10));
        vtcx.run_until_parked();
    }
    vtcx.run_until_parked();
    // 行集由泵后台重建（`rebuild_browse_rows`）——测试不跑泵，手动建一次：
    // 「Ctrl+A 选的是用户看到的那一屏」，而那一屏就是 `dir_rows`。
    data.lock().expect("lock").rebuild_browse_rows();

    let sel_len = || data.lock().map(|d| d.main.selection().len()).unwrap_or(0);
    let clear_sel = || {
        if let Ok(mut d) = data.lock() {
            d.main.select_all(true);
        }
    };
    // 置某动作的配置值（走真实「设置变更」入口：写 [keys] + 镜像），再把镜像
    // 装进 keymap —— 与泵的两步逐字同源。
    let set_key = |vtcx: &mut gpui_kit::VisualTestContext, field: &str, raw: &str| {
        let keys = {
            let mut d = data.lock().expect("lock");
            let mut k = d.settings.keys.clone();
            *k.field_mut(field).expect("契约键名") = raw.to_string();
            d.apply_setting(
                crate::viewmodel::SettingId::Keymap,
                &crate::app::settings_registry::SettingValue::Keys(Box::new(k.clone())),
            );
            d.settings.keys.clone()
        };
        vtcx.update(|_window, cx| crate::ui::shell::keymap::install(cx, &keys));
        vtcx.run_until_parked();
    };

    // 启动装配（等价 run_ui 的 startup install）：默认键位
    set_key(vtcx, "select_all", "");

    // ① 默认 `ctrl-a` 全选
    clear_sel();
    vtcx.simulate_keystrokes("ctrl-a");
    vtcx.run_until_parked();
    assert_eq!(sel_len(), 2, "默认键位 ctrl-a 应全选当前层两行");

    // ② 改键为 `f2`：旧键失效、新键生效
    set_key(vtcx, "select_all", "f2");
    clear_sel();
    vtcx.simulate_keystrokes("ctrl-a");
    vtcx.run_until_parked();
    assert_eq!(sel_len(), 0, "改键后旧键 ctrl-a 必须不再触发");
    vtcx.simulate_keystrokes("f2");
    vtcx.run_until_parked();
    assert_eq!(sel_len(), 2, "新键 f2 必须生效");

    // ③ 禁用：新旧键都不触发
    set_key(vtcx, "select_all", "none");
    clear_sel();
    vtcx.simulate_keystrokes("f2");
    vtcx.run_until_parked();
    assert_eq!(sel_len(), 0, "禁用后 f2 不得触发");
    vtcx.simulate_keystrokes("ctrl-a");
    vtcx.run_until_parked();
    assert_eq!(sel_len(), 0, "禁用后 ctrl-a 同样不得触发");
}

// 探针动作：绑在 `ctrl-q` 上，用来观测「按键有没有被录制吞掉」。
gpui_kit::actions!(hzip_gui_shell_keys_probe, [ProbeHotkey]);

/// 录制中**切换主页签**必须释放键盘（T-KEYS 收口）。
///
/// 录制态挂的是 `App::intercept_keystrokes`——App 级副作用，不会因为设置面板
/// 不再渲染而消失。切到浏览页后面板实体仍活着（只是不画），壳层若不在切页签处
/// 收口，用户会发现键盘全失灵（按什么都被吞）直到切回设置页。本用例用一条绑在
/// `ctrl-q` 上的探针动作观测「按键是否真的被派发出去」。
#[gpui_kit::test]
fn leaving_settings_tab_releases_key_recording(cx: &mut gpui_kit::TestAppContext) {
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine,
    )));
    {
        let mut d = data.lock().expect("lock");
        d.signal_tx = Some(tx.clone());
        d.temp_area = Some(Arc::new(
            hzip_platform::temp_area::TempArea::new_session(None).expect("temp area"),
        ));
    }
    let hits: std::rc::Rc<std::cell::Cell<usize>> = std::rc::Rc::default();
    let counter = hits.clone();
    let data_w = data.clone();
    let slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_w.clone()),
            Some(tx.clone()),
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = slot.borrow().clone().expect("shell 实体");
    // 探针绑定：与设置页的场景测试同一手法（`App::on_action` 全局监听）；
    // 不复用 `sh::Quit`（那会真去退出进程，测试里不可观测"是否被吞"）。
    vtcx.update(|_window, cx| {
        cx.bind_keys([gpui_kit::KeyBinding::new(
            "ctrl-q",
            ProbeHotkey,
            Some("Main"),
        )]);
        cx.on_action(move |_: &ProbeHotkey, _: &mut gpui_kit::App| counter.set(counter.get() + 1));
    });

    // 先开档案（浏览页签存在，切回浏览才有意义）
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ar = dir.path().join("demo.tar");
    crate::viewmodel::testutil::write_ustar(&ar, &[("a.txt", b"a")]);
    allow_parking_for_real_io(vtcx);
    let task = vtcx
        .update(|_, cx| crate::ui::shell::open_archive_task(cx, data.clone(), ar.clone(), false));
    vtcx.update(|_, cx| {
        cx.spawn(async move |_| {
            let _ = task.await;
        })
        .detach();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !data.lock().map(|d| d.main.is_open()).unwrap_or(false) {
        assert!(std::time::Instant::now() < deadline, "打开档案超时");
        std::thread::sleep(std::time::Duration::from_millis(10));
        vtcx.run_until_parked();
    }
    vtcx.run_until_parked();

    // 基线：探针动作能被派发
    vtcx.simulate_keystrokes("ctrl-q");
    vtcx.run_until_parked();
    assert_eq!(hits.get(), 1, "基线：ctrl-q 应派发到探针动作");

    // 打开设置页 → 切到「快捷键」页（下标 7）→ 点「退出」行的键位芯片（进入录制态）
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, crate::ui::contracts::ToolbarAction::Settings)
        });
    });
    let click = |vtcx: &mut gpui_kit::VisualTestContext, anchor: &'static str| {
        vtcx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let bounds = vtcx
            .debug_bounds(anchor)
            .unwrap_or_else(|| panic!("缺锚点 {anchor}"));
        vtcx.simulate_click(bounds.center(), gpui_kit::Modifiers::none());
        vtcx.update(|_window, cx| cx.background_executor().run_until_parked());
    };
    let tab_now = |vtcx: &mut gpui_kit::VisualTestContext| -> crate::ui::contracts::MainTab {
        vtcx.update(|_window, cx| shell.read(cx).state.read(cx).tab)
    };
    click(vtcx, "settings-tab-7");
    click(vtcx, "kc-quit");

    // 录制中：按键被拦下（不派发）
    vtcx.simulate_keystrokes("ctrl-q");
    vtcx.run_until_parked();
    assert_eq!(hits.get(), 1, "录制中 ctrl-q 不得派发（拦截图必须吃掉它）");

    // ① **快照路径**：推一份「打开了新档案」的快照 ⇒ `ShellState::apply` 把页签
    //    拽回浏览页（生产里由泵投递，测试不跑泵故自推）。壳层必须在这条路上也
    //    收掉录制——它是「录制中打开压缩包」那条真实用户路径。
    let snapshot = snap(None, false, "demo.tar");
    vtcx.update(|_window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &snapshot));
    });
    vtcx.run_until_parked();
    vtcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    assert_eq!(
        tab_now(vtcx),
        crate::ui::contracts::MainTab::Browse,
        "前置：快照应把页签拽回浏览页（否则本段没测到东西）"
    );
    vtcx.simulate_keystrokes("ctrl-q");
    vtcx.run_until_parked();
    assert_eq!(
        hits.get(),
        2,
        "快照把页签切走后 ctrl-q 必须恢复派发（`update_from_state` 未收口 = 键盘被吞）"
    );

    // ② **页签切换路径**：重开设置页（面板复用，仍在「快捷键」子页）→ 再点一次
    //    同一行芯片进录制 → 显式 `switch_tab` 离开设置页。前置必须先让浏览页签
    //    **真的存在**（`present_tabs` 里没有 Browse 时 `switch_tab` 会早退，
    //    那样这段就成了空转——第一段推的快照已把 archive_name 设上）。
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, crate::ui::contracts::ToolbarAction::Settings)
        });
    });
    click(vtcx, "kc-quit");
    vtcx.simulate_keystrokes("ctrl-q");
    vtcx.run_until_parked();
    assert_eq!(hits.get(), 2, "第二次录制中 ctrl-q 同样不得派发");

    vtcx.update(|_window, cx| {
        shell.update(cx, |s, cx| {
            s.switch_tab(cx, crate::ui::contracts::MainTab::Browse)
        });
    });
    vtcx.run_until_parked();
    assert_eq!(
        tab_now(vtcx),
        crate::ui::contracts::MainTab::Browse,
        "前置：`switch_tab` 必须真的离开了设置页（页签不存在会静默早退）"
    );
    vtcx.simulate_keystrokes("ctrl-q");
    vtcx.run_until_parked();
    assert_eq!(
        hits.get(),
        3,
        "切走设置页后 ctrl-q 必须恢复派发（订阅没 drop = 键盘被永久吞掉）"
    );
}

/// 加密 zip 里的容器条目：进容器必须**复用**格式层已 spool 的那份副本（移动，
/// 不再复制），且内层会话内容完整。
///
/// 用户实测 2026-10-22：7.3 GB 的 `qcow2.zip`，spool（整条解码）+ 物化副本 =
/// 两份 ⇒ 临时盘被挤爆、密码后无任何提示。这里守三件事：
/// ① 会话确实换成内层档案（`nested` + 标题）；② 会话源文件**只有一份**且长度等于
/// 条目解码后大小（证明走的是移动而不是「spool 后再复制」，也没被截断）；
/// ③ 内层条目可读（内容对）。
#[gpui_kit::test]
fn container_open_moves_spooled_copy_for_encrypted_entries(cx: &mut gpui_kit::TestAppContext) {
    use hzip_format::FormatHandler as _;
    use std::io::Write;

    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine,
    )));
    {
        let mut d = data.lock().expect("lock");
        d.signal_tx = Some(tx.clone());
        d.temp_area = Some(Arc::new(
            hzip_platform::temp_area::TempArea::new_session(None).expect("temp area"),
        ));
    }
    let data_for_window = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let app_shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_for_window.clone()),
            Some(tx.clone()),
        );
        let app_shell = cx.new(|_| app_shell);
        *slot2.borrow_mut() = Some(app_shell.clone());
        let view: gpui_kit::AnyView = app_shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    // 夹具：加密 zip（ZipCrypto）里放一份真实 tar
    const PW: &str = "nested-pw";
    let dir = tempfile::TempDir::new().expect("tempdir");
    let inner = dir.path().join("child.tar");
    crate::viewmodel::testutil::write_ustar(&inner, &[("deep/inner.txt", b"payload")]);
    let inner_bytes = std::fs::read(&inner).expect("读内层");
    let outer = dir.path().join("outer.zip");
    let pwz = zeroize::Zeroizing::new(PW.to_string());
    {
        let mut w = hzip_fmt_zip::ZipFormatHandler::new()
            .create_writer(
                hzip_format::Sink::File(outer.clone()),
                &hzip_format::CreateOptions {
                    encryption: Some(hzip_format::EncryptionRequest::ZipCrypto),
                    ..Default::default()
                },
            )
            .expect("create");
        let meta = hzip_format::EntryMeta {
            name: "child.tar".into(),
            size_raw: Some(inner_bytes.len() as u64),
            ..Default::default()
        };
        let mut s = w
            .next_entry(&meta, hzip_format::PasswordRef::new(&pwz))
            .expect("entry");
        s.write_all(&inner_bytes).expect("write");
        s.finish().expect("finish");
        w.finish().expect("finish archive");
    }

    allow_parking_for_real_io(vtcx);
    let task = vtcx.update(|_, cx| {
        crate::ui::shell::open_archive_task(cx, data.clone(), outer.clone(), false)
    });
    vtcx.update(|_, cx| {
        cx.spawn(async move |_| {
            let _ = task.await;
        })
        .detach();
    });
    let wait = |vtcx: &mut gpui_kit::VisualTestContext, what: &str, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}：20s 内未完成");
            std::thread::sleep(std::time::Duration::from_millis(10));
            vtcx.run_until_parked();
        }
    };
    {
        let probe = data.clone();
        wait(vtcx, "打开外层档案", &|| {
            probe.lock().map(|d| d.main.is_open()).unwrap_or(false)
        });
    }
    // 密码门：直接置缓存（headless 不走对话框）
    data.lock()
        .unwrap()
        .remember_password(&outer, zeroize::Zeroizing::new(PW.to_string()));

    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.open_container("child.tar", cx));
    });
    {
        let probe = data.clone();
        wait(vtcx, "进入加密容器", &|| {
            probe
                .lock()
                .map(|d| d.export_snapshot().nested)
                .unwrap_or(false)
        });
    }

    let (snap_name, src, entries) = {
        let d = data.lock().unwrap();
        let src = d.main.source_path().map(|p| p.to_path_buf());
        let names: Vec<String> = d
            .main
            .entries()
            .iter()
            .map(|e| e.name.as_str().to_string())
            .collect();
        (d.export_snapshot().archive_name, src, names)
    };
    assert_eq!(snap_name, "child.tar", "内层会话标题 = 容器条目名");
    let src = src.expect("嵌套会话必须有源文件（物化副本）");
    assert!(
        src.starts_with(
            data.lock()
                .unwrap()
                .temp_area
                .as_ref()
                .unwrap()
                .session_dir()
        ),
        "会话源应落在会话临时目录：{}",
        src.display()
    );
    let len = std::fs::metadata(&src).expect("源文件应存在").len();
    assert_eq!(
        len,
        inner_bytes.len() as u64,
        "物化副本必须完整（截断 = 内层内容不对）"
    );
    // 会话目录里**只该有这一份**副本（spool 已被移动，不残留第二份同内容文件）
    let session_dir = data
        .lock()
        .unwrap()
        .temp_area
        .as_ref()
        .unwrap()
        .session_dir()
        .to_path_buf();
    let mut files = 0usize;
    let mut stack = vec![session_dir];
    while let Some(p) = stack.pop() {
        for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().map(|n| n != "session.json") == Some(true) {
                files += 1;
            }
        }
    }
    assert_eq!(
        files, 1,
        "会话目录里应只有一份副本（旧的 spool+物化会留两份）"
    );
    assert!(
        entries.iter().any(|n| n == "deep/inner.txt"),
        "内层条目应可读：{entries:?}"
    );
}

/// 套娃包装必须显式提示：进到「只有一行、且与本条目同名」的层时投递通知。
///
/// 用户实测 2026-10-22：`X.qcow2.zip` 里的 `X.qcow2` 本身还是 zip（里面又有一个
/// 同名 `X.qcow2`）——进入后看到「还是那个 qcow2」，用户读作「没打开」。这里守
/// 提示的触发条件（单条目 + 同名 + 非目录），不再让用户靠猜。
#[gpui_kit::test]
fn entering_a_single_entry_wrapper_notifies_the_user(cx: &mut gpui_kit::TestAppContext) {
    use std::io::Write;

    let (tx, rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(256);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine,
    )));
    {
        let mut d = data.lock().expect("lock");
        d.signal_tx = Some(tx.clone());
        d.temp_area = Some(Arc::new(
            hzip_platform::temp_area::TempArea::new_session(None).expect("temp area"),
        ));
    }
    let data_for_window = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let app_shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_for_window.clone()),
            Some(tx.clone()),
        );
        let app_shell = cx.new(|_| app_shell);
        *slot2.borrow_mut() = Some(app_shell.clone());
        let view: gpui_kit::AnyView = app_shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    // 夹具：outer.zip → inner.zip（真身是 tar，但名字仍叫 inner.zip）
    let dir = tempfile::TempDir::new().expect("tempdir");
    let tar = dir.path().join("payload.tar");
    crate::viewmodel::testutil::write_ustar(&tar, &[("real/file.txt", b"payload")]);
    let tar_bytes = std::fs::read(&tar).expect("读 tar");
    let make_zip = |path: &std::path::Path, name: &str, bytes: &[u8]| {
        let mut w = hzip_format::FormatHandler::create_writer(
            &hzip_fmt_zip::ZipFormatHandler::new(),
            hzip_format::Sink::File(path.to_path_buf()),
            &hzip_format::CreateOptions::default(),
        )
        .expect("create");
        let meta = hzip_format::EntryMeta {
            name: name.into(),
            size_raw: Some(bytes.len() as u64),
            ..Default::default()
        };
        let mut s = w
            .next_entry(&meta, hzip_format::PasswordRef::none())
            .expect("entry");
        s.write_all(bytes).expect("write");
        s.finish().expect("finish entry");
        w.finish().expect("finish archive");
    };
    let inner = dir.path().join("inner.zip");
    make_zip(&inner, "inner.zip", &tar_bytes);
    let inner_bytes = std::fs::read(&inner).expect("读 inner");
    let outer = dir.path().join("outer.zip");
    make_zip(&outer, "inner.zip", &inner_bytes);

    allow_parking_for_real_io(vtcx);
    let task = vtcx.update(|_, cx| {
        crate::ui::shell::open_archive_task(cx, data.clone(), outer.clone(), false)
    });
    vtcx.update(|_, cx| {
        cx.spawn(async move |_| {
            let _ = task.await;
        })
        .detach();
    });
    let wait = |vtcx: &mut gpui_kit::VisualTestContext, what: &str, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}：20s 内未完成");
            std::thread::sleep(std::time::Duration::from_millis(10));
            vtcx.run_until_parked();
        }
    };
    {
        let probe = data.clone();
        wait(vtcx, "打开外层档案", &|| {
            probe.lock().map(|d| d.main.is_open()).unwrap_or(false)
        });
    }
    vtcx.update(|_, cx| {
        shell.update(cx, |s, cx| s.open_container("inner.zip", cx));
    });
    {
        let probe = data.clone();
        wait(vtcx, "进入包装层", &|| {
            probe
                .lock()
                .map(|d| d.export_snapshot().nested)
                .unwrap_or(false)
        });
    }

    let want_title = crate::tr("notify.wrapper_level.title");
    let mut found = false;
    while let Ok(sig) = rx.try_recv() {
        if let crate::viewmodel::VmSignal::Notify(ev) = sig
            && ev.title == want_title
        {
            assert!(
                ev.body.contains("inner.zip"),
                "提示应点名那个同名条目：{}",
                ev.body
            );
            found = true;
        }
    }
    assert!(found, "进入单条目同名包装层必须提示用户再双击一次");
}

/// 任意文件都能「按内容」打开（用户口径 2026-10-26）：
/// 改了后缀名的压缩包（`.bin`）必须直接打开，不靠扩展名。
#[gpui_kit::test]
fn opens_a_renamed_archive_by_content(cx: &mut gpui_kit::TestAppContext) {
    use std::io::Write;

    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(128);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine,
    )));
    data.lock().expect("lock").signal_tx = Some(tx.clone());
    let data_for_window = data.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let app_shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_for_window.clone()),
            None,
        );
        let app_shell = cx.new(|_| app_shell);
        let view: gpui_kit::AnyView = app_shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });

    // 夹具：真 zip，存成 `.bin`（后缀完全不像压缩包）
    let dir = tempfile::TempDir::new().expect("tempdir");
    let renamed = dir.path().join("payload.bin");
    {
        let mut w = hzip_format::FormatHandler::create_writer(
            &hzip_fmt_zip::ZipFormatHandler::new(),
            hzip_format::Sink::File(renamed.clone()),
            &hzip_format::CreateOptions::default(),
        )
        .expect("create");
        let meta = hzip_format::EntryMeta {
            name: "inside.txt".into(),
            size_raw: Some(5),
            ..Default::default()
        };
        let mut s = w
            .next_entry(&meta, hzip_format::PasswordRef::none())
            .expect("entry");
        s.write_all(b"hello").expect("write");
        s.finish().expect("finish entry");
        w.finish().expect("finish archive");
    }

    allow_parking_for_real_io(vtcx);
    let outcome = Arc::new(std::sync::Mutex::new(None::<Result<(), String>>));
    let out2 = outcome.clone();
    let task = vtcx.update(|_, cx| {
        crate::ui::shell::open_archive_task(cx, data.clone(), renamed.clone(), false)
    });
    vtcx.update(|_, cx| {
        cx.spawn(async move |_| {
            *out2.lock().unwrap() = Some(task.await);
        })
        .detach();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while outcome.lock().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "改名档案 20s 内未完成打开"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        vtcx.run_until_parked();
    }
    let res = outcome.lock().unwrap().clone().expect("已完成");
    assert!(res.is_ok(), "按内容识别必须成功打开：{res:?}");
    let names: Vec<String> = data
        .lock()
        .unwrap()
        .main
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert_eq!(names, vec!["inside.txt"], "必须按魔数认出这是 zip");
}

/// 真不是压缩包 → 必须给出人话错误（点名文件 + 说明按内容识别），
/// 而不是把 `HZ-2001 … bad signature` 直接甩给用户。
#[gpui_kit::test]
fn non_archive_reports_a_human_readable_error(cx: &mut gpui_kit::TestAppContext) {
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(128);
    let tx = Arc::new(tx);
    let engine = Arc::new(Engine::new(EngineConfig::default()).expect("engine"));
    let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine,
    )));
    data.lock().expect("lock").signal_tx = Some(tx.clone());
    let data_for_window = data.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let app_shell = AppShell::with_data(
            cx,
            ShellHandlers::default(),
            Some(data_for_window.clone()),
            None,
        );
        let app_shell = cx.new(|_| app_shell);
        let view: gpui_kit::AnyView = app_shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });

    let dir = tempfile::TempDir::new().expect("tempdir");
    let plain = dir.path().join("notes.bin");
    std::fs::write(&plain, b"definitely not an archive, just text").expect("write");

    allow_parking_for_real_io(vtcx);
    let outcome = Arc::new(std::sync::Mutex::new(None::<Result<(), String>>));
    let out2 = outcome.clone();
    let task = vtcx.update(|_, cx| {
        crate::ui::shell::open_archive_task(cx, data.clone(), plain.clone(), false)
    });
    vtcx.update(|_, cx| {
        cx.spawn(async move |_| {
            *out2.lock().unwrap() = Some(task.await);
        })
        .detach();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while outcome.lock().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "非档案 20s 内应失败返回"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        vtcx.run_until_parked();
    }
    let msg = outcome
        .lock()
        .unwrap()
        .clone()
        .expect("已完成")
        .expect_err("非压缩包必须失败");
    assert!(msg.contains("notes.bin"), "必须点名文件：{msg}");
    assert!(
        msg.contains("不是可识别的压缩包"),
        "必须说人话（说明按内容识别），而不是裸错误码：{msg}"
    );
}
