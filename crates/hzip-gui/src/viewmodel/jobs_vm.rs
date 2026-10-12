//! JobsViewModel：任务队列 / 进度 / 取消（02 §11.1 VM2；T41）。
//!
//! 订阅 `JobHandle` 事件流经 **10 Hz 节流合并**后的状态聚合（02 §4.3 C2
//! 消费者）——逐条事件处理仅限：节流器输出的 [`ProgressSnapshot`] 聚合进行、
//! `EntryDone{crc_ok:false}` 失败记账（不触发 UI 信号）、`JobFinished` 终局
//! 落账。
//!
//! # 行状态机（02 §9.3 一一对应）
//!
//! 行状态由事件驱动迁移：`JobStarted` → Running（含 total 初始化）；快照
//! 推进进度；`JobFinished` → 依 `Outcome` 收敛终态（Done/PartialDone/
//! Failed/Cancelled）。`WaitingInput`/`Cancelling` 由 core 状态机产生——
//! 本层经 [`crate::viewmodel::pump`] 从 core 状态快照桥接（运行中行按需
//! 同步），事件流不直接表达这两态。
//!
//! # 密码与脱敏（02 §1.2）
//!
//! 本层不接触密码；`JobRow`/日志面断言无密码明文。

use std::collections::HashMap;
use std::time::Duration;

use hzip_core::engine::Engine;
use hzip_core::jobs::{JobHandle, JobSpec, JobState};
use hzip_error::HzipError;
use hzip_progress::JobId;
use hzip_progress::{Outcome, ProgressEvent, ProgressSnapshot};

use super::rows::{FailureRow, JobRow, JobRowOutcome, JobRowState, RecentEntry};

/// 任务行查询/聚合（02 §11.1 VM2）。
/// 「开始执行」落账：回填开始时刻（耗时列数据源）。状态置位由调用方负责。
/// 返回是否改动了行。
fn mark_started(row: &mut JobRow) -> bool {
    if row.started_at.is_none() {
        row.started_at = Some(std::time::SystemTime::now());
        return true;
    }
    false
}

/// 任务终局判定（会话密码能否入缓存；用户口径 2026-09-29）。
///
/// 只有 [`JobVerify::Succeeded`] 才把该任务的密码写进会话缓存——**任务成功即
/// 密码正确**（该任务正是拿着它去读写那个档案的）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobVerify {
    /// 仍在跑（等终局再判）
    Active,
    /// 成功结束（Done/Partial）
    Succeeded,
    /// 失败/取消（**不入缓存**）
    Failed,
    /// 任务行已不在（已清行/未知 id；保守按未成功处理）
    Gone,
}

/// core 权威状态 → 行状态投影（`sync_states_from_core` 的唯一映射）。
///
/// **终局不投影**（返回 `None`）：`on_finished` 是终局唯一落账点——它同时收敛
/// outcome/失败明细/耗时定格；若此处也写终局，会和它抢写、丢掉失败明细。
fn project_core_state(s: JobState) -> Option<JobRowState> {
    match s {
        JobState::Queued => Some(JobRowState::Queued),
        JobState::Running => Some(JobRowState::Running),
        JobState::WaitingInput => Some(JobRowState::WaitingInput),
        JobState::Cancelling => Some(JobRowState::Cancelling),
        JobState::Done | JobState::PartialDone | JobState::Failed | JobState::Cancelled => None,
    }
}

#[derive(Debug, Default)]
pub struct JobsViewModel {
    /// 任务行（**`Arc` + 写时复制**：2026-09-24 审 ⑦⑲——快照导出只克隆引用、
    /// 任务中心视图可据 `Arc` 身份判「行数据没变」而跳过全量重建派生行）。
    rows: std::sync::Arc<Vec<JobRow>>,
    by_id: HashMap<JobId, usize>,
    /// 活跃任务句柄（cancel 用；终局后移除）
    handles: HashMap<JobId, JobHandle>,
    /// 任务的**会话密码缓存键**（用户口径 2026-09-29）：读档案类任务 = 档案本体，
    /// 压缩 = 产出档案。任务被问密码/参数窗已填密码时按它登记，任务**成功后**
    /// 写进会话缓存；终局即移除（不留无用映射）。
    password_keys: HashMap<JobId, std::path::PathBuf>,
    /// 解压进度窗挂起表（job → force；设计稿 11 §4.2 密码门时序：提交点
    /// 登记，泵在首个**非 JobStarted**事件消费——JobStarted 先于密码问询，
    /// 其余事件保证密码已应答或任务已终局）。
    progress_open: HashMap<JobId, bool>,
    /// 下一条「打开类记录」的 id（高位命名空间，见 [`OPEN_ROW_ID_BASE`]）。
    next_open_id: JobId,
}

