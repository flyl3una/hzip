//! 请求-应答状态机：密码三态 / 冲突会话（全部应用）（02 §11.3/§11.4；T41）。
//!
//! worker 经 `JobHandle.requests()` 上抛 `JobRequest::Password` / `Conflict`
//! （通道容量 1——同时最多一个待决问题，02 附录 B.2）；本 router 收容并驱动
//! **弹窗状态机**（纯状态、可被任意线程消费——T42 gpui/gpui_kit 侧绑定），用户提交后
//! 经 `handle.answer(req_id, resp)` 回写。
//!
//! # 密码三态（02 §11.4「显示目标条目；错误重试计数」）
//!
//! `Hidden → Awaiting`（Password 请求，记 request_id/target）→ 提交
//! `Answer(Password)`（瞬态动作不驻留）→ 任务继续。同任务再次 Password 请求
//! （错误密码重问 `PasswordReason::WrongPassword`）→ `Retry`（attempts+1）；
//! 达 [`PASSWORD_MAX_RETRIES`]=3 → `handle.cancel()` + 复位 Hidden。
//! 密码以 `Zeroizing<String>` 持有（02 §1.2）——`Retry` 态不持有明文副本。
//!
//! # 冲突会话与「全部应用」（02 §11.4 写回策略）
//!
//! 首个冲突请求 → 弹窗 Awaiting；用户提交决策 + `apply_all=true` → 决策写入
//! 会话策略表 → 后续同决策面冲突请求**自动应答不弹窗**。会话作用域 = 单任务
//! （一次解压/压缩），不写回持久配置（写回 UI 偏好归 T42，仅作默认值建议）。
//!
//! # 超时终态消费
//!
//! 120 s 决策超时（core 侧 [`REQUEST_TIMEOUT_SECS`]）→ 任务 Failed（HZ-5002）
//! → pump 消费 `JobFinished{Failed}` 后调用 [`DialogRouter::on_job_finished`]
//! 复位弹窗 Hidden。
//!
//! 出处：02 §11.3 时序（R→S→R 段 ViewModel 半场）、§11.4、§9.11、附录 B.2。

use hzip_core::jobs::{
    ConflictDecision, JobHandle, JobRequest, JobResponse, PasswordReason, RequestId,
};
use hzip_error::HzipError;
use hzip_format::EntryName;
use zeroize::Zeroizing;

/// 密码重试上限（02 附录 B.2 `PASSWORD_MAX_RETRIES = 3`；GUI 侧共用 CLI 同值）。
pub const PASSWORD_MAX_RETRIES: u8 = 3;

/// 密码弹窗状态（02 §11.4 三态 + Answered 瞬态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordDialogState {
    /// 关闭（无待决密码）
    Hidden,
    /// 等待输入（显示目标条目）
    Awaiting {
        /// 任务 ID
        job_id: u64,
        /// 待决请求 ID（answer 校验用）
        request_id: RequestId,
        /// 目标条目（None = 整档/头）
        target: Option<EntryName>,
        /// 请求原因（Initial/HeaderEncrypted → 首次输入；WrongPassword 不会
        /// 停在 Awaiting）
        reason: PasswordReason,
    },
    /// 错误重试（attempts 计数 + 上次失败原因；不持有密码明文副本）
    Retry {
        /// 任务 ID
        job_id: u64,
        /// 待决请求 ID
        request_id: RequestId,
        /// 目标条目
        target: Option<EntryName>,
        /// 已尝试次数（达 [`PASSWORD_MAX_RETRIES`] → cancel）
        attempts: u8,
        /// 上次失败原因（core 重问 reason 固定 WrongPassword；保留字段供
        /// 文案：e.g. HeaderEncrypted 首问失败）
        last_reason: PasswordReason,
    },
}

impl Default for PasswordDialogState {
    /// 初始关闭（Hidden）。
    fn default() -> Self {
        Self::Hidden
    }
}

/// 冲突会话（单任务「全部应用」策略表；作用域 = 本任务会话）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictSession {
    /// 任务 ID
    pub job_id: u64,
    /// 「全部应用」决策（Abort 一次性不缓存——core 语义）
    pub decision: ConflictDecision,
    /// 待决请求 ID（会话期间的最新冲突请求；answer 校验用）
    pub pending_request_id: RequestId,
}

