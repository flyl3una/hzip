//! Global 注册表：JobId → 进度窗口槽（防重复开 + 泵分发目标 + 判活/关窗句柄）。
//!
//! 从 `views/progress/mod.rs` 迁出（T-STD：`mod.rs` 只做声明与再导出）；内容逐字未改。

use crate::ui::constant::{
    PROGRESS_WINDOW_H, PROGRESS_WINDOW_MIN_H, PROGRESS_WINDOW_MIN_W, PROGRESS_WINDOW_W,
};
use crate::viewmodel::JobRow;
use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, SharedString, WindowHandle, size};

use super::ProgressWindow;
use super::file_list;

/// Global 注册表（JobId → 窗口；防重复开 + 泵分发目标 + 判活/关窗句柄）。
#[derive(Default)]
pub(super) struct ProgressGlobal(pub(super) std::collections::HashMap<u64, ProgressSlot>);

impl gpui_kit::Global for ProgressGlobal {}

/// 单任务窗口槽：实体（数据灌入）+ 句柄（判活/关窗）+ 延迟关窗截止时刻。
#[derive(Clone)]
pub(super) struct ProgressSlot {
    pub(super) entity: Entity<ProgressWindow>,
    handle: WindowHandle<ProgressWindow>,
    /// 终局延迟关窗截止时刻（勾选「完成后退出」时首次观察到终局即排定；
    /// None = 未排定/不自动关）。到点由 [`tick_pending_closes`] 关窗——
    /// 终局后不再有行变化信号，分发门控会停，必须靠兜底 tick 驱动。
    pub(super) close_at: Option<std::time::Instant>,
}

/// 「完成后退出」的**有效值**（用户口径 2026-09-29 第 ⑧ 条）。
///
/// = `[jobs].exit_on_finish` ∧ **本进程允许自动退出**（`ALLOW_QUIT_ON_FINISH`：
/// 只有 Direct 模式——右键动词进程，无主界面——置位）。主界面里这个开关对进度窗
/// 与进程**一律无效**：业务操作完成后不得把 hzip-gui 带走；复选框也随之隐藏
/// （勾了没反应＝谎报，故不显示假状态）。
pub(crate) fn exit_on_finish_effective() -> bool {
    crate::app::pump_host::ALLOW_QUIT_ON_FINISH.load(std::sync::atomic::Ordering::Relaxed)
        && hzip_config::Config::load().jobs.exit_on_finish
}

