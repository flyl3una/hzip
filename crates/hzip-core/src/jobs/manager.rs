//! JobManager：队列/状态机/门控/级联取消的落账侧，与 worker 侧 `JobCtx` 回调解口。
//!
//! 从 `jobs/mod.rs` 迁出（T-STD：`mod.rs` 只做声明与再导出）；内容逐字未改。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};
use hzip_error::HzipError;
use hzip_format::FormatRegistry;
use hzip_progress::{CancellationToken, JobId, JobStats, Outcome, ProgressBus, ProgressEvent};

use crate::engine::pool::Pool;
use crate::services;

use super::consts::{REQUEST_CHANNEL_CAP, REQUEST_TIMEOUT_SECS};
use super::handle::JobHandle;
use super::request::{JobRequest, JobResponse, RequestId, decision_timeout_err};
use super::sched::{pick_dispatchable, resource_keys};
use super::slot::{JobSlot, JobSpec};
use super::state::{self, JobState, Transition};

/// 任务执行上下文（worker 侧回调接口；T32/T33 services 消费）。
///
/// worker 经本类型与 JobManager 通信：状态迁移、请求上抛/应答、进度、取消。
/// services 实现（T32/T33）只依赖本接口，不直接触碰 JobManager 内部。
pub struct JobCtx {
    /// 任务 ID
    pub job_id: JobId,
    /// 取消令牌（子令牌）
    pub cancel: CancellationToken,
    /// 进度总线（Arc 共享；services 经 ctx 发事件）
    pub progress: Arc<ProgressBus>,
    /// 格式注册表（open_reader/create_writer 入口）
    pub registry: Arc<FormatRegistry>,
    /// 内部迁移通道（worker → JobManager 调度线程）
    tx: Sender<JobMsg>,
    /// 请求-应答状态（need/answer）
    req_tx: Sender<JobRequest>,
    ans_rx: Receiver<JobResponse>,
    /// 待决请求 ID（单调）
    next_req_id: AtomicU64,
    /// 当前待决请求 id（与 [`JobSlot::pending_req`] 同一登记位）
    pending_req: Arc<Mutex<Option<RequestId>>>,
    /// 决策超时（测试可缩短）
    req_timeout: Duration,
}

impl JobCtx {
    /// 标记 WaitingInput（worker 上抛请求前调用）。
    pub(crate) fn set_waiting(&self) {
        let _ = self.tx.send(JobMsg::Transition {
            job_id: self.job_id,
            t: Transition::WaitInput,
        });
    }

    /// 标记回 Running（应答到达后）。
    pub(crate) fn back_to_running(&self) {
        let _ = self.tx.send(JobMsg::Transition {
            job_id: self.job_id,
            t: Transition::Resume,
        });
    }

    /// 提交终局（Done/Partial/Failed/Cancelled）。
    pub(crate) fn finish(&self, outcome: Outcome) {
        let t = match &outcome {
            Outcome::Done(_) => Transition::FinishDone,
            Outcome::Partial(..) => Transition::FinishPartial,
            Outcome::Cancelled | Outcome::RolledBack(_) => Transition::FinishCancel,
            Outcome::Failed(_) => Transition::FinishFailed,
            // #[non_exhaustive] 兜底（Outcome 只增变体）
            _ => Transition::FinishFailed,
        };
        let _ = self.tx.send(JobMsg::Finish {
            job_id: self.job_id,
            t,
            outcome,
        });
    }