/// 冲突弹窗待决态（无会话命中时展示 path/existing）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictPrompt {
    /// 任务 ID
    pub job_id: u64,
    /// 待决请求 ID
    pub request_id: RequestId,
    /// 待写路径
    pub path: std::path::PathBuf,
    /// 现存条目简要信息（展示）
    pub existing: ExistingBrief,
}

/// 现存条目简要（冲突请求携带；行数据面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExistingBrief {
    /// 是否为目录
    pub is_dir: bool,
    /// 大小（目录为 0）
    pub size: u64,
    /// 修改时间（Unix 秒；无则 None）
    pub mtime: Option<i64>,
}

impl From<&hzip_core::jobs::FileBrief> for ExistingBrief {
    fn from(f: &hzip_core::jobs::FileBrief) -> Self {
        Self {
            is_dir: f.is_dir,
            size: f.size,
            mtime: f.mtime,
        }
    }
}

/// 密码门动作（无任务态；设计稿 11 §4.3 + 用户口径 2026-09-28 会话密码缓存）。
///
/// 与「任务中密码请求」（`PasswordDialogState`）并列：门是**无 job** 的
/// 前置问询——验证通过才放行原操作，且密码按 [`BrowseGate::key`] 缓存。
/// 条目打开**形态**（用户口径 2026-09-28）。命名避开
/// `app::settings_registry::OpenTarget`——那是设置页的「打开日志目录/配置文件」动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenForm {
    /// 按扩展名分流设置-查看器（图片 → `[ui].image_viewer`，其余 → `[ui].editor`），
    /// 都为空 → 系统默认程序（双击 / 右键「打开」）
    Configured,
    /// **系统原生「打开方式」询问框**（`SHOpenWithDialog`；右键「打开方式…」）
    SystemPicker,
}

impl OpenForm {
    /// 日志/诊断短名。
    pub fn kind(&self) -> &'static str {
        match self {
            OpenForm::Configured => "configured",
            OpenForm::SystemPicker => "system-picker",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateAction {
    /// 头加密档案打开：带密码重开整档（`shell::begin_archive_gate_reopen` 消费）
    OpenArchive,
    /// 打开当前会话内的加密条目：验证通过后重跑打开（**携带目标 id/名 +
    /// 打开程序**——门弹出期间的选择集变化/门内重跑都不该让后续流程落空或丢失
    /// 用户已选的程序；用户口径 2026-09-28 实测「输入密码后不继续」）
    OpenSelection {
        /// 条目 id（会话内下标）
        id: hzip_format::EntryId,
        /// 条目名（进度窗/任务中心展示 + 重读时定位）
        name: String,
        /// 打开形态（配置分流 / 系统「打开方式」询问框）
        form: crate::viewmodel::router::OpenForm,
    },
    /// 双击进入当前会话内的加密内层容器：验证通过后重跑 `open_container`
    OpenContainer(String),
}

impl GateAction {
    /// 动作短名（日志/诊断用；不含路径与条目名，避免日志噪音）。
    pub fn kind(&self) -> &'static str {
        match self {
            GateAction::OpenArchive => "open-archive",
            GateAction::OpenSelection { .. } => "open-selection",
            GateAction::OpenContainer(_) => "open-container",
        }
    }
}

/// 浏览密码门（无任务；设计稿 11 §4.3 + 用户口径 2026-09-28）。
#[derive(Debug, Clone)]
struct BrowseGate {
    /// 门动作（验证通过后放行）
    action: GateAction,
    /// 档案路径（**会话密码缓存键**：验证通过后按此键缓存 → 同一档案后续
    /// 打开文件/解压不再询问）
    key: std::path::PathBuf,
    /// 已失败次数（≥ PASSWORD_MAX_RETRIES → 解门）
    attempts: u8,
}

/// 弹窗路由状态机（02 §11.3 的 ViewModel 半场）。
#[derive(Debug, Default)]
pub struct DialogRouter {
    /// 密码三态（Hidden 默认）
    password: PasswordDialogState,
    /// 浏览密码门（None = 未武装）
    browse_gate: Option<BrowseGate>,
    /// 门验证通过后待执行的动作（主线程泵取出重跑；见 [`Self::gate_resolve`]）
    pending_gate_action: Option<GateAction>,
    /// 冲突会话（None = 无待决冲突；会话表随任务生命周期）
    conflict: Option<ConflictPrompt>,
    /// 活跃任务清单（job_id → 会话记忆）；任务终局清理
    sessions: std::collections::HashMap<u64, ConflictSession>,
    /// 各任务待决 job_id（弹窗归属；容量 1 语义由 core 通道保证）
    active_job: Option<u64>,
}

impl DialogRouter {
    /// 新建（Hidden + 无会话）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 武装浏览密码门（已武装 → false，不重复置位）。
    pub fn arm_gate(&mut self, action: GateAction, key: std::path::PathBuf) -> bool {
        if self.browse_gate.is_some() {
            return false;
        }
        self.browse_gate = Some(BrowseGate {
            action,
            key,
            attempts: 0,
        });
        true
    }

