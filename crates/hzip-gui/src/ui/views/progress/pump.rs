//! 泵侧分发：行数据灌入 / 终局延迟关窗排定与到点关窗 / 语言切换重建。
//!
//! 从 `views/progress/mod.rs` 迁出（T-STD：`mod.rs` 只做声明与再导出）；内容逐字未改。

use crate::viewmodel::JobRow;
use gpui_kit::App;

use super::{ProgressGlobal, close_slot, exit_on_finish_effective, live_slots};

// `relocalize` 的 rustdoc 链接（``[`err_text`]``）需要该名在**本模块作用域**内
// 才可解析（rustdoc 不跨模块回退）；业务代码不用它，故只在文档构建期导入。
#[cfg(doc)]
use super::file_list::err_text;

/// 终局自动关判定（勾选「完成后退出」= `[jobs].exit_on_finish`，与进度窗
/// 底部复选框同一配置；开关开时 Done/Cancelled 关窗——Failed/Partial
/// 恒停留，失败明细必须可见）。
pub fn is_close_on_terminal(state: crate::viewmodel::JobRowState, exit_on_finish: bool) -> bool {
    exit_on_finish
        && matches!(
            state,
            crate::viewmodel::JobRowState::Done | crate::viewmodel::JobRowState::Cancelled
        )
}

/// 泵侧分发入口（10 Hz 快照后调用；T-PW README §5）：
///
/// 遍历注册表 → 灌行数据 → 终局按形态收口（勾选「完成后退出」时
/// Done/Cancelled **排定延迟关窗**——进度到 100% 后停留 `[jobs].exit_delay_ms`
/// 再关，由 [`tick_pending_closes`] 到点执行；Failed/Partial 恒停留）。
/// 行被「清除已完成」移除或窗口已被用户关闭 → 条目退场。
pub fn dispatch_rows(cx: &mut App, rows: &[JobRow]) {
    // 「完成后退出」+ 停留时长现读（与设置页/复选框同一配置；每次分发刷新——
    // 改动即时生效，并回灌复选框显示态）
    let cfg = hzip_config::Config::load();
    let exit_on_finish = exit_on_finish_effective();
    let delay = std::time::Duration::from_millis(cfg.jobs.exit_delay_ms);
    for (job_id, any, row) in live_slots(cx, rows) {
        // handle.update 现场 = (&mut V, &mut Window, &mut Context)——数据拷贝
        // 与输出区行集增量推入（虚拟列表数据面无 Window 依赖）。回灌
        // exit_on_finish：设置页改动即时同步到已开的进度窗复选框。
        let _ = any.update(cx, |w, _, cx| {
            w.apply(&row);
            w.exit_on_finish = exit_on_finish;
            cx.notify();
        });
        // 终局形态（T-PW README §3）：勾选「完成后退出」时排定延迟关窗——
        // 幂等（已排定不重置，防每次分发把截止时刻往后顺延）。
        if is_close_on_terminal(row.state, exit_on_finish)
            && cx.has_global::<ProgressGlobal>()
            && let Some(slot) = cx.global_mut::<ProgressGlobal>().0.get_mut(&job_id)
            && slot.close_at.is_none()
        {
            slot.close_at = Some(std::time::Instant::now() + delay);
        }
    }
}

/// 延迟关窗到点检查（主线程泵**每 tick** 调用，含空闲 tick）。
///
/// 与 [`dispatch_rows`] 分离的原因：终局后不再有行变化信号，泵的批次门控
/// （`!batch.is_empty() || state_changed`）不再进分发——排定的关窗必须由
/// 兜底 tick 驱动。10 Hz 精度对秒级停留足够；无排定项时零开销（空表遍历）。
pub fn tick_pending_closes(cx: &mut App) {
    if !cx.has_global::<ProgressGlobal>() {
        return;
    }
    let now = std::time::Instant::now();
    let due: Vec<u64> = cx
        .global::<ProgressGlobal>()
        .0
        .iter()
        .filter(|(_, s)| s.close_at.is_some_and(|at| now >= at))
        .map(|(id, _)| *id)
        .collect();
    for job_id in due {
        close_slot(cx, job_id);
    }
}

/// 语言切换后的输出区重建（`Effect::Locale` 的 UI 半场）。
///
/// 失败原因文案（[`err_text`]）在**灌入时**定稿（`OutLine.tail`），换 locale
/// 不会自动改写既有行 → 必须整表重建（`sync_full`）才能让已展示的输出区
/// 立刻切到新语言。无活窗口时零开销。
pub fn relocalize(cx: &mut App, rows: &[JobRow]) {
    for (_, any, row) in live_slots(cx, rows) {
        let _ = any.update(cx, |w, _, cx| {
            w.relocalize(&row);
            cx.notify();
        });
    }
}
