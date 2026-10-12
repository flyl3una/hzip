//! JobManager：任务队列/状态机/并发门控/级联取消（02 §9.3）。
//!
//! # 归一化裁定（杜绝状态竞态）
//!
//! 全部状态迁移走本模块 `Mutex` 临界区；worker 不直接改 state，经
//! [`JobCtx`] 回调（`set_waiting() / back_to_running() / finish(outcome)`）
//! 提交迁移请求，由 JobManager 校验合法性后落账。
//!
//! # 队列与池分离
//!
//! 队列（含门控回退语义）在本模块：一个 `Mutex<VecDeque<JobId>>` +
//! Condvar。池通道用 rendezvous（`bounded(0)`）传递「已获准运行」的票，
//! 池 worker 数与 `jobs.max_parallel` 解耦（门控上限 = `min(max_parallel,
//! 池大小)`）。调度线程取队头 → 门控放行 → 发票 → worker 执行。
//!
//! # 级联取消（02 §4.2）
//!
//! `root.child()` 为每 job 建子令牌（父取消 → 子自动取消）；`cancel()` 调
//! 子令牌 `cancel()`。队列中未启动任务的「直接移除」：调度循环取队头前
//! 检查 `child.is_cancelled()` → `Queued→Cancelled` 迁移 + 不出队。
//!
//! # 子模块（T-STD：本文件只做声明与再导出）
//!
//! | 文件 | 内容 |
//! |---|---|
//! | `slot` | 任务规格与槽：`JobKind` / `JobSpec` / `JobSlot` |
//! | `manager` | `JobManager` + `JobCtx`（消息循环/迁移/调度/池执行体） |
//! | `sched` | 调度纯函数（资源键推导、可派发挑选） |
//!
//! 业务项迁出后，本模块以 `pub use` / `pub(crate) use` 保持原公开面
//! （`crate::jobs::X` 与目录内 `super::X` 路径零改动）。

mod consts;
mod handle;
mod manager;
mod request;
mod sched;
mod slot;
mod state;

pub use consts::{REQUEST_CHANNEL_CAP, REQUEST_TIMEOUT_SECS};
pub use handle::JobHandle;
pub use manager::{JobCtx, JobResult};
pub use request::{
    ConflictDecision, FileBrief, JobRequest, JobResponse, PasswordReason, RequestId,
    decision_timeout_err,
};
pub use slot::{JobKind, JobSpec};
pub use state::JobState;

// —— 原 `mod.rs` 内 `pub(crate)` 项：跨文件后仍以同档可见性再导出 ——
pub(crate) use manager::JobManager;
pub(crate) use slot::JobSlot;

// —— 调度纯函数：`tests/mod_rs.rs` 经 `use super::*` 消费；业务期 `manager`
// 直接走 `super::sched::`，故仅测试期导入（免 unused 告警）——
#[cfg(test)]
use sched::{pick_dispatchable, resource_keys};

/// 调度策略（同键串行 / 异键并行）回归——纯函数，无引擎依赖。
#[cfg(test)]
#[path = "tests/mod_rs.rs"]
mod tests;