    /// 门动作（None = 未武装）。
    pub fn gate_action(&self) -> Option<&GateAction> {
        self.browse_gate.as_ref().map(|g| &g.action)
    }

    /// 门缓存键（档案路径；None = 未武装）。
    pub fn gate_key(&self) -> Option<std::path::PathBuf> {
        self.browse_gate.as_ref().map(|g| g.key.clone())
    }

    /// 门已尝试次数（投影用；None = 未武装；0 = 首问）。
    pub fn gate_attempts(&self) -> Option<u8> {
        self.browse_gate.as_ref().map(|g| g.attempts)
    }

    /// 一次失败（attempts+1；达 `PASSWORD_MAX_RETRIES` 解门 → false）。
    pub fn gate_fail(&mut self) -> bool {
        let Some(g) = self.browse_gate.as_mut() else {
            return false;
        };
        g.attempts = g.attempts.saturating_add(1);
        if g.attempts >= PASSWORD_MAX_RETRIES {
            self.browse_gate = None;
            return false;
        }
        true
    }

    /// 解门（取消/超限）。
    pub fn gate_disarm(&mut self) {
        self.browse_gate = None;
    }

    /// 验证通过：解门 + 把动作移入待执行槽（主线程泵取出重跑）。
    ///
    /// 返回是否确有门被放行（false = 门未武装，调用方可忽略）。
    pub fn gate_resolve(&mut self) -> bool {
        match self.browse_gate.take() {
            Some(gate) => {
                self.pending_gate_action = Some(gate.action);
                true
            }
            None => false,
        }
    }

    /// 取出待执行的门动作（泵消费；取出后清空）。
    pub fn take_pending_gate_action(&mut self) -> Option<GateAction> {
        self.pending_gate_action.take()
    }

    /// 当前密码弹窗状态（T42 绑定）。
    pub fn password_state(&self) -> &PasswordDialogState {
        &self.password
    }

    /// 当前冲突待决（T42 绑定；None = 无冲突弹窗）。
    pub fn conflict_prompt(&self) -> Option<&ConflictPrompt> {
        self.conflict.as_ref()
    }

    /// 是否有任何弹窗打开（密码或冲突；互斥——通道容量 1）。
    pub fn dialog_open(&self) -> bool {
        !matches!(self.password, PasswordDialogState::Hidden) || self.conflict.is_some()
    }

