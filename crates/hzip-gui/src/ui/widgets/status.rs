//! 状态类公共件（T-G50 §3.3 自 app.rs 迁入；T-G56 组件对齐 gpui-kit）。
//!
//! busy 条（`h_flex` 保留 + 内嵌 `Spinner`）、状态栏（`StatusBar` 组件——
//! `.left()/.right()` 槽内建）、任务胶囊（容器保留自组——应用特定，内部
//! `Progress` + `on_pill_click` 回调切任务中心页签）。

use crate::ui::constant::PILL_PROGRESS_REL;
use crate::ui::spacing::sp;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, IntoElement, ParentElement, SharedString, Styled, div, relative};

use crate::ui::contracts::{JobPill, SelectionStats};

/// busy 条（T42 语义：工具栏下方细条；busy=false 渲染零高度占位。
/// T-G56 #13：内嵌 `Spinner` 动效）。
pub fn busy_bar(busy: bool, busy_text: &str, cx: &App) -> AnyElement {
    if busy {
        v_flex()
            .flex_shrink_0()
            .px(sp(2.))
            .py_0p5()
            .bg(cx.theme().selection)
            .child(
                h_flex()
                    .gap(sp(2.))
                    .items_center()
                    .child(Spinner::new().with_size(gpui_kit::component::Size::Small))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(busy_text.to_owned())),
                    ),
            )
            .into_any_element()
    } else {
        div().flex_shrink_0().into_any_element()
    }
}

/// 状态栏（左侧计数文案 + 右侧任务胶囊；T-G56 #12 换 `StatusBar`——
/// `.left()/.right()` 槽内建边框/底色/布局）。
///
/// 左槽文案（T-SEL2）：有可见选中（`sel_stats` 非空——可见行交集口径）→
/// 「已选 {files} 个文件，{dirs} 个文件夹，{size}（压缩后：{packed}）」；
/// 无选中 → 原「{entries} 个条目 · 已选 {selected} · 可疑编码 {suspect}」。
/// 大小为自适应人类可读格式（binding `fmt_bytes`，与列口径一致）；
/// `packed` 合计 0 = 未计算 → 占位「—」。
#[allow(clippy::too_many_arguments)]
pub fn status_bar(
    entry_count: usize,
    selection_count: usize,
    suspect_count: usize,
    sel_stats: SelectionStats,
    pill: &Option<JobPill>,
    cx: &App,
) -> AnyElement {
    let has_visible_selection = sel_stats.files + sel_stats.dirs > 0;
    let left = if has_visible_selection {
        let packed = if sel_stats.size_packed > 0 {
            crate::ui::binding::fmt_bytes(sel_stats.size_packed)
        } else {
            crate::ui::binding::NO_VALUE.to_string()
        };
        crate::tr_with(
            "status.selected",
            &[
                ("files", &sel_stats.files.to_string()),
                ("dirs", &sel_stats.dirs.to_string()),
                ("size", &crate::ui::binding::fmt_bytes(sel_stats.size_raw)),
                ("packed", &packed),
            ],
        )
    } else {
        crate::tr_with(
            "status.counts",
            &[
                ("entries", &entry_count.to_string()),
                ("selected", &selection_count.to_string()),
                ("suspect", &suspect_count.to_string()),
            ],
        )
    };
    StatusBar::new()
        .flex_shrink_0()
        .left(
            div()
                .flex()
                .gap(sp(1.))
                .items_center()
                .child(SharedString::from(left))
                .when(suspect_count > 0, |this| {
                    this.child(
                        gpui_kit::component::Icon::new(crate::ui::widgets::AppIcon::Warn)
                            .size_3()
                            .text_color(cx.theme().warning),
                    )
                }),
        )
        .right(div())
        .children(job_pill_element(pill, cx))
        .into_any_element()
}

/// 任务胶囊（第一条运行中任务；None = 隐藏。`on_pill_click` 切任务中心页签
///——T-G56 接线；None = 保持空占位）。
pub fn job_pill_element(pill: &Option<JobPill>, cx: &App) -> Option<AnyElement> {
    job_pill_element_with_click(pill, None, cx)
}

/// 任务胶囊（带点击回调注入——`on_pill_click` 由 app.rs 传 `switch_tab`；
/// 回调不可用（T-G55 集成前）传 None 保持空占位）。
pub fn job_pill_element_with_click(
    pill: &Option<JobPill>,
    on_pill_click: Option<std::rc::Rc<dyn Fn()>>,
    cx: &App,
) -> Option<AnyElement> {
    let pill = pill.as_ref()?;
    let progress = Progress::new("pill-progress")
        .value(pill.progress * 100.)
        .loading(pill.progress <= 0.0);
    let on_click =
        on_pill_click.unwrap_or_else(|| std::rc::Rc::new(|| {}) as std::rc::Rc<dyn Fn()>);
    Some(
        h_flex()
            .id("job-pill")
            .gap(sp(2.))
            .items_center()
            .px(sp(2.))
            // 圆角跟全局主题（`[ui].corner_radius`）
            .rounded(cx.theme().radius)
            .bg(cx.theme().selection)
            .cursor_pointer()
            .child(
                div()
                    .text_xs()
                    .child(SharedString::from(pill.summary.clone())),
            )
            .child(progress.w(relative(PILL_PROGRESS_REL)))
            .on_click(move |_, _, _| on_click())
            .into_any_element(),
    )
}
