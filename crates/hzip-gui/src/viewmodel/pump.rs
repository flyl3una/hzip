//! 事件消费循环（02 §11.3 / §4.3；T41）。
//!
//! [`VmPump`] 是唯一持 channel 的组件：订阅 `JobHandle` 事件流，经
//! [`hzip_progress::Throttle`]（10 Hz 语义）合并高频 `Bytes` 事件 → 刷新
//! [`JobsViewModel`]；`JobRequest`（密码/冲突）路由进 [`DialogRouter`]；
//! 终局后经 [`VmNotifier`] 纯回调通知 UI（gpui/gpui_kit 侧 T-G30 桥接）。
//!
//! `run()` 阻塞至该任务终局（`JobFinished` 落账）——自然退出无线程泄漏。

use std::sync::Arc;

use crossbeam_channel::Receiver;
use hzip_core::jobs::{JobHandle, JobRequest};
use hzip_error::HzipError;
use hzip_progress::JobId;
use hzip_progress::{EventReceiver, ProgressEvent, ProgressSnapshot, Throttle};

use super::jobs_vm::JobsViewModel;
use super::router::DialogRouter;

/// 用户通知事件载荷（2026-09-15 重构：通知收口组件 `app::notify` 的跨线程
/// 载体——泵线程产出，主线程泵消费执行界面/系统投递）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyEvent {
    /// 通知标题（任务完成 = i18n 组装好的成品文案）
    pub title: String,
    /// 通知正文
    pub body: String,
    /// 强制系统通知（Auto 判定 = 主窗口在场走界面 toast，否则系统通知；
    /// 此位为 true 时无视在场判定直走系统通知——direct 模式专用）
    pub force_system: bool,
}

/// UI 通知信号（增量刷新；T42 消费转 gpui_kit 状态更新）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmSignal {
    /// 任务行/进度变化
    JobsChanged,
    /// 弹窗状态变化（密码/冲突）
    DialogChanged,
    /// 条目/过滤/树变化
    EntriesChanged,
    /// 请求退出应用（R-GUI4 #3：全部任务完成 + [jobs].exit_on_finish 开）。
    /// 由泵线程发出、主线程泵消费（gpui 线程亲和——quit 必须在主线程）。
    QuitRequested,
    /// 设置项已生效（T-SET §1.2 第五步）：数据面 + 落盘已完成，UI 半场消费
    /// （主题 `apply` / 主窗口快照刷新）。`Deferred` 项（格式/级别）不发。
    SettingApplied(super::setting_id::SettingId),
    /// 用户通知投递（2026-09-15 重构：全部业务通知经 `app::notify` 组件
    /// 收口；泵线程产出，主线程执行 gpui Notification / 系统通知）。
    Notify(NotifyEvent),
    /// 请求主线程开进度窗口（编辑等后台任务提交后的用户可见出口；窗口
    /// 线程亲和——泵/对话框闭包线程不可直开，经主线程泵消费）。
    OpenProgress(u64),
    /// 解压进度窗挂起消费（泵发：首个非 JobStarted 事件已到 = 密码已应答
    /// 或任务已终局；`force` 来自提交点登记——true 强制显示、false 遵守配置）。
    ProgressWindowOpen(u64, bool),
    /// 菜单数据版本递增（T-MENU4：MRU 变化 → 菜单快照重建）。
    MenuEpochBump,
    /// 浏览行已由后台线程重建完成（2026-09-24 UI 线程阻塞审计 B5）。
    ///
    /// 语义 = 「快照该重导了」而**不是**「该重建行了」：它被
    /// `shell::needs_browse_refresh` 明确排除在重建触发条件之外，避免
    /// 「重建 → 发信号 → 又触发重建」的自激回路。
    BrowseRowsRebuilt,
    /// 文件类型图标已落缓存（T-ICON）。
    ///
    /// 语义与 [`VmSignal::BrowseRowsRebuilt`] 同款：**只**重绘（浏览视图重渲
    /// 即原地换上系统位图），**绝不**触发行重建——同样被
    /// `shell::needs_browse_refresh` 排除。由 `ui::widgets::file_icon` 的探测
    /// 线程每批结果投一次（关旋钮后不再有信号）。
    IconCacheFilled,
    /// 密码门**验证通过、动作已放行**（用户口径 2026-09-28）。
    ///
    /// 门内后台线程发出；主线程泵在**无条件信号循环**里消费（与 `OpenProgress`
    /// 同款——不依赖「快照批次」分支），把待执行动作重跑起来。取「专用信号」
    /// 而非复用 `DialogChanged` 的原因：门放行必须**不依赖**任何批次条件，
    /// 否则用户会看到「输完密码什么都不发生，得再双击一次」。
    GateActionReady,
    /// 标准消息**弹窗**（用户口径 2026-09-28 / 2026-09-29）：载荷 = 级别 + 标题 +
    /// 详情，由 `ui::views::op_window::open_message_window` 统一渲染成可调大小、
    /// 详情可滚动的标准窗口。
    ///
    /// 与 [`VmSignal::Notify`]（界面 toast）区分：这是独立窗口（用户必须看到）。
    /// 密码连错、直接模式失败、文件关联失败都发它。
    ShowMessage(crate::viewmodel::message::MessageDialog),
}