    /// 收容一个请求（pump 从 `handle.requests()` 收取后调用）。
    ///
    /// `job_id`：来源任务（请求本身不带 job_id——pump 知道来源 handle）。
    /// 返回是否有 UI 需要展示的新弹窗状态变化。
    pub fn intake(&mut self, job_id: u64, req: JobRequest) -> bool {
        match req {
            JobRequest::Password { entry, reason } => {
                self.conflict = None; // 弹窗互斥
                self.active_job = Some(job_id);
                // 请求 ID 由 worker 单调分配；router 侧无直接可见 id——core
                // `answer(req_id, …)` 的 req_id 参数当前被 worker 忽略（单调性
                // 由 need() 内部登记复验），GUI 应答一律传 0（T30 冻结签名：
                // handle.answer 校验态而非 id；见 core jobs/handle.rs rustdoc）。
                let request_id = 0;
                self.password = match reason {
                    // 首次需要密码
                    PasswordReason::Initial | PasswordReason::HeaderEncrypted => {
                        PasswordDialogState::Awaiting {
                            job_id,
                            request_id,
                            target: entry,
                            reason,
                        }
                    }
                    // 上次密码错误 → 重试态（若此前在 Awaiting/Retry）
                    PasswordReason::WrongPassword => {
                        let attempts = match &self.password {
                            PasswordDialogState::Retry { attempts, .. } => {
                                attempts.saturating_add(1)
                            }
                            PasswordDialogState::Awaiting { .. } => 1,
                            PasswordDialogState::Hidden => 0,
                        };
                        PasswordDialogState::Retry {
                            job_id,
                            request_id,
                            target: entry,
                            attempts,
                            last_reason: reason,
                        }
                    }
                    // #[non_exhaustive] 未知原因 → 保守按首次等待
                    _ => PasswordDialogState::Awaiting {
                        job_id,
                        request_id,
                        target: entry,
                        reason: PasswordReason::Initial,
                    },
                };
                true
            }
            JobRequest::Conflict { path, existing } => {
                self.password = PasswordDialogState::Hidden; // 弹窗互斥
                self.active_job = Some(job_id);
                let brief = ExistingBrief::from(&existing);
                // 会话命中（同任务已「全部应用」非 Abort 决策）→ 自动应答、
                // 不弹窗——但本方法无句柄，返回 false + 记录自动应答由
                // pump 侧查表执行（见 [`auto_decision_for`]）。
                if self
                    .sessions
                    .get(&job_id)
                    .is_some_and(|s| s.decision != ConflictDecision::Abort)
                {
                    self.conflict = None;
                    if let Some(s) = self.sessions.get_mut(&job_id) {
                        s.pending_request_id = 0; // 更新待决 id（pump 自动应答）
                    }
                    return false;
                }
                self.conflict = Some(ConflictPrompt {
                    job_id,
                    request_id: 0,
                    path,
                    existing: brief,
                });
                true
            }
            // #[non_exhaustive] 未知请求变体：不弹窗（worker 超时路径默认处理）
            _ => false,
        }
    }

    /// 会话自动决策查询（pump 冲突请求处理用）：同任务已「全部应用」→
    /// 返回会话决策（自动应答不弹窗）；否则 None。
    pub fn auto_decision_for(&self, job_id: u64) -> Option<ConflictDecision> {
        let d = self.sessions.get(&job_id).map(|s| s.decision)?;
        // Abort 一次性不缓存（core 语义：Abort = 中止任务，无「后续自动」）
        if d == ConflictDecision::Abort {
            None
        } else {
            Some(d)
        }
    }

    /// 用户提交密码（Awaiting/Retry → `handle.answer(Password)`）。
    ///
    /// 提交后状态复位 Hidden（任务继续/重问由下次请求驱动）。
    ///
    /// # 重试超限（02 §11.4：达 `PASSWORD_MAX_RETRIES`(3) → cancel）
    ///
    /// core worker 对错密码以 `PasswordReason::WrongPassword` 重问，自身收敛
    /// （core `PASSWORD_MAX_RETRIES` 同值）；GUI 侧在 `Retry` 态提交且
    /// `attempts` 已达上限时，本次密码视为最后一次尝试——若仍错（下次
    /// WrongPassword intake 到达）由 pump 判 `attempts >= MAX` 直接
    /// `handle.cancel()`；此处对已达上限的提交**照常应答**（给最后一次机会），
    /// 不提前取消。真正「重试 3 次 → cancel」的判定在 intake 侧
    /// （[`should_cancel_password`]）。
    pub fn submit_password(
        &mut self,
        handle: &JobHandle,
        pw: Zeroizing<String>,
    ) -> Result<(), HzipError> {
        let state = std::mem::replace(&mut self.password, PasswordDialogState::Hidden);
        let (job_id, request_id) = match state {
            PasswordDialogState::Awaiting {
                job_id, request_id, ..
            }
            | PasswordDialogState::Retry {
                job_id, request_id, ..
            } => (job_id, request_id),
            PasswordDialogState::Hidden => {
                return Err(HzipError::Unsupported {
                    what: crate::tr("msg.password_no_pending").into(),
                });
            }
        };
        // 密码明文只在提交瞬态传递（answer 通道）；不落任何行/日志面
        handle.answer(request_id, JobResponse::Password(pw))?;
        self.clear_active_if(job_id);
        Ok(())
    }