/// 打开类记录（打开档案 / 进入容器）的 id 起点。
///
/// 真实任务 id 由 engine 计数器自 1 起递增，故高位段不可能撞车；
/// `handles` 里没有这些 id ⇒ 不可取消、泵不驱动、徽标（活跃数）不计。
pub(crate) const OPEN_ROW_ID_BASE: JobId = 1 << 62;

/// `recent_entries` 容量上限（T-PW 进度窗口虚拟列表数据面；淘汰最旧）。
pub const RECENT_ENTRIES_CAP: usize = 500;

impl JobsViewModel {
    /// 新建（空队列）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 提交任务（02 §9.3 A 节点：`Engine::submit`）并登记行（Queued）。
    pub fn submit(&mut self, engine: &Engine, spec: JobSpec) -> Result<JobId, HzipError> {
        let kind = kind_str(&spec);
        // 任务中心列扩展数据面（R-GUI4 #4：输入/输出/行为/格式列）
        let src = spec
            .source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| spec.source.display().to_string());
        let dest = match spec.kind {
            hzip_core::jobs::JobKind::Compress => {
                spec.out.as_ref().map(|p| p.display().to_string())
            }
            _ => spec.dest.as_ref().map(|p| p.display().to_string()),
        };
        let format = match spec.kind {
            hzip_core::jobs::JobKind::Compress => spec
                .compress
                .as_ref()
                .map(|c| c.format.to_string())
                .unwrap_or_default(),
            _ => spec
                .source
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
        };
        let key = crate::ui::shell::dispatch::password_cache_key(&spec);
        let handle = engine.submit(spec)?;
        let id = handle.job_id();
        self.handles.insert(id, handle);
        if let Some(key) = key {
            self.password_keys.insert(id, key);
        }
        self.register(JobRow {
            job_id: id,
            kind,
            state: JobRowState::Queued,
            progress: None,
            speed_bps: 0.0,
            eta: None,
            done_bytes: 0,
            total_bytes: 0,
            entries_done: 0,
            entries_total: 0,
            failures: Vec::new(),
            finished: None,
            src,
            dest,
            format,
            started_at: None,
            elapsed: None,
            final_line: None,
            current_entry: None,
            recent_entries: std::collections::VecDeque::new(),
        });
        Ok(id)
    }

    /// 「打开档案 / 进入容器」记录（用户口径 2026-09-24）。
    ///
    /// 打开是同步轻活（无 JobId/句柄/取消语义），原先只在「正在打开」窗口里
    /// 一闪而过——用户明确要求「这些错误都需要在任务中心记录该操作及操作结果
    /// （如果失败，则需要记录比较明确的失败原因）」。故以**行**形态落账：列表
    /// 渲染、清空、徽标、双击重开等既有逻辑零改动（打开行恒为终态、无句柄）。
    ///
    /// `result` 携带结果：`Ok` → Done 行（记录「打开成功了哪个档案」）；
    /// `Err(reason)` → Failed 行 + `final_line`（原因原文，含 HZ 码）。
    pub fn record_open(
        &mut self,
        display_name: &str,
        full_path: &str,
        result: &Result<(), String>,
    ) -> JobId {
        self.record_op("open", display_name, full_path, result)
    }

    /// 通用「非任务操作」记录（打开 / 复制 / 外部打开…；用户口径 2026-09-24：
    /// 所有任务都要在任务中心留痕，失败要带明确原因）。
    ///
    /// 与 [`Self::record_open`] 同一行形态，只有 `kind` 不同（「打开」→「复制」
    /// 等，`job_kind_label` 查表）。
    pub fn record_op(
        &mut self,
        kind: &'static str,
        display_name: &str,
        full_path: &str,
        result: &Result<(), String>,
    ) -> JobId {
        let job_id = OPEN_ROW_ID_BASE.saturating_add(self.next_open_id);
        self.next_open_id = self.next_open_id.saturating_add(1);
        let reason = result.as_ref().err().cloned();
        let ok = reason.is_none();
        self.register(JobRow {
            job_id,
            kind,
            state: if ok {
                JobRowState::Done
            } else {
                JobRowState::Failed
            },
            progress: ok.then_some(1.0),
            speed_bps: 0.0,
            eta: Some(Duration::ZERO),
            done_bytes: 0,
            total_bytes: 0,
            entries_done: 0,
            entries_total: 0,
            failures: Vec::new(),
            finished: Some(if ok {
                JobRowOutcome::Done { bytes: 0 }
            } else {
                JobRowOutcome::Failed {
                    code: 0,
                    i18n_key: reason.clone().unwrap_or_default(),
                    detail: reason.clone().unwrap_or_default(),
                }
            }),
            src: format!("{display_name}  ({full_path})"),
            current_entry: None,
            // 失败原因（含 HZ 码原文；空 entry = 整行原因，渲染侧见
            // `ui/views/jobs.rs` 的失败行）
            final_line: reason.map(|r| FailureRow {
                entry: String::new(),
                code: 0,
                message: r,
            }),
            recent_entries: std::collections::VecDeque::new(),
            dest: None,
            format: std::path::Path::new(full_path)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
            started_at: Some(std::time::SystemTime::now()),
            elapsed: Some(Duration::ZERO),
        });
        job_id
    }

    /// 登记行（submit/测试直插）。
    pub fn register(&mut self, row: JobRow) {
        self.by_id.insert(row.job_id, self.rows.len());
        std::sync::Arc::make_mut(&mut self.rows).push(row);
    }

    /// 挂起进度窗（解压提交点用；`force` 同 `open_progress_window_with` 语义：
    /// `true` 强制显示、`false` 遵守配置开关）。
    pub fn register_progress_open(&mut self, job_id: JobId, force: bool) {
        self.progress_open.insert(job_id, force);
    }

    /// 消费挂起（泵在首个非 JobStarted 事件调用；`None` = 未挂起/已消费）。
    pub fn take_progress_open(&mut self, job_id: JobId) -> Option<bool> {
        self.progress_open.remove(&job_id)
    }

    /// 取消任务（`handle.cancel()` 幂等；级联子令牌）。
    ///
    /// 取消后行状态不立即改——等待 `JobFinished{Cancelled}` 事件落账
    /// （core 终局唯一事实来源）。
    pub fn cancel(&mut self, job_id: JobId) {
        if let Some(h) = self.handles.get(&job_id) {
            h.cancel();
        }
    }

    /// 取消全部活跃任务（T-GUI T31「全部取消」接线；逐 id 幂等 cancel）。
    pub fn cancel_all(&mut self) {
        let ids: Vec<JobId> = self
            .rows
            .iter()
            .filter(|r| !is_terminal(r.state))
            .map(|r| r.job_id)
            .collect();
        for id in ids {
            self.cancel(id);
        }
    }

    /// 清除全部终态行（T-GUI T31「清除已完成」接线；活跃行保留）。
    ///
    /// 返回清除的行数（0 = 无终态行，UI 侧可静默）。
    pub fn clear_finished(&mut self) -> usize {
        let before = self.rows.len();
        std::sync::Arc::make_mut(&mut self.rows).retain(|r| !is_terminal(r.state));
        let removed = before - self.rows.len();
        // by_id 索引重建（行下标全量平移）
        self.by_id.clear();
        for (ix, r) in self.rows.iter().enumerate() {
            self.by_id.insert(r.job_id, ix);
        }
        // 密码缓存键跟随行生命周期（清行即清键，不留无用映射）
        self.password_keys
            .retain(|id, _| self.by_id.contains_key(id));
        removed
    }

    /// 句柄访问（T42 补丁：UI 侧应答密码/冲突请求需经句柄
    /// `answer`；终局后句柄移除返回 None）。
    pub fn handle(&self, job_id: JobId) -> Option<&JobHandle> {
        self.handles.get(&job_id)
    }

    /// 全部行（任务中心列表）。
    pub fn rows(&self) -> &[JobRow] {
        &self.rows
    }

    /// 行集 **Arc 句柄**（快照导出零深拷贝；同一份未变时多次导出 `ptr_eq` 相等
    /// ——任务中心视图据此跳过派生行重建，审计 ⑦⑲）。
    pub fn rows_arc(&self) -> std::sync::Arc<Vec<JobRow>> {
        self.rows.clone()
    }

    /// 单行。
    pub fn row(&self, job_id: JobId) -> Option<&JobRow> {
        self.by_id.get(&job_id).map(|&i| &self.rows[i])
    }

    /// 行可变访问（pump 状态同步用）。
    pub fn row_mut(&mut self, job_id: JobId) -> Option<&mut JobRow> {
        let ix = self.by_id.get(&job_id).copied()?;
        Some(&mut std::sync::Arc::make_mut(&mut self.rows)[ix])
    }

    /// 任务的**会话密码缓存键**（读档案类 = 档案本体；压缩 = 产出档案）。
    pub fn password_key_of(&self, job_id: JobId) -> Option<&std::path::Path> {
        self.password_keys.get(&job_id).map(|p| p.as_path())
    }

    /// 任务终局判定（会话密码能否入缓存用；用户口径 2026-09-29）。
    pub fn verify_of(&self, job_id: JobId) -> JobVerify {
        let Some(row) = self.row(job_id) else {
            return JobVerify::Gone;
        };
        match &row.finished {
            Some(JobRowOutcome::Done { .. } | JobRowOutcome::Partial { .. }) => {
                JobVerify::Succeeded
            }
            Some(_) => JobVerify::Failed,
            // 终局但未见终局摘要（事件早于订阅/已清行）：保守按未成功处理
            None if is_terminal(row.state) => JobVerify::Failed,
            None => JobVerify::Active,
        }
    }

    /// 运行中行数（任务中心徽标；终态除外）。
    pub fn active_count(&self) -> usize {
        self.rows.iter().filter(|r| !is_terminal(r.state)).count()
    }

    /// 是否有任务在运行/排队（pump 退出判定）。
    pub fn has_active(&self) -> bool {
        self.active_count() > 0
    }

    /// 是否有**在飞**的编辑任务（kind = `edit` 且未终局）。
    ///
    /// 编辑任务在飞期间**不能**重开浏览会话：重开会重新打开源档案（读句柄），
    /// 而编辑任务随后要对同一路径做原子替换——Windows 上目标被打开时
    /// `MoveFileExW(REPLACE_EXISTING)` 直接返回 os error 5（拒绝访问）。
    ///
    /// 现场（2026-09-29 用户实测）：加密档案重命名 → 弹密码期间/重写期间泵就把
    /// 会话重开了 → 替换必失败（`IO 错误: 拒绝访问。 (os error 5)`）。未加密档
    /// 重写快（原样拷贝毫秒级）常能抢在重开之前，故此前未被发现。
    pub fn has_active_edit(&self) -> bool {
        self.rows
            .iter()
            .any(|r| r.kind == "edit" && !is_terminal(r.state))
    }

    /// 吞入一条原始进度事件（pump 逐条调用；事件语义见模块文档）。
    ///
    /// 快照聚合归 [`apply`](Self::apply)（字节面经节流器）；本入口只处理
    /// 逐条语义：`JobTotal` 总量补报（条目总数即时生效）、`EntryDone` 活计数
    /// （任务中心「x/y 条」实时显示，不等终局 stats）。
    /// 从 core 权威句柄回填行状态（**每 tick 调用**；返回是否有行变化）。
    ///
    /// 为什么不能只靠事件：进度总线是「多订阅者广播、无历史」，而 `JobStarted`
    /// 在 `submit` **内部**派发时就发（`jobs/mod.rs` 调度循环），GUI 却在
    /// `submit` 返回后才 `handle.events()` 订阅——晚订阅者必丢首发事件。于是
    /// 行状态会一直停在 `Queued`，直到第一个进度事件（`apply` 才写 Running），
    /// 流式归档读索引阶段可达数秒：界面把正在解压的任务显示成「等待开始…」
    /// （实测 tar.gz 3.6s）。`JobHandle::state()` 是 core 唯一事实来源，
    /// 状态投影必须归它。
    ///
    /// 终局不在此处落账（留给 `on_finished` 收敛 outcome/失败明细）。
    ///
    /// 两段式：先**只读**预扫出待改行，无改动立即返回、不碰 `self.rows`。
    /// 为什么：UI/快照层持 `rows_arc()` 的 `Arc` 克隆（引用计数 ≥ 2），
    /// 无条件 `Arc::make_mut` 会在引用计数 ≥ 2 时深拷贝整个 `Vec<JobRow>`
    /// （每行还带 `VecDeque<RecentEntry>` 等）——而「无变化」的唤醒（100 ms
    /// 空闲 tick 等）占绝大多数，故先读后写规避这次无谓深拷贝。
    pub fn sync_states_from_core(&mut self) -> bool {
        // 阶段 1（只读）：记录「(行下标, 目标状态, 是否可能补记开始时刻)」。
        // 只在真会改时入队，故空计划 ⇔ 什么都没变（含已 Running 但已记过
        // started_at 的行）。
        let mut plan: Vec<(usize, JobRowState, bool)> = Vec::new();
        for (ix, row) in self.rows.iter().enumerate() {
            let Some(handle) = self.handles.get(&row.job_id) else {
                continue;
            };
            let Some(next) = project_core_state(handle.state()) else {
                continue;
            };
            let running = next == JobRowState::Running;
            // 状态需改，或 Running 且尚未回填开始时刻（mark_started 会落账）。
            if row.state != next || (running && row.started_at.is_none()) {
                plan.push((ix, next, running));
            }
        }
        if plan.is_empty() {
            return false;
        }

        // 阶段 2（写入）：仅在确有改动时深拷贝一次，按计划落账。
        // 返回语义与原实现逐行等价：任一状态不同或任一 mark_started 落账 → true。
        let rows = std::sync::Arc::make_mut(&mut self.rows);
        let mut changed = false;
        for (ix, next, running) in plan {
            let row = &mut rows[ix];
            if row.state != next {
                row.state = next;
                changed = true;
            }
            if running {
                changed |= mark_started(row);
            }
        }
        changed
    }

    pub fn on_event(&mut self, job_id: JobId, ev: &ProgressEvent) {
        let Some(row) = self.row_mut(job_id) else {
            return;
        };
        match ev {
            ProgressEvent::JobStarted { .. } => {
                // 任务已开始执行（core 在派发服务**之前**发本事件）→ 状态必须立刻
                // 推进。否则行状态要等第一个 `apply()`（JobTotal/Bytes）才变 Running，
                // 而流式归档「建条目索引」阶段可以几秒不发任何进度事件（实测 57MB
                // tar.gz 3.6s）——界面于是把正在解压的任务显示成「等待开始…」。
                row.state = JobRowState::Running;
                mark_started(row);
            }
            ProgressEvent::JobTotal { total_entries, .. } => {
                row.entries_total = *total_entries;
            }
            ProgressEvent::EntryStarted { entry, .. } => {
                row.current_entry = Some(entry.to_string());
            }
            ProgressEvent::EntryDone {
                entry,
                bytes,
                crc_ok,
                ..
            } => {
                row.entries_done = row.entries_done.saturating_add(1);
                row.current_entry = Some(entry.to_string());
                row.recent_entries.push_back(RecentEntry {
                    name: entry.to_string(),
                    bytes: *bytes,
                    ok: *crc_ok,
                    reason: String::new(),
                    code: 0,
                });
                while row.recent_entries.len() > RECENT_ENTRIES_CAP {
                    row.recent_entries.pop_front();
                }
            }
            _ => {}
        }
    }

    /// 逐条目失败原因落账（pump `JobFinished` 时调用）。
    ///
    /// core 在 best_effort 跳过失败条目时不发 `EntryDone{crc_ok:false}`
    ///（K3 直接记 failures 继续）——此处把 `Outcome::Partial`/`Failed`
    /// 的失败明细按条目名并入 `recent_entries`（就近原位替换；否则追加），
    /// 进度窗口文件列表得以持续输出带 i18n 原因的失败行。
    pub fn attach_failure_reason(&mut self, job_id: JobId, outcome: &Outcome) {
        let fs: Vec<(String, u32, String)> = match outcome {
            Outcome::Partial(_, fs) => fs
                .iter()
                .map(|f| (f.entry.to_string(), f.error.code(), f.error.i18n_key()))
                .collect(),
            Outcome::Failed(e) => vec![(String::new(), e.code(), e.i18n_key())],
            _ => return,
        };
        let Some(row) = self.row_mut(job_id) else {
            return;
        };
        for (name, code, msg) in fs {
            let pos = if name.is_empty() {
                None
            } else {
                row.recent_entries.iter().rposition(|e| e.name == name)
            };
            match pos {
                Some(i) => {
                    let e = &mut row.recent_entries[i];
                    e.ok = false;
                    e.reason = msg;
                    e.code = code;
                }
                None => {
                    row.recent_entries.push_back(RecentEntry {
                        name,
                        bytes: 0,
                        ok: false,
                        reason: msg,
                        code,
                    });
                    while row.recent_entries.len() > RECENT_ENTRIES_CAP {
                        row.recent_entries.pop_front();
                    }
                }
            }
        }
    }

    /// 应用一条节流快照（pump 10 Hz flush 调用）。
    ///
    /// 快照携带 job_id 语义经调用方解析（见 [`apply_snapshot`]）——
    /// 单任务场景以当前首行应用；多任务按 id 由 pump 路由。
    pub fn apply(&mut self, job_id: JobId, snap: &ProgressSnapshot) {
        let Some(row) = self.row_mut(job_id) else {
            return;
        };
        if row.started_at.is_none() {
            row.started_at = Some(std::time::SystemTime::now());
        }
        row.state = JobRowState::Running;
        row.total_bytes = snap.total;
        row.done_bytes = snap.done;
        row.speed_bps = snap.speed.unwrap_or(0.0);
        row.progress = if snap.total > 0 {
            Some((snap.done as f64 / snap.total as f64).min(1.0))
        } else {
            None
        };
        row.eta = snap.eta.and_then(eta_to_duration);
    }

    /// `JobFinished` 终局落账（pump 消费）。
    pub fn on_finished(&mut self, job_id: JobId, outcome: &Outcome) {
        self.handles.remove(&job_id);
        self.progress_open.remove(&job_id);
        // 终局即移除密码缓存键（登记表条目由 AppData 在提交批次里收口后再清）
        self.password_keys.remove(&job_id);
        let Some(row) = self.row_mut(job_id) else {
            return;
        };
        row.state = match outcome {
            Outcome::Done(_) => JobRowState::Done,
            Outcome::Partial(..) => JobRowState::PartialDone,
            Outcome::Failed(_) | Outcome::RolledBack(_) => JobRowState::Failed,
            Outcome::Cancelled => JobRowState::Cancelled,
            // #[non_exhaustive] 兜底
            _ => JobRowState::Failed,
        };
        row.eta = Some(Duration::ZERO);
        // 终局清当前条目（T-PW：进度窗口「当前：…」行退场；失败明细归 failures）
        row.current_entry = None;
        row.elapsed = row
            .started_at
            .and_then(|s| std::time::SystemTime::now().duration_since(s).ok());
        row.finished = Some(outcome_row(outcome));
        // 终局摘要行（进度窗口文件列表持续输出终局结果；Failed/Partial 有因）
        row.final_line = match &row.finished {
            Some(JobRowOutcome::Failed {
                code,
                i18n_key,
                detail,
            }) => Some(FailureRow {
                entry: String::new(),
                code: *code,
                // 原文优先（用户口径 2026-09-29：任务中心要能看出「具体」原因——
                // 只有类别 key 时显示「IO 错误」，看不出是哪个文件出什么事）；
                // 无原文（如 _ 兜底分支）回落 i18n key。
                message: if detail.is_empty() {
                    i18n_key.clone()
                } else {
                    format!("HZ-{code:04}: {detail}")
                },
            }),
            Some(JobRowOutcome::Partial { failures, .. }) => failures.first().map(|f| FailureRow {
                entry: {
                    let n = failures.len().to_string();
                    crate::tr_with("vm.more_items", &[("n", &n)])
                },
                code: f.code,
                message: f.message.clone(),
            }),
            _ => None,
        };
        match outcome {
            Outcome::Partial(stats, failures) => {
                row.entries_done = stats.entries_done;
                row.entries_total = stats.entries_total.max(stats.entries_done);
                row.failures = failures
                    .iter()
                    .map(|f| FailureRow {
                        entry: f.entry.to_string(),
                        code: f.error.code(),
                        message: f.error.i18n_key(),
                    })
                    .collect();
            }
            Outcome::Done(stats) => {
                row.entries_done = stats.entries_done;
                row.entries_total = stats.entries_total.max(stats.entries_done);
                row.done_bytes = stats.bytes_done;
                row.total_bytes = row.total_bytes.max(stats.bytes_done);
                row.progress = Some(1.0);
            }
            _ => {}
        }
        // 逐条目失败原因并入已处理列表（进度窗口文件列表输出带因失败行）
        self.attach_failure_reason(job_id, outcome);
    }

    /// 运行中行按 core 状态同步（WaitingInput/Cancelling 桥接）。
    pub fn sync_state(&mut self, job_id: JobId, core_state: &str) {
        let Some(row) = self.row_mut(job_id) else {
            return;
        };
        if is_terminal(row.state) {
            return; // 终态以 JobFinished 落账为准
        }
        row.state = match core_state {
            "waiting-input" => JobRowState::WaitingInput,
            "cancelling" => JobRowState::Cancelling,
            _ => row.state,
        };
    }
}

