//! 每任务事件泵线程（T42 pump_host；02 §11.3 的 R 侧）。
//!
//! T41 `VmPump` 为「单任务独占阻塞」设计且 router 私有，不满足 GUI 多任务
//! 下 UI 线程应答密码/冲突的需求。本模块在 T41 契约上自建宿主循环：
//!
//! - 共享 `JobsViewModel`/`DialogRouter`（Arc<Mutex>），每任务一条线程只
//!   消费**自己 job_id** 的事件；
//! - 事件处理语义照搬 T41 pump（JobStarted 初始化 / Bytes 经
//!   `hzip_progress::Throttle` 10 Hz 合并 / EntryDone 失败记账 /
//!   JobFinished 落账并退出线程）；
//! - 请求路由：密码/冲突 → `router.intake`；intake=false 且会话命中自动
//!   决策 → 直接应答（不弹窗）；密码重试达 `PASSWORD_MAX_RETRIES` → 取消；
//! - `JobHandle` 不跨线程持有（无 Clone）：rx/req_rx 派生后，应答/取消经
//!   `jobs` 锁内引用执行（短临界，锁序 jobs→router 全局一致）。
//! - 变更经 notifier 回投 gpui 主线程（T-G00：make_notifier 已由 contracts
//!   `SignalTx` 版取代——T-G13 提供 `signal_notifier` 桥，机制 = Spike#7）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::Receiver;
use hzip_core::jobs::{ConflictDecision, JobRequest, JobResponse};
use hzip_error::HzipError;
use hzip_progress::{EventReceiver, ProgressEvent, RecvTimeout, Throttle};

use crate::ui::contracts::SignalTx;
use crate::viewmodel::VmSignal;
use crate::viewmodel::jobs_vm::JobsViewModel;
use crate::viewmodel::router::{DialogRouter, PASSWORD_MAX_RETRIES, PasswordDialogState};

/// 泵停止标志（应用退出时置位，线程尽快退出）。
pub static PUMP_STOP: AtomicBool = AtomicBool::new(false);

/// 是否允许「全部任务结束后自动退出进程」（Direct 模式 = 右键动词进程，无主界面）。
///
/// 由启动期按 `launch_mode` 置位（`ui/shell/runtime.rs`；默认 false = 主界面语义：
/// 任何业务操作都不得把程序带走——用户口径 2026-09-29）。
pub static ALLOW_QUIT_ON_FINISH: AtomicBool = AtomicBool::new(false);

/// 启动单任务泵线程。
pub fn spawn_job_pump(
    jobs: Arc<Mutex<JobsViewModel>>,
    router: Arc<Mutex<DialogRouter>>,
    job_id: u64,
    tx: Arc<SignalTx>,
) {
    // 任务泵起点留痕（spawn 失败见下方 error；线程名 gui-pump-{job_id}）
    tracing::info!(job = job_id, "任务泵线程启动");
    if let Err(e) = std::thread::Builder::new()
        .name(format!("gui-pump-{job_id}"))
        .spawn(move || {
            let notifier: Arc<dyn crate::viewmodel::VmNotifier> =
                Arc::new(signal_notifier(tx.clone()));
            // rx/req_rx 从句柄派生（Clone；handle 不跨线程持有）
            let (rx, req_rx) = {
                let jobs = match jobs.lock() {
                    Ok(j) => j,
                    Err(_) => return,
                };
                let Some(handle) = jobs.handle(job_id) else {
                    return; // 理论不可达（submit 已登记）
                };
                (handle.events(), handle.requests().clone())
            };
            let mut throttle = Throttle::new();
            let _ = run_job_loop(
                job_id,
                &jobs,
                &router,
                &rx,
                &req_rx,
                &mut throttle,
                &notifier,
            );
            // 泵退出点（JobFinished 落账或 bus 关闭——任务生命周期终点）
            tracing::info!(job = job_id, "任务泵线程退出");
        })
    {
        // spawn 失败 = 系统资源耗尽；任务账目由退出钩子收敛（不 panic UI 进程）
        tracing::error!("泵线程 spawn 失败 job_id={job_id}: {e}");
    }
}