    /// 用户提交冲突决策（`apply_to_all=true` → 写会话策略表，02 §11.4）。
    ///
    /// `Abort` 为一次性决策（core 语义：不缓存——缓存会让后续冲突直接
    /// 中止而非逐个处理）；`apply_to_all` 且决策非 Abort → 写会话表。
    pub fn submit_conflict(
        &mut self,
        handle: &JobHandle,
        decision: ConflictDecision,
        apply_to_all: bool,
    ) -> Result<(), HzipError> {
        let Some(prompt) = self.conflict.take() else {
            return Err(HzipError::Unsupported {
                what: crate::tr("msg.conflict_no_pending").into(),
            });
        };
        if apply_to_all && decision != ConflictDecision::Abort {
            self.sessions.insert(
                prompt.job_id,
                ConflictSession {
                    job_id: prompt.job_id,
                    decision,
                    pending_request_id: prompt.request_id,
                },
            );
        }
        handle.answer(prompt.request_id, JobResponse::Conflict(decision))?;
        self.clear_active_if(prompt.job_id);
        Ok(())
    }

    /// 用户点「取消」→ `handle.cancel()` + 复位当前弹窗（密码或冲突）。
    pub fn cancel_request(&mut self, handle: &JobHandle) {
        handle.cancel();
        self.password = PasswordDialogState::Hidden;
        self.conflict = None;
        // 活动任务标记保留（终局事件到达时统一清理）
    }

    /// 任务终局消费（pump 见 `JobFinished` 调用）：复位该任务弹窗 + 清会话。
    ///
    /// 120 s 决策超时（HZ-5002 Failed）路径同样经此复位——弹窗不残留。
    pub fn on_job_finished(&mut self, job_id: u64) {
        self.sessions.remove(&job_id);
        if let Some(active) = self.active_job
            && active == job_id
        {
            self.active_job = None;
            self.password = PasswordDialogState::Hidden;
            self.conflict = None;
        }
    }

    /// 活动任务 id（None = 无待决）。
    pub fn active_job(&self) -> Option<u64> {
        self.active_job
    }

    /// 清活动标记（应答后任务继续；仅在无其它待决时清）。
    fn clear_active_if(&mut self, job_id: u64) {
        if self.active_job == Some(job_id)
            && matches!(self.password, PasswordDialogState::Hidden)
            && self.conflict.is_none()
        {
            self.active_job = None;
        }
    }

    /// 测试辅助：复位弹窗（同 [`cancel_request`] 语义但不需 handle——
    /// 纯状态机测试在无真实句柄下验证复位）。
    #[cfg(test)]
    pub(crate) fn cancel_request_without_handle(&mut self) {
        self.password = PasswordDialogState::Hidden;
        self.conflict = None;
        self.active_job = None;
    }

    /// 测试辅助：当前 Retry 态的重试计数（非 Retry 态返回 None）。
    #[cfg(test)]
    pub(crate) fn password_retry_count(&self) -> Option<u8> {
        match &self.password {
            PasswordDialogState::Retry { attempts, .. } => Some(*attempts),
            _ => None,
        }
    }

    /// 测试辅助：注入会话 apply-all 决策（模拟用户勾选「全部应用」）。
    #[cfg(test)]
    pub(crate) fn apply_all_for_test(&mut self, job_id: u64, decision: ConflictDecision) {
        self.sessions.insert(
            job_id,
            ConflictSession {
                job_id,
                decision,
                pending_request_id: 0,
            },
        );
        // 若当前有待决冲突弹窗且命中 → 自动应答路径（auto_decision_for）
        if let Some(p) = &self.conflict
            && p.job_id == job_id
        {
            let _ = self.auto_decision_for(job_id);
        }
    }
}

#[cfg(test)]
#[path = "tests/router.rs"]
mod tests;
