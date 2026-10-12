//! 进度事件与结果类型（02 §4.3/§4.4；TC-CORE-010）。
//!
//! [`OrderValidator`] 为事件顺序合法性校验器（pub 测试工具，rustdoc 注明
//! 「非冻结、内部协作用」，T30 core 层直接复用）。

use std::path::PathBuf;
use std::time::Instant;

use hzip_error::HzipError;

/// 条目名承载（占位）：02 §7.2 的 `EntryName` 正式归属 hzip-format（T20 交付）。
/// 本 crate 先行以 `SmolStr` 类型别名占位，T20 冻结前统一 re-export 对齐；
/// 字段名与语义已按 02 §4.4 落地，不随承载类型改变。
pub type EntryName = smol_str::SmolStr;

/// 任务 ID（core 侧引用，02 §4.4）。
pub type JobId = u64;

/// 聚合统计载体（02 §4.4）。字段集「非冻结」：core 聚合器可扩展。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobStats {
    /// 已完成字节数
    pub bytes_done: u64,
    /// 总字节数
    pub bytes_total: u64,
    /// 已完成条目数
    pub entries_done: u64,
    /// 总条目数
    pub entries_total: u64,
    /// 任务开始时刻
    pub started: Instant,
    /// 任务结束时刻（进行中为 `None`）
    pub finished: Option<Instant>,
}

impl JobStats {
    /// 空统计（任务启动瞬间）。
    pub fn new(bytes_total: u64, entries_total: u64) -> Self {
        Self {
            bytes_done: 0,
            bytes_total,
            entries_done: 0,
            entries_total,
            started: Instant::now(),
            finished: None,
        }
    }

    /// 标记完成并记录结束时刻。
    pub fn finish(&mut self) {
        self.finished = Some(Instant::now());
    }
}

/// 单条目失败明细（02 §4.4 `Partial` 载体）。
#[derive(Debug, Clone)]
pub struct EntryFailure {
    /// 失败条目名
    pub entry: EntryName,
    /// 原始错误
    pub error: Box<HzipError>,
}

/// 任务终局（02 §4.4）。`#[non_exhaustive]`：后续只增变体。
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Outcome {
    /// 全部成功
    Done(JobStats),
    /// 部分失败（best_effort 聚合）
    Partial(JobStats, Vec<EntryFailure>),
    /// 失败
    Failed(HzipError),
    /// 已取消（副作用信息走 `JobFinished` 的 outcome，02 §3.5）
    Cancelled,
    /// 取消/回滚时列出的已清理文件
    RolledBack(Vec<PathBuf>),
}

/// 进度事件（02 §4.4 冻结签名）。`#[non_exhaustive]`：消费者必须带 `_` 分支。
///
/// 事件顺序合法性规范（本 crate 定义，[`OrderValidator`] 断言依据）：
/// 1. `JobStarted` 必须首个且恰一次；
/// 2. `JobTotal`（如出现）只能在 `JobStarted` 之后、首个 `EntryStarted` 之前恰一次；
/// 3. 每个 `EntryStarted` 到对应 `EntryDone` 间只允许 `Bytes`（可零个）；
/// 4. `JobFinished` 必须最后且恰一次；
/// 5. `Done` 结果下 `Σ(Bytes.delta) == total_bytes`（总量取 `JobTotal` 补报值，
///    未补报时回落 `JobStarted` 值）。
///
/// 因 `HzipError` 在 T01 已补 `Clone`（Io 变体按 os error/kind 重建），
/// 本类型可整体 `Clone`（bus 广播逐订阅者 clone 需要，02 §4.1）。
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// 任务开始
    JobStarted {
        /// 任务 ID
        job_id: JobId,
        /// 总字节数
        total_bytes: u64,
        /// 总条目数
        total_entries: u64,
    },
    /// 任务总量补报（JobStarted 骨架期恒 0——服务层构建计划后补发真实值；
    /// GUI 进度条百分比 / CLI 进度行的数据源）
    JobTotal {
        /// 任务 ID
        job_id: JobId,
        /// 总字节数
        total_bytes: u64,
        /// 总条目数
        total_entries: u64,
    },
    /// 条目开始
    EntryStarted {
        /// 条目名
        entry: EntryName,
        /// 已声明字节数
        bytes: u64,
    },
    /// 条目结束
    EntryDone {
        /// 条目名
        entry: EntryName,
        /// 实际字节数
        bytes: u64,
        /// CRC 是否通过
        crc_ok: bool,
    },
    /// 字节增量（高频原始事件）
    Bytes {
        /// 增量字节
        delta: u64,
    },
    /// 任务结束
    JobFinished {
        /// 任务 ID
        job_id: JobId,
        /// 终局
        outcome: Outcome,
    },
}