/// `SignalTx` 版 notifier 桥（T-G13 §3.3：`tx.try_send` 即返回，不阻塞泵线程；
/// 节流逻辑仍归 VmPump/Throttle——10 Hz 语义不变）。
/// 原旧框架版 `state::make_notifier`（事件循环回投）已随迁移删除。
pub(crate) fn signal_notifier(tx: Arc<SignalTx>) -> impl Fn(VmSignal) + Send + Sync + 'static {
    move |sig: VmSignal| {
        let _ = tx.try_send(sig);
    }
}

/// 单任务事件循环（阻塞至 JobFinished）。
#[allow(clippy::too_many_arguments)]
fn run_job_loop(
    job_id: u64,
    jobs: &Arc<Mutex<JobsViewModel>>,
    router: &Arc<Mutex<DialogRouter>>,
    rx: &EventReceiver,
    req_rx: &Receiver<JobRequest>,
    throttle: &mut Throttle,
    notifier: &Arc<dyn crate::viewmodel::VmNotifier>,
) -> Result<(), HzipError> {
    loop {
        if PUMP_STOP.load(Ordering::Relaxed)
            && let Ok(j) = jobs.lock()
            && let Some(h) = j.handle(job_id)
        {
            h.cancel();
        }
        // 20 ms 静默检查点（与 core `need()` 的取消检查节奏一致）：空转等待期
        // **也要排空请求**。解压早期密码门（头加密重开 / 预热密码）的问询发生在
        // 「首个进度事件之后、下一个事件之前」的无事件窗口——只在下个事件到达
        // 时排空，会把密码弹窗饿到 worker 侧 120 s 决策超时（hz-5002，用户侧
        // 只看到「不支持的操作」）。
        let ev = match rx.recv_timeout(std::time::Duration::from_millis(20)) {
            RecvTimeout::Event(ev) => ev,
            RecvTimeout::Timeout => {
                drain_requests(job_id, jobs, router, req_rx, notifier);
                continue;
            }
            RecvTimeout::Closed => return Ok(()), // bus 关（Engine drop）
        };
        // 解压密码门时序（设计稿 11 §4.2）：提交点挂起的进度窗在**首个非
        // JobStarted** 事件才开——JobStarted 由 jobs 起跑线通用发出（先于
        // 头加密/预热密码问询），JobTotal/Entry*/JobFinished 则保证密码已
        // 应答、或任务已终局（失败也要有可见出口）。
        if !matches!(ev, ProgressEvent::JobStarted { .. })
            && let Ok(mut j) = jobs.lock()
            && let Some(force) = j.take_progress_open(job_id)
        {
            notifier(VmSignal::ProgressWindowOpen(job_id, force));
        }
        match &ev {
            ProgressEvent::JobStarted { .. } => {
                if let Ok(mut j) = jobs.lock() {
                    j.on_event(job_id, &ev);
                }
                notifier(VmSignal::JobsChanged);
            }
            ProgressEvent::JobTotal { .. } => {
                // 总量补报（GUI 进度条数据源）：经 throttle 立即冲刷出带真实
                // total 的快照——total 恒 0 时进度恒 None（0%→100% 跳变根因）
                if let Some(snap) = throttle.push(&ev) {
                    if let Ok(mut j) = jobs.lock() {
                        j.apply(job_id, &snap);
                        j.on_event(job_id, &ev);
                    }
                    notifier(VmSignal::JobsChanged);
                }
            }
            ProgressEvent::Bytes { .. } => {
                if let Some(snap) = throttle.push(&ev) {
                    if let Ok(mut j) = jobs.lock() {
                        j.apply(job_id, &snap);
                    }
                    notifier(VmSignal::JobsChanged);
                }
            }
            ProgressEvent::EntryStarted { .. } | ProgressEvent::EntryDone { .. } => {
                if let Ok(mut j) = jobs.lock() {
                    j.on_event(job_id, &ev);
                }
                notifier(VmSignal::JobsChanged);
            }
            ProgressEvent::JobFinished { outcome, .. } => {
                let outcome = outcome.clone();
                // 行终局落账 + 记下 kind（T-OPT 项12：原实现随后为读 `kind`
                // 二次抢同一把锁——`kind` 在 submit 后不变，首锁内取即可）
                let is_edit = {
                    let mut j = jobs.lock().ok();
                    match j.as_mut() {
                        Some(j) => {
                            let is_edit = j.row(job_id).is_some_and(|r| r.kind == "edit");
                            j.on_finished(job_id, &outcome);
                            is_edit
                        }
                        None => false,
                    }
                };
                if let Ok(mut r) = router.lock() {
                    r.on_job_finished(job_id);
                }
                notifier(VmSignal::JobsChanged);
                notifier(VmSignal::DialogChanged);
                // 编辑任务终局：回投 EntriesChanged → 主线程泵批处理里
                // refresh_browse_rows 重开浏览会话（EditBatch 提交前曾释放
                // 句柄保原子替换；成功重开显示新条目集，失败也重开回原态）。
                if is_edit {
                    notifier(VmSignal::EntriesChanged);
                }
                // 任务完成流程（R-GUI4 #3）：通知 + 退出两个开关各自独立触发
                job_finished_hooks(job_id, jobs, notifier);
                return Ok(());
            }
            // #[non_exhaustive] 兜底
            _ => {}
        }
        drain_requests(job_id, jobs, router, req_rx, notifier);
    }
}