    /// 需要用户决策：上抛请求并阻塞等应答（容量 1 + 超时）。
    ///
    /// 返回 `Ok(resp)` 应答；`Err` = 决策超时（5002）或通道断（按超时处理）。
    pub fn need(&self, req: JobRequest) -> Result<JobResponse, HzipError> {
        // 先标记 WaitingInput（在 send 前——send 可能短暂阻塞，UI 取走即应答）
        self.set_waiting();
        let id = self.next_req_id.fetch_add(1, Ordering::Relaxed);
        // **先登记再上抛**：请求一旦可见，用户就可能已经应答——登记位是
        // `answer` 放行的同步判据（顺序反了会把应答丢在窗口里）
        *self.pending_req.lock().unwrap_or_else(|p| p.into_inner()) = Some(id);
        if self.req_tx.send(req).is_err() {
            *self.pending_req.lock().unwrap_or_else(|p| p.into_inner()) = None;
            self.back_to_running();
            return Err(decision_timeout_err());
        }
        // 等应答：select 取消（每 20ms 探一次令牌）+ 超时裁决。不忙转——
        // try_recv 空时休眠 20ms（取消检查点间隔，02 §4.2 语义）。
        let deadline = std::time::Instant::now() + self.req_timeout;
        loop {
            if self.cancel.is_cancelled() {
                *self.pending_req.lock().unwrap_or_else(|p| p.into_inner()) = None;
                self.back_to_running();
                return Err(HzipError::Cancelled);
            }
            match self.ans_rx.try_recv() {
                Ok(resp) => {
                    self.back_to_running();
                    *self.pending_req.lock().unwrap_or_else(|p| p.into_inner()) = None;
                    return Ok(resp);
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    if std::time::Instant::now() >= deadline {
                        *self.pending_req.lock().unwrap_or_else(|p| p.into_inner()) = None;
                        return Err(decision_timeout_err());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    *self.pending_req.lock().unwrap_or_else(|p| p.into_inner()) = None;
                    return Err(decision_timeout_err());
                }
            }
        }
    }

    /// 应答校验（JobHandle::answer 委托）：id 匹配当前待决且处于 WaitingInput。
    #[allow(dead_code)] // JobHandle::answer 骨架期未接；T32 请求-应答全链激活
    pub(crate) fn validate_answer(&self, req_id: RequestId) -> bool {
        let cur = self.pending_req.lock().unwrap_or_else(|p| p.into_inner());
        *cur == Some(req_id)
    }

    /// 进度广播（services 经 ctx 发事件；JobStarted/Finished 由 JobManager 发）。
    pub fn progress(&self) -> &ProgressBus {
        &self.progress
    }
}

/// 任务结果（wait() 返回）。
pub type JobResult = Result<Outcome, HzipError>;

/// JobManager 内部消息（worker/Handle → 调度线程）。
enum JobMsg {
    /// 唤醒调度（新任务入队后触发 dispatch——run_loop 无消息时阻塞，
    /// 需显式 kick 而非只 notify cond）
    Kick,
    /// 状态迁移（不含终局）
    Transition { job_id: JobId, t: Transition },
    /// 终局迁移 + outcome
    Finish {
        job_id: JobId,
        t: Transition,
        outcome: Outcome,
    },
}

/// 任务管理器（02 §9.3）。
pub(crate) struct JobManager {
    jobs: Mutex<HashMap<JobId, Arc<Mutex<JobSlot>>>>,
    queue: Mutex<VecDeque<JobId>>,
    cond: Condvar,
    /// 当前运行数（门控）
    running: Mutex<usize>,
    /// 运行中任务占用的**资源键**计数（同键串行；键 = 源/输出档案路径归一）
    running_res: Mutex<HashMap<String, usize>>,
    /// 根取消令牌（Engine 持有；级联）
    root: CancellationToken,
    /// 门控上限 = min(max_parallel, 池大小)
    max_running: usize,
    next_id: AtomicU64,
    /// 消息通道（worker → 本管理器）
    msg_tx: Sender<JobMsg>,
    msg_rx: Mutex<Receiver<JobMsg>>,
    /// 关闭标志（Engine drop 后 submit 拒绝）
    closed: std::sync::atomic::AtomicBool,
    /// 池引用（drop 顺序保障）
    pool: Arc<Pool>,
    /// 共享注册表
    registry: Arc<FormatRegistry>,
}

/// 单槽锁保护下的字段访问辅助。
impl JobManager {
    /// 新建（pool 由 Engine 构建传入）。
    pub(crate) fn new(
        root: CancellationToken,
        max_running: usize,
        pool: Arc<Pool>,
        registry: Arc<FormatRegistry>,
    ) -> Arc<Self> {
        let (msg_tx, msg_rx) = bounded::<JobMsg>(64);
        let mgr = Arc::new(Self {
            jobs: Mutex::new(HashMap::new()),
            queue: Mutex::new(VecDeque::new()),
            cond: Condvar::new(),
            running: Mutex::new(0),
            running_res: Mutex::new(HashMap::new()),
            root,
            max_running,
            next_id: AtomicU64::new(1),
            msg_tx,
            msg_rx: Mutex::new(msg_rx),
            closed: std::sync::atomic::AtomicBool::new(false),
            pool,
            registry,
        });
        // 消息处理线程：状态迁移/终局 + 队列调度
        let mgr2 = Arc::clone(&mgr);
        let th = std::thread::Builder::new()
            .name("hzip-jobs".into())
            .spawn(move || mgr2.run_loop());
        if let Err(e) = th {
            panic!("spawn jobs thread: {e}");
        }
        mgr
    }

