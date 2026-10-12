//! JobHandle：任务句柄（02 §9.11 冻结签名）。
//!
//! 调用方（CLI/GUI）持句柄订阅进度、收取请求、应答、取消、等待终局。
//! 请求通道容量 1（[`REQUEST_CHANNEL_CAP`]）；决策超时 [`REQUEST_TIMEOUT_SECS`]
//! 由 worker 侧 `recv_timeout` 统一裁决。

use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};
use hzip_error::HzipError;
use hzip_progress::{JobId, ProgressBus};

use super::state::JobState;
use super::{JobRequest, JobResponse, JobResult, JobSlot};

/// 任务句柄（02 §9.11）。
pub struct JobHandle {
    job_id: JobId,
    bus: Arc<ProgressBus>,
    req_rx: Receiver<JobRequest>,
    ans_tx: Sender<JobResponse>,
    /// 完成信号（终态 → send；wait 阻塞 recv）
    done_rx: Receiver<()>,
    slot: Arc<Mutex<JobSlot>>,
}

impl std::fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHandle")
            .field("job_id", &self.job_id)
            .finish()
    }
}

impl JobHandle {
    pub(crate) fn new(
        job_id: JobId,
        bus: Arc<ProgressBus>,
        req_rx: Receiver<JobRequest>,
        ans_tx: Sender<JobResponse>,
        done_rx: Receiver<()>,
        slot: Arc<Mutex<JobSlot>>,
    ) -> Self {
        Self {
            job_id,
            bus,
            req_rx,
            ans_tx,
            done_rx,
            slot,
        }
    }

    /// 任务 ID。
    pub fn job_id(&self) -> JobId {
        self.job_id
    }

    /// 进度事件流（`subscribe` 获取独立扇出接收端；JobStarted 首发、
    /// JobFinished 末发——[`ProgressEvent`] 顺序规范）。
    pub fn events(&self) -> hzip_progress::EventReceiver {
        self.bus.subscribe()
    }

    /// 请求接收端（容量 1：同时最多一个待决问题）。
    pub fn requests(&self) -> &Receiver<JobRequest> {
        &self.req_rx
    }

    /// 应答请求。
    ///
    /// 校验：job 处于 `WaitingInput` 才接受；否则 `Err(HzipError::Unsupported{
    /// 5003 语义})`——防过期/乱序应答串到下一个问题。
    pub fn answer(&self, req_id: super::RequestId, resp: JobResponse) -> Result<(), HzipError> {
        let (state, pending) = {
            let s = self.slot.lock().unwrap_or_else(|p| p.into_inner());
            let pending = s
                .pending_req
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some();
            (s.state, pending)
        };
        // 放行判据 =「已在 WaitingInput」**或**「请求已登记待决」：`WaitingInput`
        // 是异步消息落账的，请求发出到状态切过去之间有一个窗口——只看状态会把
        // 这个窗口里的应答当 stale 丢掉（应答被吞 → 任务干等到 120 s 决策超时，
        // 界面上就是「输了密码没反应」，2026-09-29 实测）。登记位由 `need` 在
        // **上抛请求之前**同步写入，故两者任一成立即可投递。
        if state != JobState::WaitingInput && !pending {
            return Err(HzipError::Unsupported {
                what: format!("hz-5003 stale-answer (job {})", self.job_id).into(),
            });
        }
        // 应答通道容量 1：worker 正阻塞等——send 必成功（除非 worker 刚超时离开）
        self.ans_tx.send(resp).map_err(|_| HzipError::Unsupported {
            what: format!("hz-5003 answer-channel-closed (job {})", self.job_id).into(),
        })?;
        let _ = req_id; // RequestId 单调性由 worker 侧复验（need 内登记）
        Ok(())
    }

    /// 取消（幂等；级联子令牌 → worker 下个检查点感知）。
    pub fn cancel(&self) {
        let child = {
            let s = self.slot.lock().unwrap_or_else(|p| p.into_inner());
            s.child.clone()
        };
        child.cancel();
    }

    /// 当前状态快照（GUI 队列视图/测试断言）。
    pub fn state(&self) -> JobState {
        let s = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        s.state
    }

    /// 阻塞等待终局（02 §9.11 `wait`）。返回 `JobResult`。
    ///
    /// 基于完成信号（通道 recv）阻塞——不轮询不占 CPU；终态必然收到
    /// 完成信号（JobManager 保证）。`Failed`/`Cancelled`/`RolledBack` 收敛进
    /// `Outcome`（错误码语义不丢失）。
    pub fn wait(self) -> JobResult {
        // 等完成信号
        let _ = self.done_rx.recv();
        // 读终局（终态 + outcome）
        let mut s = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        if s.state.is_terminal()
            && let Some(o) = s.outcome.take()
        {
            return Ok(o);
        }
        Err(HzipError::Unsupported {
            what: format!("hz-0000 terminal-without-outcome (job {})", self.job_id).into(),
        })
    }
}

#[cfg(test)]
#[path = "tests/handle.rs"]
mod tests;