/// 任务完成流程钩子（`JobFinished` 落账后调用；R-GUI4 #3）。
///
/// - `[jobs].notify_on_finish` 开 → 组装通知文案经 `VmSignal::Notify` 回投
///   主线程泵（2026-09-15 通知收口：投递目标由 `ui::notify` 组件按「主窗口
///   在场 → 界面 toast / 无主窗口 → 系统通知」判定，本线程不直接投递）。
/// - `[jobs].exit_on_finish` 开 **且** 启动模式允许关窗（`ALLOW_QUIT_ON_FINISH`：
///   仅 Direct = 右键动词进程、无主界面时置位；主界面恒 false）**且** 队列已无
///   活跃任务（active_count == 0，锁内判定防竞态）→ 等待 `[jobs].exit_delay_ms`
///   （默认 1 s，仅配置可调——进度到 100% 后停留展示再退）→ 回投
///   `VmSignal::QuitRequested`
///   （主线程泵消费执行优雅退出——gpui 线程亲和，quit 不可在泵线程直呼；
///   延迟放在本泵线程：主线程继续泵 UI/进度窗，停留期界面照常刷新）。
fn job_finished_hooks(
    job_id: u64,
    jobs: &Arc<Mutex<JobsViewModel>>,
    notifier: &Arc<dyn crate::viewmodel::VmNotifier>,
) {
    let cfg = hzip_config::Config::load();
    let (notify_on, exit_on) = (cfg.jobs.notify_on_finish, cfg.jobs.exit_on_finish);
    if !notify_on && !exit_on {
        return;
    }
    let Ok(j) = jobs.lock() else {
        return;
    };
    // 退出资格（用户口径 2026-09-29：**业务操作不得把程序带走**）——
    // 启动模式（Direct/主界面）由全局量表达，与 PUMP_STOP 同款刻意选择：
    // 不改 run_job_loop/spawn_job_pump 签名（调用点归 ui/shell/dispatch.rs）。
    let finished = j.row(job_id);
    let has_bad_row = j.rows().iter().any(|row| {
        matches!(
            row.state,
            crate::viewmodel::JobRowState::Failed
                | crate::viewmodel::JobRowState::PartialDone
                | crate::viewmodel::JobRowState::Cancelled
        )
    });
    let allow_quit = ALLOW_QUIT_ON_FINISH.load(std::sync::atomic::Ordering::Relaxed);
    let exit_eligible = exit_eligible_after_finish(
        exit_on,
        finished.map(|r| (r.kind, r.state)),
        has_bad_row,
        allow_quit,
    );
    // 通知文案锁内组装（纯字符串克隆，锁持有 ns 级）；通知收口（2026-09-15）：
    // 不再直调 hzip_platform::notification——经 Notify 信号回投主线程，投递
    // 目标由 ui::notify 组件判定（force_system=false = 组件按主窗口在场自动）。
    let notice = notify_on.then(|| j.row(job_id)).flatten().map(|row| {
        let kind = match row.kind {
            "extract" => crate::tr("notify.kind_extract"),
            "compress" => crate::tr("notify.kind_compress"),
            "test" => crate::tr("notify.kind_test"),
            _ => crate::tr("notify.kind_job"),
        };
        let state = match row.state {
            crate::viewmodel::JobRowState::Done => crate::tr("notify.state_done"),
            crate::viewmodel::JobRowState::PartialDone => crate::tr("notify.state_partial_done"),
            crate::viewmodel::JobRowState::Failed => crate::tr("notify.state_failed"),
            crate::viewmodel::JobRowState::Cancelled => crate::tr("notify.state_cancelled"),
            _ => crate::tr("notify.state_finished"),
        };
        let failures = if row.failures.is_empty() {
            String::new()
        } else {
            let n = row.failures.len().to_string();
            crate::tr_with("notify.failures_suffix", &[("n", &n)])
        };
        crate::viewmodel::pump::NotifyEvent {
            title: crate::tr("notify.component_title"),
            body: crate::tr_with(
                "notify.title",
                &[
                    ("kind", &kind),
                    ("job_id", &job_id.to_string()),
                    ("state", &state),
                    ("failures", &failures),
                ],
            ),
            force_system: false,
        }
    });
    let all_done = !j.has_active();
    let exit_eligible = exit_eligible && all_done;
    drop(j); // 通知/回投不占锁
    if let Some(ev) = notice {
        notifier(VmSignal::Notify(ev));
    }
    if exit_eligible {
        // 100% 停留展示：泵线程等满配置延迟再发退出请求（主线程不阻塞——
        // 进度窗/主界面在停留期照常渲染；任务已终局，无活跃工作被拖住）。
        let delay = std::time::Duration::from_millis(cfg.jobs.exit_delay_ms);
        tracing::info!(
            "exit_on_finish：全部任务已结束，{} ms 后请求退出",
            delay.as_millis()
        );
        std::thread::sleep(delay);
        notifier(VmSignal::QuitRequested);
    }
}