/// UI 通知回调（纯 Rust；gpui/gpui_kit 侧 T-G30 桥接）。
pub trait VmNotifier: Fn(VmSignal) + Send + Sync {}
impl<T: Fn(VmSignal) + Send + Sync> VmNotifier for T {}

/// 事件消费泵（每任务一个实例；`run()` 阻塞至该任务终局）。
pub struct VmPump {
    jobs: JobsViewModel,
    router: DialogRouter,
    throttle: Throttle,
    notifier: Arc<dyn VmNotifier>,
    rx: EventReceiver,
    req_rx: Receiver<JobRequest>,
    /// 句柄（cancel/answer 委托——T42 UI 层用；本泵内只用于 job_id）
    #[allow(dead_code)]
    handle: JobHandle,
    job_id: JobId,
}

impl VmPump {
    /// 构建泵（订阅 handle 事件流 + 请求端）。
    pub fn new(
        jobs: JobsViewModel,
        router: DialogRouter,
        notifier: Arc<dyn VmNotifier>,
        handle: JobHandle,
    ) -> Self {
        let job_id = handle.job_id();
        let rx = handle.events();
        let req_rx = handle.requests().clone();
        Self {
            jobs,
            router,
            throttle: Throttle::new(),
            notifier,
            rx,
            req_rx,
            handle,
            job_id,
        }
    }

    /// 任务行视图（委托）。
    pub fn jobs(&self) -> &JobsViewModel {
        &self.jobs
    }

    /// 弹窗路由（委托）。
    pub fn router(&self) -> &DialogRouter {
        &self.router
    }

    /// 事件接收端引用（测试注入）。
    pub fn events_rx(&self) -> &EventReceiver {
        &self.rx
    }

    /// 运行事件循环至任务终局（阻塞调用线程）。
    pub fn run(&mut self) -> Result<(), HzipError> {
        loop {
            // 事件优先（阻塞 recv——不忙转）
            let ev = match self.rx.recv() {
                Some(ev) => ev,
                None => return Ok(()), // 通道关闭（Engine drop）
            };
            match &ev {
                ProgressEvent::Bytes { .. } => {
                    // 高频事件：经节流器合并（push 返回 snapshot 时机 = 10 Hz flush）
                    if let Some(snap) = self.throttle.push(&ev) {
                        self.apply_snapshot(snap);
                    }
                }
                ProgressEvent::JobFinished { job_id, outcome } => {
                    let outcome = outcome.clone();
                    let job_id = *job_id;
                    self.jobs.on_finished(job_id, &outcome);
                    self.router.on_job_finished(job_id);
                    (self.notifier)(VmSignal::JobsChanged);
                    (self.notifier)(VmSignal::DialogChanged);
                    return Ok(()); // 终局 → run 退出
                }
                ProgressEvent::JobStarted { total_bytes, .. } => {
                    // 初始化 total（经一次空 snapshot 语义——jobs_vm 行已含 total 字段）
                    self.jobs.on_event(self.job_id, &ev);
                    let _ = total_bytes;
                    (self.notifier)(VmSignal::JobsChanged);
                }
                ProgressEvent::JobTotal { .. } => {
                    // 总量补报：throttle 立即冲刷出带真实 total 的快照 + 条目总数生效
                    if let Some(snap) = self.throttle.push(&ev) {
                        self.apply_snapshot(snap);
                    }
                    self.jobs.on_event(self.job_id, &ev);
                }
                ProgressEvent::EntryStarted { .. } | ProgressEvent::EntryDone { .. } => {
                    self.jobs.on_event(self.job_id, &ev);
                    (self.notifier)(VmSignal::JobsChanged);
                }
                // #[non_exhaustive] 兜底
                _ => {}
            }
            // 请求路由（非阻塞排空——容量 1 单待决）
            while let Ok(req) = self.req_rx.try_recv() {
                let job_id = self.job_id;
                if self.router.intake(job_id, req) {
                    (self.notifier)(VmSignal::DialogChanged);
                }
            }
        }
    }

    /// 节流快照 → 行状态（10 Hz 合并）。
    fn apply_snapshot(&mut self, snap: ProgressSnapshot) {
        self.jobs.apply(self.job_id, &snap);
        (self.notifier)(VmSignal::JobsChanged);
    }
}

/// 便捷构造：单任务泵（jobs_vm + router + notifier + handle → VmPump）。
pub fn single_job_pump(
    jobs: JobsViewModel,
    handle: JobHandle,
    on_signal: impl Fn(VmSignal) + Send + Sync + 'static,
) -> VmPump {
    VmPump::new(jobs, DialogRouter::new(), Arc::new(on_signal), handle)
}