/// `JobSpec` → 行展示种类（core JobKind 无 Display；T42 文案查表）。
fn kind_str(spec: &JobSpec) -> &'static str {
    match spec.kind {
        hzip_core::jobs::JobKind::Extract => "extract",
        hzip_core::jobs::JobKind::Test => "test",
        hzip_core::jobs::JobKind::Compress => "compress",
        hzip_core::jobs::JobKind::Convert => "convert",
        hzip_core::jobs::JobKind::EditSave => "edit",
        hzip_core::jobs::JobKind::EditBatch => "edit",
        // #[non_exhaustive] 兜底
        _ => "job",
    }
}

/// `Eta` → 剩余时长（行数据面；AlmostDone → 近零）。
fn eta_to_duration(eta: hzip_progress::Eta) -> Option<Duration> {
    match eta {
        hzip_progress::Eta::AlmostDone => Some(Duration::from_secs(1)),
        hzip_progress::Eta::Seconds(s) => Some(Duration::from_secs(s)),
    }
}

/// 行状态是否终态（pump 退出判定/状态同步守卫）。
pub fn is_terminal(s: JobRowState) -> bool {
    matches!(
        s,
        JobRowState::Done | JobRowState::PartialDone | JobRowState::Failed | JobRowState::Cancelled
    )
}

/// `Outcome` → 行终局摘要。
pub(crate) fn outcome_row(outcome: &Outcome) -> JobRowOutcome {
    match outcome {
        Outcome::Done(s) => JobRowOutcome::Done {
            bytes: s.bytes_done,
        },
        Outcome::Partial(s, fs) => JobRowOutcome::Partial {
            entries_done: s.entries_done,
            failures: fs
                .iter()
                .map(|f| FailureRow {
                    entry: f.entry.to_string(),
                    code: f.error.code(),
                    message: f.error.i18n_key(),
                })
                .collect(),
        },
        Outcome::Failed(e) => JobRowOutcome::Failed {
            code: e.code(),
            i18n_key: e.i18n_key(),
            // 原文（含路径/os 文案）——任务中心「失败原因」列显示它
            detail: e.to_string(),
        },
        Outcome::Cancelled | Outcome::RolledBack(_) => JobRowOutcome::Cancelled,
        // #[non_exhaustive] 兜底
        _ => JobRowOutcome::Failed {
            code: 0,
            i18n_key: "err.hz_0000".into(),
            detail: String::new(),
        },
    }
}

#[cfg(test)]
#[path = "tests/jobs_vm.rs"]
mod tests;