/// 开进度窗口（配置开关由调用方判定；同 job 已开且存活 → 仅聚焦）。
///
/// `row`：任务行快照（Queued 期开窗即有概要信息）；`on_cancel`：取消上行
/// （与任务中心取消同通道语义——`jobs.cancel(job_id)`）。
pub fn open_progress_window(
    cx: &mut App,
    row: &JobRow,
    on_cancel: impl Fn(u64, &mut App) + 'static,
    on_exit_toggle: impl Fn(bool, &mut App) + 'static,
) {
    // 防重复开：死窗清理（判活失败 → 移除）+ 存活同 job → 仅聚焦
    let ids: Vec<(u64, WindowHandle<ProgressWindow>)> = cx
        .try_global::<ProgressGlobal>()
        .map(|g| {
            g.0.iter()
                .map(|(id, s)| (*id, s.handle))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (id, handle) in ids {
        if handle.update(cx, |_, _, _| {}).is_err() {
            cx.global_mut::<ProgressGlobal>().0.remove(&id);
        }
    }
    if let Some(slot) = cx
        .try_global::<ProgressGlobal>()
        .and_then(|g| g.0.get(&row.job_id))
        .cloned()
    {
        slot.entity.update(cx, |_, cx| cx.notify());
        return;
    }
    let title = format!(
        "{} {}",
        crate::ui::binding::job_kind_label(row.kind),
        row.src
    );
    let summary = (row.src.clone(), row.dest.clone().unwrap_or_default());
    // 输出区 = 虚拟列表数据面（OutputState 无 Window 依赖）；本体实体先建
    // （output 占位 empty），目标窗口构建闭包内 sync_full 回填已有数据
    // （任务中心重开/快照领先时输出区直接呈现历史——实体跨窗口合法，T-PW §5）。
    let entity = cx.new(|_| ProgressWindow {
        job_id: row.job_id,
        title: title.clone(),
        summary,
        row: row.clone(),
        output: None,
        output_tab: file_list::OutputTab::default(),
        on_cancel: Box::new(on_cancel),
        exit_on_finish: exit_on_finish_effective(),
        allow_quit: crate::app::pump_host::ALLOW_QUIT_ON_FINISH
            .load(std::sync::atomic::Ordering::Relaxed),
        on_exit_toggle: Box::new(on_exit_toggle),
    });
    let options = gpui_kit::WindowOptions {
        window_bounds: Some(gpui_kit::WindowBounds::Windowed(
            gpui_kit::Bounds::centered(None, size(PROGRESS_WINDOW_W, PROGRESS_WINDOW_H), cx),
        )),
        window_min_size: Some(size(PROGRESS_WINDOW_MIN_W, PROGRESS_WINDOW_MIN_H)),
        is_resizable: true,
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some(SharedString::from(title)),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        ..Default::default()
    };
    let job_id = row.job_id;
    let row0 = row.clone();
    let handle = cx
        .open_window(options, |_window, cx| {
            // 输出区数据面全量同步（虚拟列表行集 + 滚底）
            let mut out = file_list::OutputState::new();
            out.sync_full(&row0);
            entity.update(cx, |w, cx| {
                w.output = Some(out);
                cx.notify();
            });
            entity.clone()
        })
        .unwrap_or_else(|e| {
            let job_id_s = job_id.to_string();
            let err_s = e.to_string();
            let msg = crate::tr_with(
                "progress.window_create_failed",
                &[("job_id", job_id_s.as_str()), ("err", err_s.as_str())],
            );
            panic!("{msg}")
        });
    // 新开的输出窗必须**主动激活**：直接模式下本进程由资源管理器拉起，新窗口
    // 默认落在后台（用户看不到，以为「点了没反应」）。activate_window 会把窗口
    // 提到前台并聚焦（平台层 SetForegroundWindow 语义）。
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    cx.default_global::<ProgressGlobal>().0.insert(
        job_id,
        ProgressSlot {
            entity,
            handle,
            close_at: None,
        },
    );
}

/// 活窗口槽枚举（判活 + 行查找；`dispatch_rows`/`relocalize` 共用）。
///
/// 返回 `(job_id, 窗口句柄, 该任务当前行)`；判活失败的槽就地移除，行已不
/// 存在的槽就地关窗（不在返回值里）。
pub(super) fn live_slots(
    cx: &mut App,
    rows: &[JobRow],
) -> Vec<(u64, WindowHandle<ProgressWindow>, JobRow)> {
    if !cx.has_global::<ProgressGlobal>() {
        return Vec::new();
    }
    let slots: Vec<(u64, WindowHandle<ProgressWindow>)> = cx
        .global::<ProgressGlobal>()
        .0
        .iter()
        .map(|(k, s)| (*k, s.handle))
        .collect();
    let mut live = Vec::with_capacity(slots.len());
    for (job_id, any) in slots {
        // 判活：窗口已被用户关 → update Err → 条目移除
        if any.update(cx, |_, _, _| {}).is_err() {
            cx.global_mut::<ProgressGlobal>().0.remove(&job_id);
            continue;
        }
        let Some(row) = rows.iter().find(|r| r.job_id == job_id) else {
            // 行已被清除（任务中心「清除已完成」）→ 窗口随行退场
            close_slot(cx, job_id);
            continue;
        };
        live.push((job_id, any, row.clone()));
    }
    live
}

/// 注册表收口：移除条目 + 关窗（窗口存活时）。
pub(super) fn close_slot(cx: &mut App, job_id: u64) {
    let Some(slot) = cx.global_mut::<ProgressGlobal>().0.remove(&job_id) else {
        return;
    };
    let _ = slot.handle.update(cx, |_, window, _| {
        window.remove_window();
    });
}
