//! 任务规格与单任务槽：`JobKind` / `JobSpec` / `JobSlot`（JobManager 内部状态）。
//!
//! 从 `jobs/mod.rs` 迁出（T-STD：`mod.rs` 只做声明与再导出）；内容逐字未改。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};
use hzip_progress::{CancellationToken, JobStats, Outcome, ProgressBus};

use super::request::{JobRequest, JobResponse, RequestId};
use super::state::JobState;

/// 任务种类（02 §9.3 JobKind；T30 分派骨架，T32/T33 填实）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// 解压（T32）
    Extract,
    /// 完整性测试（T32）
    Test,
    /// 压缩（T33）
    Compress,
    /// 格式转换（T33）
    Convert,
    /// 编辑保存（T33）
    EditSave,
    /// 批量编辑提交（T-MENU6：ChangeSet → rewrite 后台任务）
    EditBatch,
}

/// 任务规格（02 §9.3；字段集「非冻结」——T32~T34 追加 services 所需参数，
/// rustdoc 标注；T30 交付最小骨架供引擎机制与集成测试驱动）。
///
/// `Debug` 为**手写实现**：`password` 只打印 `Some(<redacted>)`/`None`，
/// 明文绝不进日志/诊断（红线：密码不落日志）。
#[non_exhaustive]
#[derive(Clone)]
pub struct JobSpec {
    /// 任务种类
    pub kind: JobKind,
    /// 源档案路径
    pub source: PathBuf,
    /// 输出/解压目标目录（按 kind 语义）
    pub dest: Option<PathBuf>,
    /// 输出文件路径（压缩/转换向）
    pub out: Option<PathBuf>,
    /// 解压选项（T32；`JobKind::Extract` 消费；None → [`ExtractOptions::default`]）
    pub extract: Option<crate::services::extract::ExtractOptions>,
    /// 压缩选项（T33；`JobKind::Compress`/`Convert` 消费；None → [`CompressSpec::default`]）
    pub compress: Option<crate::services::compress::CompressSpec>,
    /// 批量编辑变更集（T-MENU6；`JobKind::EditBatch` 消费；None → 空 ChangeSet）
    pub changes: Option<hzip_format::ChangeSet>,
    /// 档案密码（用户口径 2026-09-28；`EditSave`/`EditBatch` 消费）。
    ///
    /// 加密档案重写（读旧条目 / 写新条目）所需；`None` → 由服务层经
    /// [`JobRequest::Password`] 上抛询问。未加密档案忽略。
    pub password: Option<zeroize::Zeroizing<String>>,
}

impl std::fmt::Debug for JobSpec {
    /// 手写：`password` 脱敏（`Zeroizing` 的派生 Debug 会打印明文）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobSpec")
            .field("kind", &self.kind)
            .field("source", &self.source)
            .field("dest", &self.dest)
            .field("out", &self.out)
            .field("extract", &self.extract)
            .field("compress", &self.compress)
            .field("changes", &self.changes)
            .field(
                "password",
                // `format_args!`：`&str` 的 Debug 会带引号，这里要裸字面
                &format_args!(
                    "{}",
                    if self.password.is_some() {
                        "Some(<redacted>)"
                    } else {
                        "None"
                    }
                ),
            )
            .finish()
    }
}

impl JobSpec {
    /// 最小构造：kind + 源（其余字段 None，T32/T33 追加 builder）。
    pub fn new(kind: JobKind, source: PathBuf) -> Self {
        Self {
            kind,
            source,
            dest: None,
            out: None,
            extract: None,
            compress: None,
            changes: None,
            password: None,
        }
    }

    /// 解压任务构造：kind=Extract + 解压选项（T32；T50 黑盒消费面）。
    pub fn extract_job(source: PathBuf, opts: crate::services::extract::ExtractOptions) -> Self {
        Self {
            kind: JobKind::Extract,
            source,
            dest: opts.dest_root.clone(),
            out: None,
            extract: Some(opts),
            compress: None,
            changes: None,
            password: None,
        }
    }
}

/// 单任务槽（JobManager 内部状态；字段 pub(crate) 供 `jobs::handle` 访问）。
#[allow(dead_code)] // T32/T33 services 消费；骨架期保留
pub(crate) struct JobSlot {
    pub(crate) state: JobState,
    /// 任务规格（**Arc 共享**）：调度算资源键（[`JobManager::keys_of`]）与池
    /// worker 取 spec（[`JobManager::handle_job`]）都只需借用——旧实现这两处各
    /// 整份深拷贝一次（`JobSpec` 含路径/选项/变更集）。
    pub(crate) spec: Arc<JobSpec>,
    /// 子取消令牌（父 = Engine 根令牌；级联）
    pub(crate) child: CancellationToken,
    /// 进度总线（每任务独立扇出；JobHandle 订阅；Arc 共享）
    pub(crate) bus: Arc<ProgressBus>,
    /// 请求通道（worker → UI；容量 1）
    pub(crate) req_tx: Sender<JobRequest>,
    /// 应答通道（UI → worker；每请求一次）
    pub(crate) ans_tx: Sender<JobResponse>,
    /// 应答接收端（worker 侧 JobCtx 用；Option——worker 构造 ctx 时 take，
    /// 单次执行后槽内不再需要）
    pub(crate) ans_rx: Option<Receiver<JobResponse>>,
    /// 终局（JobManager 写入；wait() 读取）
    pub(crate) outcome: Option<Outcome>,
    /// 完成信号（终态写入时 send；wait() 阻塞 recv——不轮询不占 CPU）
    pub(crate) done_tx: Sender<()>,
    /// 快照统计
    pub(crate) stats: JobStats,
    /// 待决请求登记（`need` 在**上抛请求之前**置位，应答消费后清空）。
    ///
    /// 为什么放槽里（2026-09-29 实测竞态）：worker 的 `WaitingInput` 是**异步**
    /// 消息经调度线程落账的，`JobHandle::answer` 只看状态就会在「请求已发出、
    /// 状态还没切过去」的窗口里把用户的应答当 stale 丢掉——应答被吞 → 任务只能
    /// 干等到 120 s 决策超时（界面表现为「输了密码没反应」）。登记位是同步写入，
    /// 故 `answer` 以「状态 or 登记」双判据放行。
    pub(crate) pending_req: Arc<Mutex<Option<RequestId>>>,
}