/// 「干完活自动关窗」资格（纯函数，便于单测；用户口径 2026-09-29）。
///
/// - `exit_on`：`[jobs].exit_on_finish` 开关（默认 false）；
/// - `finished`：本次终局任务行（None = 行已不在，保守不退）；
/// - `has_bad_row`：队列里存在失败/部分失败/取消行；
/// - `allow_quit`：进程是否处于允许关窗的启动模式（Direct = 右键动词进程，
///   无主界面；主界面语义下恒 false——用户口径 2026-09-29）。
///
/// 四条同时成立才退：
/// ① 本次终局**成功**（Failed/PartialDone/Cancelled 一律不退——用户正在看错误，
///    窗口先消失是最糟的反馈方式；「看起来像崩溃」正是这么来的）；
/// ② 只有**归档类长操作**（解压/压缩/测试/转换）算「干完活自动关窗」的语义来源；
///    编辑（重命名/新建/改内容）、打开等**交互类**任务结束即结束，绝不解散界面；
/// ③ 队列里不得残留任何失败/取消行（同一次会话里失败过就不退）；
/// ④ `allow_quit`：进程是否处于允许关窗的启动模式（主界面 false → 恒不退，
///    即使前三条全成立也不得把整个程序带走）。
fn exit_eligible_after_finish(
    exit_on: bool,
    finished: Option<(&str, crate::viewmodel::JobRowState)>,
    has_bad_row: bool,
    allow_quit: bool,
) -> bool {
    allow_quit
        && exit_on
        && !has_bad_row
        && finished.is_some_and(|(kind, state)| {
            matches!(state, crate::viewmodel::JobRowState::Done)
                && matches!(kind, "extract" | "compress" | "test" | "convert")
        })
}