    /// 提交任务（02 §9.3 V：参数校验失败即时 Err，不排队）。
    pub(crate) fn submit(self: &Arc<Self>, spec: JobSpec) -> Result<JobHandle, HzipError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HzipError::Unsupported {
                what: "hz-6003 queue-closed".into(),
            });
        }
        // 参数校验（V 节点）：源存在性
        if !spec.source.exists() {
            return Err(HzipError::Unsupported {
                what: format!("hz-5004 source-not-found: {}", spec.source.display()).into(),
            });
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let child = self.root.child();
        let bus = Arc::new(ProgressBus::new());
        let (req_tx, req_rx) = bounded(REQUEST_CHANNEL_CAP);
        let (ans_tx, ans_rx) = bounded(1);
        let (done_tx, done_rx) = bounded::<()>(1);
        let slot = Arc::new(Mutex::new(JobSlot {
            state: JobState::Queued,
            // `spec` 由 `submit` 按值持有，此后再无使用者 ⇒ 包成 `Arc` 移入槽：
            // 调度/worker 各自 refcount 克隆，不再整份深拷贝。
            spec: Arc::new(spec),
            child,
            bus: Arc::clone(&bus),
            req_tx,
            ans_tx: ans_tx.clone(),
            ans_rx: Some(ans_rx),
            outcome: None,
            done_tx,
            stats: JobStats::new(0, 0),
            pending_req: Arc::new(Mutex::new(None)),
        }));
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
            jobs.insert(id, Arc::clone(&slot));
        }
        {
            let mut q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
            q.push_back(id);
        }
        // 唤醒调度线程（run_loop 阻塞于消息 recv——kick 触发 dispatch）
        let _ = self.msg_tx.send(JobMsg::Kick);
        Ok(JobHandle::new(
            id,
            Arc::clone(&bus),
            req_rx,
            ans_tx,
            done_rx,
            slot,
        ))
    }

    /// 取某任务的**资源键**（槽缺失 → 空；调度算资源键用）。
    ///
    /// 槽锁内**只做一件事**：把 `Arc<JobSpec>` 借出来。键的推导（
    /// [`resource_keys`] → `Path::canonicalize()`，文件系统调用）在**出锁后**跑
    /// ——仓纪律「锁内只取句柄/纯内存，锁外做 I/O」；旧写法把整份 spec 克隆出锁
    /// 再算键，现在克隆退化为 refcount 自增，且 I/O 不再占用槽锁。
    /// 键的推导口径与调用时刻不变（同一函数、同一调度点）；锁序不变。
    fn keys_of(&self, job_id: JobId) -> Vec<String> {
        let spec = {
            let jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
            jobs.get(&job_id)
                .map(|slot| Arc::clone(&slot.lock().unwrap_or_else(|p| p.into_inner()).spec))
        };
        let keys = spec.map(|s| resource_keys(&s));
        // 槽存在 ⇒ 键必非空（`resource_keys` 恒含 `spec.source`）。空键会让
        // `pick_dispatchable` 把该任务当成「不占任何资源」⇒ 同档案任务并发读写
        // （本机制存在的唯一理由）。此处把这类回归从「静默并发」变成测试期断言。
        debug_assert!(
            keys.as_ref().is_none_or(|k| !k.is_empty()),
            "槽内任务必须至少有一个资源键（source）"
        );
        keys.unwrap_or_default()
    }

    /// 调度 + 消息循环。
    fn run_loop(self: Arc<Self>) {
        loop {
            // 先处理消息（迁移/终局）
            let msg = {
                let rx = self.msg_rx.lock().unwrap_or_else(|p| p.into_inner());
                rx.recv()
            };
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break, // 通道关闭（Engine drop）
            };
            match msg {
                JobMsg::Kick => {}
                JobMsg::Transition { job_id, t } => {
                    self.apply_transition(job_id, t);
                }
                JobMsg::Finish { job_id, t, outcome } => {
                    self.apply_finish(job_id, t, outcome);
                }
            }
            // 调度：尽量放行队列任务
            self.dispatch();
        }
    }

    /// 应用状态迁移（合法性校验后落账；非法 = debug_assert 失败，线上忽略）。
    fn apply_transition(self: &Arc<Self>, job_id: JobId, t: Transition) {
        if let Some(slot) = self
            .jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&job_id)
        {
            let mut s = slot.lock().unwrap_or_else(|p| p.into_inner());
            let from = s.state;
            if t.legal_from(from) {
                let to = t.target(from);
                s.state = to;
                // 任务生命周期关键迁移（Queued→Running 为唯一非终局 info 点）
                if matches!(t, Transition::Start) {
                    tracing::info!(
                        job = job_id,
                        kind = ?s.spec.kind,
                        "任务开始执行（Queued→Running）"
                    );
                }
            }
            // 非法迁移：debug 期断言（测试双校验），release 静默
            debug_assert!(
                t.legal_from(from),
                "非法状态迁移 {from:?} -> {:?}",
                t.target(from)
            );
        }
    }

    /// 终局迁移：状态 + outcome + 完成信号 + 运行数递减。
    fn apply_finish(self: &Arc<Self>, job_id: JobId, t: Transition, outcome: Outcome) {
        let slot = {
            let jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
            jobs.get(&job_id).cloned()
        };
        if let Some(slot) = slot {
            let mut s = slot.lock().unwrap_or_else(|p| p.into_inner());
            let from = s.state;
            let to = t.target(from);
            if state::JobState::legal(from, to) {
                s.state = to;
                s.outcome = Some(outcome);
            } else if matches!(to, JobState::Cancelled)
                && matches!(from, JobState::Running | JobState::WaitingInput)
            {
                // 协作式取消：Running/WaitingInput 经 Cancelling 中间态收口
                // （02 §9.3 取消→回滚→RolledBack；Cancelling 为瞬时态）
                s.state = JobState::Cancelling;
                s.state = JobState::Cancelled;
                s.outcome = Some(outcome);
            }
            // 终局迁移留痕（每任务恰好一行；密码/敏感内容不落日志）
            if to.is_terminal() {
                match t {
                    Transition::FinishDone => {
                        tracing::info!(job = job_id, "任务完成（→ Done）");
                    }
                    Transition::FinishPartial => {
                        tracing::info!(job = job_id, "任务部分完成（→ PartialDone）");
                    }
                    Transition::FinishFailed => {
                        // 原因必须落日志：只有一行「任务失败」时，用户/开发者
                        // 都拿不到任何线索（2026-09-29 实测现场）。`HzipError`
                        // 的 Display 不含密码明文（Password 变体只有 target）。
                        match s.outcome.as_ref() {
                            Some(Outcome::Failed(e)) => {
                                tracing::warn!(job = job_id, error = %e, "任务失败（→ Failed）")
                            }
                            _ => tracing::warn!(job = job_id, "任务失败（→ Failed）"),
                        }
                    }
                    Transition::FinishCancel => {
                        tracing::info!(job = job_id, "任务取消收口（→ Cancelled）");
                    }
                    _ => {}
                }
            }
            // 完成信号（终态才发；容量 1 幂等——重复 finish 不阻塞）
            if to.is_terminal() {
                let _ = s.done_tx.try_send(());
            }
            // 释放资源键：同键的排队任务据此解冻（apply_finish 末尾会再调度）。
            // 出锁算键（`resource_keys` 含 `Path::canonicalize()`，文件系统调用
            // 不入槽锁）；spec 不可变，出锁后取键与锁内取键同值。
            let spec = Arc::clone(&s.spec);
            drop(s);
            let keys = resource_keys(&spec);
            let mut res = self.running_res.lock().unwrap_or_else(|p| p.into_inner());
            for k in keys {
                if let Some(n) = res.get_mut(&k) {
                    *n = n.saturating_sub(1);
                }
                if res.get(&k).is_some_and(|n| *n == 0) {
                    res.remove(&k);
                }
            }
        }
        // 运行数递减（无论是否终态成功，只要曾 Running 就减）
        {
            let mut r = self.running.lock().unwrap_or_else(|p| p.into_inner());
            *r = r.saturating_sub(1);
            self.cond.notify_one();
        }
        self.dispatch();
    }

    /// 调度循环：队列非空 + running < max → 放行（Queued→Running 票发池）。
    fn dispatch(self: &Arc<Self>) {
        loop {
            // 门控检查（先看名额，避免白算键）
            {
                let r = self.running.lock().unwrap_or_else(|p| p.into_inner());
                if *r >= self.max_running {
                    return; // 门控不足：不出队
                }
            }
            // 排队快照 + 各自资源键 + 在跑资源集 → 挑第一个不冲突的
            let (ids, key_sets, busy) = {
                let ids: Vec<JobId> = {
                    let q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
                    if q.is_empty() {
                        return;
                    }
                    q.iter().copied().collect()
                };
                let key_sets: Vec<Vec<String>> = ids.iter().map(|id| self.keys_of(*id)).collect();
                let busy: std::collections::HashSet<String> = self
                    .running_res
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .iter()
                    .filter(|(_, n)| **n > 0)
                    .map(|(k, _)| k.clone())
                    .collect();
                (ids, key_sets, busy)
            };
            // 同键任务串行：被占的任务留在队里，派发其后第一个可跑的（保序）；
            // 全被占 → 本轮到此，等占用方收口（apply_finish 会再调度）。
            let Some(ix) = pick_dispatchable(&key_sets, &busy) else {
                return;
            };
            let job_id = ids[ix];
            // 名额（再确认一次：上面读与这里写之间有窗口）
            {
                let mut r = self.running.lock().unwrap_or_else(|p| p.into_inner());
                if *r >= self.max_running {
                    return;
                }
                *r += 1;
            }
            // 出队（按下标移除；前面被跳过的任务留在队里保持相对序）
            {
                let mut q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
                q.remove(ix);
            }
            // 取槽：取消检查（未启动直接移除，TC-CORE-011）
            let slot = {
                let jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
                jobs.get(&job_id).cloned()
            };
            let slot = match slot {
                Some(s) => s,
                None => {
                    // 槽缺失：归还 running 名额
                    let mut r = self.running.lock().unwrap_or_else(|p| p.into_inner());
                    *r = r.saturating_sub(1);
                    continue;
                }
            };
            let cancelled = {
                let s = slot.lock().unwrap_or_else(|p| p.into_inner());
                s.state == JobState::Queued && s.child.is_cancelled()
            };
            if cancelled {
                // Queued → Cancelled（直接移除，不发池；apply_finish 内做
                // Queued→Cancelled 合法迁移 + outcome 落账 + 完成信号）
                self.apply_finish(job_id, Transition::FinishCancel, Outcome::Cancelled);
                continue;
            }
            // Queued → Running
            self.apply_transition(job_id, Transition::Start);
            // 占用资源键（同键串行；排队同键任务据此等待）
            {
                let mut res = self.running_res.lock().unwrap_or_else(|p| p.into_inner());
                for k in &key_sets[ix] {
                    *res.entry(k.clone()).or_insert(0) += 1;
                }
            }
            // 发票给池
            let mgr = Arc::clone(self);
            let pool = Arc::clone(&self.pool);
            pool.execute(job_id, move || {
                mgr.handle_job(job_id, slot);
            });
        }
    }

    /// 池 worker 执行体：构建 JobCtx → services::execute → 终局。
    fn handle_job(self: &Arc<Self>, job_id: JobId, slot: Arc<Mutex<JobSlot>>) {
        // 取 spec + 总线（Arc 克隆 = refcount，不深拷贝 spec；
        // `services::execute` 收 `&JobSpec`，持 Arc 跨越「不持锁执行」即可）
        let (spec, bus) = {
            let s = slot.lock().unwrap_or_else(|p| p.into_inner());
            (Arc::clone(&s.spec), Arc::clone(&s.bus))
        };
        // 发 JobStarted（骨架期总量恒 0；服务层构建计划后经 JobTotal 补报真实值
        // ——services/extract.rs 选择集解析后发送，GUI/CLI 进度条数据源）
        let total = 0;
        bus.send(ProgressEvent::JobStarted {
            job_id,
            total_bytes: total,
            total_entries: 0,
        });
        let ctx = {
            let cancel = slot.lock().unwrap_or_else(|p| p.into_inner()).child.clone();
            let req_tx = slot
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .req_tx
                .clone();
            let (ans_rx, pending_req) = {
                let mut s = slot.lock().unwrap_or_else(|p| p.into_inner());
                let rx = match s.ans_rx.take() {
                    Some(rx) => rx,
                    None => panic!("ans_rx 单次 take"),
                };
                (rx, Arc::clone(&s.pending_req))
            };
            JobCtx {
                job_id,
                cancel,
                progress: Arc::clone(&bus),
                registry: Arc::clone(&self.registry),
                tx: self.msg_tx.clone(),
                req_tx,
                ans_rx,
                next_req_id: AtomicU64::new(0),
                pending_req,
                req_timeout: Duration::from_secs(REQUEST_TIMEOUT_SECS),
            }
        };
        // panic 隔离（TC-CORE-013）：job 粒度 catch_unwind
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            services::execute(&spec.kind, &ctx, &spec)
        }));
        let outcome = match outcome {
            Ok(o) => o,
            Err(p) => {
                let msg = panic_payload_to_string(&p);
                eprintln!("[hzip] worker panic job={job_id}: {msg}");
                Outcome::Failed(HzipError::Unsupported {
                    what: format!("hz-0000 worker-panic: {msg}").into(),
                })
            }
        };
        // 终局事件
        let outcome_clone = outcome.clone();
        bus.send(ProgressEvent::JobFinished {
            job_id,
            outcome: outcome_clone,
        });
        ctx.finish(outcome);
    }

    /// 快照（GUI 队列视图；测试断言 running ≤ 上限用）。
    pub(crate) fn snapshot(&self) -> Vec<(JobId, JobState)> {
        let jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        jobs.iter()
            .map(|(&id, slot)| {
                let s = slot.lock().unwrap_or_else(|p| p.into_inner());
                (id, s.state)
            })
            .collect()
    }

    /// 当前运行数（门控断言）。
    #[allow(dead_code)] // 集成测试（TC-CORE-012）使用
    pub(crate) fn running_count(&self) -> usize {
        *self.running.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 取槽（Handle 用）。
    #[allow(dead_code)] // 集成测试（mock_jobs）使用
    pub(crate) fn slot(&self, job_id: JobId) -> Option<Arc<Mutex<JobSlot>>> {
        self.jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&job_id)
            .cloned()
    }

    /// 取消全部（级联：root 已取消则全部子令牌取消；Engine shutdown）。
    pub(crate) fn cancel_all(&self) {
        let jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        for slot in jobs.values() {
            let child = slot.lock().unwrap_or_else(|p| p.into_inner()).child.clone();
            child.cancel();
        }
    }

    /// 关闭（Engine drop）：拒绝新提交 + 取消全部。
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.cancel_all();
    }
}

/// panic payload → 诊断字符串（不 unwrap payload，红线）。
fn panic_payload_to_string(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}