/// 校验器当前状态（内部）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    InEntry,
}

/// 事件顺序合法性校验器（非冻结测试工具；02 §4.3 事件规范，TC-CORE-010 断言依据）。
#[derive(Debug)]
pub struct OrderValidator {
    phase: Phase,
    seen_started: bool,
    seen_total: bool,
    seen_finished: bool,
    bytes_sum: u64,
    total_bytes: u64,
}

impl Default for OrderValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderValidator {
    /// 空校验器。
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            seen_started: false,
            seen_total: false,
            seen_finished: false,
            bytes_sum: 0,
            total_bytes: 0,
        }
    }

    /// 吞入一个事件；非法序列返回描述性错误。
    pub fn push(&mut self, ev: &ProgressEvent) -> Result<(), String> {
        match ev {
            ProgressEvent::JobStarted { total_bytes, .. } => {
                if self.seen_started || self.seen_finished || self.phase != Phase::Idle {
                    return Err("JobStarted 必须是首个事件且恰一次".into());
                }
                self.seen_started = true;
                self.total_bytes = *total_bytes;
                Ok(())
            }
            ProgressEvent::JobTotal { total_bytes, .. } => {
                if !self.seen_started || self.seen_finished || self.seen_total {
                    return Err("JobTotal 只能在 JobStarted 之后、JobFinished 之前恰一次".into());
                }
                if self.phase == Phase::InEntry {
                    return Err("JobTotal 只能出现在首个 EntryStarted 之前".into());
                }
                self.seen_total = true;
                self.total_bytes = *total_bytes;
                Ok(())
            }
            ProgressEvent::EntryStarted { .. } => {
                if !self.seen_started || self.seen_finished {
                    return Err("EntryStarted 只能出现在 Started 之后、Finished 之前".into());
                }
                if self.phase == Phase::InEntry {
                    return Err("EntryStarted 必须与 EntryDone 配对（不允许嵌套/重入）".into());
                }
                self.phase = Phase::InEntry;
                Ok(())
            }
            ProgressEvent::Bytes { delta } => {
                if self.phase != Phase::InEntry {
                    return Err("Bytes 只能出现在 EntryStarted 与对应 EntryDone 之间".into());
                }
                self.bytes_sum = self.bytes_sum.saturating_add(*delta);
                Ok(())
            }
            ProgressEvent::EntryDone { .. } => {
                if self.phase != Phase::InEntry {
                    return Err("EntryDone 无配对 EntryStarted".into());
                }
                self.phase = Phase::Idle;
                Ok(())
            }
            ProgressEvent::JobFinished { .. } => {
                if !self.seen_started || self.seen_finished {
                    return Err("JobFinished 必须恰好一次且出现在 Started 之后".into());
                }
                self.seen_finished = true;
                Ok(())
            }
        }
    }

    /// 流结束校验：`JobFinished` 必须已见；返回字节守恒状态。
    pub fn finish(&self) -> Result<(), String> {
        if !self.seen_started {
            return Err("缺少 JobStarted".into());
        }
        if !self.seen_finished {
            return Err("缺少 JobFinished".into());
        }
        if self.phase == Phase::InEntry {
            return Err("存在未配对的 EntryStarted".into());
        }
        Ok(())
    }

    /// `Done` 语义下 `Σ(Bytes.delta) == total_bytes` 守恒判定（02 §4.3 规则 5）。
    pub fn bytes_conserved(&self) -> bool {
        self.bytes_sum == self.total_bytes
    }

    /// 累计字节增量（测试/聚合用）。
    pub fn bytes_sum(&self) -> u64 {
        self.bytes_sum
    }
}

/// 便捷校验：整段事件流合法则 `Ok(())`（含 `finish` 语义）。
pub fn validate_stream(events: &[ProgressEvent]) -> Result<(), String> {
    let mut v = OrderValidator::new();
    for ev in events {
        v.push(ev)?;
    }
    v.finish()
}

#[cfg(test)]
#[path = "tests/event.rs"]
mod tests;