fn drain_requests(
    job_id: u64,
    jobs: &Arc<Mutex<JobsViewModel>>,
    router: &Arc<Mutex<DialogRouter>>,
    req_rx: &Receiver<JobRequest>,
    notifier: &Arc<dyn crate::viewmodel::VmNotifier>,
) {
    // 锁序 jobs → router（全局一致防死锁）
    let jobs_g = match jobs.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let mut open_dialog = false;
    let mut auto: Option<ConflictDecision> = None;
    let mut cancel_for_retries = false;
    {
        let mut r = match router.lock() {
            Ok(r) => r,
            Err(_) => return,
        };
        // 弹窗互斥串行（请求通道容量 1/任务、router 单槽）：已有待决弹窗
        //（密码/冲突）或浏览密码门在等应答时不 intake——请求留在通道，等当前
        // 弹窗收口后的下一轮再收。否则后到任务的请求会顶掉先到任务的
        // `active_job`，应答被路由到错误的 job，先到任务饿死到 120 s 决策超时。
        let busy = r.dialog_open() || r.gate_attempts().is_some();
        let mut batch = 0usize;
        if !busy {
            while let Ok(req) = req_rx.try_recv() {
                batch += 1;
                if !r.intake(job_id, req) {
                    auto = r.auto_decision_for(job_id);
                } else {
                    open_dialog = true;
                }
            }
        }
        if batch > 0 {
            // 排查辅助高频态：批量计数 + 是否弹窗（intake 结果概要）
            tracing::debug!(job = job_id, batch, open_dialog, "drain_requests 处理请求");
        }
        if let PasswordDialogState::Retry { attempts, .. } = r.password_state()
            && *attempts >= PASSWORD_MAX_RETRIES
            && r.active_job() == Some(job_id)
        {
            cancel_for_retries = true;
        }
        drop(r);
    }
    if let (Some(decision), Some(handle)) = (auto, jobs_g.handle(job_id)) {
        let _ = handle.answer(0, JobResponse::Conflict(decision));
    }
    if cancel_for_retries && let Some(h) = jobs_g.handle(job_id) {
        h.cancel();
    }
    if cancel_for_retries {
        drop(jobs_g);
        notifier(VmSignal::DialogChanged);
        return;
    }
    drop(jobs_g);
    if open_dialog {
        notifier(VmSignal::DialogChanged);
    }
}

// ---- UI forwarder（T-G13 §3.3）----

/// `SignalRx` 消费线程 → 每信号回调（T-G30 注册的 shell 更新闭包）。
///
/// 机制 = Spike#7：smol::channel（async-channel）跨线程 + gpui executor
/// roundtrip。`on_signal` 在 forwarder 线程执行——**必须立即返回**
/// （重活经 `cx.spawn`/`App::queue` 回 gpui 主线程，消费侧注册归 T-G30/G21）。
/// 线程随 `PUMP_STOP` 置位或 channel 关闭退出。
#[allow(dead_code)] // T-G21/T-G30 接线后消费（forwarder 注册归装配层）
pub(crate) fn spawn_ui_forwarder(
    rx: crate::ui::contracts::SignalRx,
    on_signal: impl Fn(VmSignal) + Send + 'static,
) {
    if let Err(e) = std::thread::Builder::new()
        .name("gui-signal-forwarder".into())
        .spawn(move || {
            while let Ok(sig) = smol::block_on(rx.recv()) {
                on_signal(sig);
                if PUMP_STOP.load(Ordering::Relaxed) {
                    break;
                }
            }
        })
    {
        tracing::error!("UI forwarder 线程 spawn 失败: {e}");
    }
}

#[cfg(test)]
#[path = "tests/pump_host.rs"]
mod tests;
