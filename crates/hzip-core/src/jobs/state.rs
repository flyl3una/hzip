//! JobState 状态机（02 §9.3 图穷举 + 迁移合法性表）。
//!
//! 终态（`Done`/`PartialDone`/`Failed`/`Cancelled`）无出边。所有迁移由
//! JobManager 单线程侧（Mutex 临界区）执行，杜绝竞态——worker 不直接改
//! state，经 `JobCtx` 回调提交迁移请求（见 `crate::jobs` 模块文档）。

/// 任务生命周期状态（02 §9.3 图 + `Cancelling` 中间态提炼）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// 已入队（未获门控放行）
    Queued,
    /// 执行中
    Running,
    /// 等待用户决策（密码/冲突；阻塞于应答通道，不占 CPU）
    WaitingInput,
    /// 取消中（回滚半写文件；瞬时中间态，唯一迁出 → Cancelled）
    Cancelling,
    /// 全部成功
    Done,
    /// 部分失败（best_effort 聚合）
    PartialDone,
    /// 失败
    Failed,
    /// 已取消（未启动直接移除 / 回滚完成）
    Cancelled,
}

impl JobState {
    /// 是否终态（无出边）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done | Self::PartialDone | Self::Failed | Self::Cancelled
        )
    }

    /// 迁移合法性（表驱动；`legal(from, to)`）。
    ///
    /// 02 §9.3 图穷举：
    /// - `Queued → {Running, Cancelled}`（Cancelled = 未启动直接移除，TC-CORE-011）
    /// - `Running → {WaitingInput, Cancelling, Done, PartialDone, Failed}`
    /// - `WaitingInput → {Running, Cancelling, Failed}`（Failed = 120s 决策超时 5002）
    /// - `Cancelling → {Cancelled}`
    /// - 其余组合（含终态出边、`Queued → Done` 类跳变）非法。
    pub const fn legal(from: JobState, to: JobState) -> bool {
        use JobState::*;
        matches!(
            (from, to),
            (Queued, Running)
                | (Queued, Cancelled)
                | (Running, WaitingInput)
                | (Running, Cancelling)
                | (Running, Done)
                | (Running, PartialDone)
                | (Running, Failed)
                | (WaitingInput, Running)
                | (WaitingInput, Cancelling)
                | (WaitingInput, Failed)
                | (Cancelling, Cancelled)
        )
    }

    /// 对外展示名（GUI/CLI 状态视图用）。
    pub const fn as_str(self) -> &'static str {
        use JobState::*;
        match self {
            Queued => "queued",
            Running => "running",
            WaitingInput => "waiting-input",
            Cancelling => "cancelling",
            Done => "done",
            PartialDone => "partial",
            Failed => "failed",
            Cancelled => "cancelled",
        }
    }
}

/// 状态迁移请求（worker → JobManager 回调；合法性由 JobManager 校验后落账）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transition {
    /// Queued → Running（门控放行）
    Start,
    /// Running → WaitingInput
    WaitInput,
    /// WaitingInput → Running
    Resume,
    /// {Running, WaitingInput} → Cancelling（取消请求；回滚完成 → Cancelled）
    #[allow(dead_code)] // 供显式两段取消（骨架期取消直接收口 Cancelled）
    BeginCancel,
    /// Cancelling → Cancelled（回滚完成）
    FinishCancel,
    /// → Done
    FinishDone,
    /// → PartialDone
    FinishPartial,
    /// → Failed
    FinishFailed,
}

impl Transition {
    /// 迁移请求对应的目标态（非法组合由调用方经 [`JobState::legal`] 预校验）。
    pub const fn target(self, _from: JobState) -> JobState {
        use JobState::*;
        match self {
            Self::Start => Running,
            Self::WaitInput => WaitingInput,
            Self::Resume => Running,
            Self::BeginCancel => Cancelling,
            Self::FinishCancel => Cancelled,
            Self::FinishDone => Done,
            Self::FinishPartial => PartialDone,
            Self::FinishFailed => Failed,
        }
    }

    /// 迁移请求的合法性（按当前态判定）。
    pub const fn legal_from(self, from: JobState) -> bool {
        JobState::legal(from, self.target(from))
    }
}
