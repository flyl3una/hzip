use super::*;
use crate::init_locale;
use crate::viewmodel::{FailureRow, JobRowState, RecentEntry};
use std::collections::VecDeque;

fn row_with(failures: Vec<FailureRow>, recent: Vec<RecentEntry>) -> JobRow {
    JobRow {
        job_id: 1,
        kind: "extract",
        state: JobRowState::Running,
        progress: Some(0.5),
        speed_bps: 0.0,
        eta: None,
        done_bytes: 0,
        total_bytes: 0,
        entries_done: 0,
        entries_total: 0,
        failures,
        finished: None,
        src: "demo.zip".into(),
        dest: None,
        format: "zip".into(),
        started_at: None,
        elapsed: None,
        current_entry: None,
        final_line: None,
        recent_entries: VecDeque::from(recent),
    }
}

fn ok_entry(name: &str, bytes: u64) -> RecentEntry {
    RecentEntry {
        name: name.into(),
        bytes,
        ok: true,
        reason: String::new(),
        code: 0,
    }
}

// 结果页投影：成功行带大小、失败行带原因、时序保持
#[test]
fn out_lines_orders_entries_and_reasons() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    init_locale("zh-CN");
    let mut bad = ok_entry("bad.txt", 100);
    bad.ok = false;
    bad.reason = "err.hz_3001".into();
    bad.code = 3001;
    let lines = out_lines(&row_with(
        Vec::new(),
        vec![ok_entry("a.txt", 12697), bad, ok_entry("c.txt", 5)],
    ));
    assert_eq!(lines.len(), 3);
    assert_eq!(
        (lines[0].name.as_str(), lines[0].tail.as_str()),
        ("a.txt", "12.4 KiB")
    );
    assert_eq!((lines[1].name.as_str(), lines[1].failed), ("bad.txt", true));
    assert!(lines[1].tail.contains("校验失败"), "{}", lines[1].tail);
    assert_eq!(lines[2].name, "c.txt");
}

// OutputState::apply 三页签分流：结果页全量、成功/失败页各归其位 +
// 终局描述行殿后（只追加一次）+ 游标增量（重复 apply 不重复推入）
#[test]
fn apply_partitions_tabs_and_appends_summary_once() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    init_locale("zh-CN");
    let mut row = row_with(
        Vec::new(),
        vec![ok_entry("a.txt", 10), {
            let mut bad = ok_entry("bad.txt", 5);
            bad.ok = false;
            bad.reason = "err.hz_3001".into();
            bad.code = 3001;
            bad
        }],
    );
    let mut st = OutputState::new();
    st.apply(&row);
    assert_eq!(st.result.len(), 2, "结果页 = 成功 + 失败");
    assert_eq!(st.ok.len(), 1, "成功页仅 1 行");
    assert_eq!(st.fail.len(), 1, "失败页仅 1 行");
    // 重复 apply（同快照）→ 游标对齐，不重复推入
    st.apply(&row);
    assert_eq!(st.result.len(), 2);
    // 终局：描述行殿后、只一次；失败页不受影响
    row.state = JobRowState::Done;
    st.apply(&row);
    st.apply(&row);
    assert_eq!(st.result.len(), 3, "结果页 = 2 条目 + 终局描述");
    assert_eq!(st.result.last().unwrap().name, "");
    assert!(st.result.last().unwrap().tail.contains("全部完成"));
    assert_eq!(st.fail.len(), 1);
    // sync_full 全量重建等价
    let mut st2 = OutputState::new();
    st2.sync_full(&row);
    assert_eq!(st2.result.len(), 3);
    assert_eq!((st2.ok.len(), st2.fail.len()), (1, 1));
}

// 运行中无终局描述行（Running 态 summary 为空 → 不追加）
#[test]
fn running_has_no_summary_line() {
    let mut st = OutputState::new();
    st.apply(&row_with(Vec::new(), vec![ok_entry("a.txt", 10)]));
    assert_eq!(st.result.len(), 1, "运行中不追加结果描述");
}

// best_effort 跳过条目（无 EntryDone）按序补齐进失败页（防重复）
#[test]
fn best_effort_skipped_failures_appended_once() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    init_locale("zh-CN");
    let mut row = row_with(
        vec![FailureRow {
            entry: "skipped.txt".into(),
            code: 3001,
            message: "err.hz_3001".into(),
        }],
        vec![ok_entry("a.txt", 10)],
    );
    // 同条目已上报 EntryDone{ok:false} → failures 补齐不得重复
    let mut done = ok_entry("skipped.txt", 0);
    done.ok = false;
    done.reason = "err.hz_3001".into();
    done.code = 3001;
    row.recent_entries.push_back(done);
    let lines = out_lines(&row);
    assert_eq!(lines.len(), 2, "skipped.txt 已在列表，不重复补齐");
    let mut st = OutputState::new();
    st.apply(&row);
    assert_eq!(
        st.fail.iter().filter(|l| l.name == "skipped.txt").count(),
        1
    );
}

// 未登记错误码 → err_text 直出 HZ-xxxx 兜底
#[test]
fn err_text_fallback_contains_code() {
    let s = err_text(9999, "");
    assert!(s.contains("HZ-9999") || s.contains("9999"), "{s}");
}

// 页签下标互转（越界回落结果页）
#[test]
fn output_tab_roundtrip() {
    assert_eq!(OutputTab::from_index(0), OutputTab::Result);
    assert_eq!(OutputTab::from_index(2), OutputTab::Fail);
    assert_eq!(OutputTab::from_index(9), OutputTab::Fail);
    assert_eq!(OutputTab::Ok.index(), 1);
}

// ── 输出行折行 & 大框圆角（用户口径 2026-10-16）──

/// 折行实证（**真实 List 路径**）：同一宽度（760px）下按名字长度逐档量**条目**尺寸
/// ——短名保持单行 20px，长名折行后条目随行数长高；且条目宽 = 列表宽（宽度约束必须
/// 传到 item，否则长文本会按 max-content 排成一行再被裁掉——那正是「内容显示不全」）。
///
/// 旧实现行内 `.truncate()`（= nowrap + 省略号）⇒ 长路径只画一行、尾巴被吞。本用例把
/// 它钉在几何上（用 `ListState::bounds_for_item` 读**真实虚拟列表**测出的条目框）。
#[gpui_kit::test]
fn long_names_wrap_and_short_names_stay_one_line(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::{Context, IntoElement, ListAlignment, ListState, Render, Window, list, px};

    /// 名字长度（1/60 = 单行放得下；216 = 真实长路径，必须折行）
    const NAMES: [usize; 4] = [1, 20, 60, 216];
    /// 输出框/列表宽（真实窗口内容列量级）
    const WIDTH: f32 = 760.;

    struct Probe {
        state: ListState,
    }
    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let st = self.state.clone();
            div()
                .w(px(WIDTH))
                .h(px(400.))
                .debug_selector(|| "wrap-host".into())
                .child(list(st, move |ix, _w, cx| {
                    let line = OutLine {
                        name: "a".repeat(NAMES[ix]),
                        tail: "12.4 KiB".into(),
                        failed: false,
                    };
                    out_row(OutputTab::Result, &line, cx).into_any_element()
                }))
        }
    }

    let shared = ListState::new(NAMES.len(), ListAlignment::Top, OUTPUT_LIST_OVERDRAW);
    let probe = shared.clone();
    let (host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        Probe { state: shared }
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let mut single = 0;
    let mut wrapped = 0;
    for (ix, n) in NAMES.iter().enumerate() {
        let b = probe.bounds_for_item(ix).expect("条目已测量");
        assert_eq!(
            px(WIDTH),
            b.size.width,
            "{n} 字符：条目宽必须 = 列表宽（否则长文本按 max-content 排一行后被裁）"
        );
        assert!(
            b.size.height >= MONO_ROW_H,
            "{n} 字符：条目高不得低于单行 {MONO_ROW_H:?}，实得 {:?}",
            b.size.height
        );
        if b.size.height == MONO_ROW_H {
            single += 1;
        } else {
            wrapped += 1;
        }
    }
    assert!(single >= 3, "短名（1/20/60 字符）必须仍是单行（20px）");
    assert!(
        wrapped >= 1,
        "长名（216 字符）必须折行（条目高 > 20px）——否则又退回「一行截断」"
    );
    drop(host);
}
