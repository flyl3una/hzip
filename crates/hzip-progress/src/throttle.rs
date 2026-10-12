//! UI 侧节流合并器（02 §4.5；TC-CORE-010 GUI 前置）。
//!
//! GUI/CLI 进度条渲染消费方：`Throttle` 把高频 `Bytes` 按 10 Hz 窗口合并为
//! 快照；`SpeedEwma` 计算 EWMA 瞬时速度（α=0.3、500 ms 预热）；边界事件
//! （`EntryDone`/`JobFinished`）立即冲刷不等窗口。

use std::time::{Duration, Instant};

use crate::consts::{SPEED_EWMA_ALPHA, SPEED_WARMUP_MS};
use crate::event::ProgressEvent;

/// 节流窗口时长（100 ms = 1000 / 10 Hz）。
const WINDOW_MS: u64 = 1000 / crate::consts::PROGRESS_THROTTLE_HZ;

/// 剩余时间指示（02 §4.5 显示规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eta {
    /// 秒数
    Seconds(u64),
    /// < 10 s：显示「即将完成」
    AlmostDone,
}

/// 单次刷新的进度快照（GUI/CLI 进度条渲染共用）。
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressSnapshot {
    /// 已完成字节
    pub done: u64,
    /// 总字节
    pub total: u64,
    /// 瞬时速度 B/s（预热期内 None）
    pub speed: Option<f64>,
    /// 剩余时间（无法估计为 None）
    pub eta: Option<Eta>,
}

/// EWMA 速度计算（02 §4.5；α=0.3、500 ms 预热）。
#[derive(Debug)]
pub struct SpeedEwma {
    v: Option<f64>,
    started: Instant,
    alpha: f64,
}

impl Default for SpeedEwma {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeedEwma {
    /// 新建（α 取 [`SPEED_EWMA_ALPHA`]，预热期 [`SPEED_WARMUP_MS`]）。
    pub fn new() -> Self {
        Self {
            v: None,
            started: Instant::now(),
            alpha: SPEED_EWMA_ALPHA,
        }
    }

    /// 每个节流窗口调用一次：`delta_bytes` 为窗口内增量，`dt` 为窗口时长。
    /// 预热期内返回 `None`；dt 为零返回当前值；首样本直接作为初值。
    pub fn sample(&mut self, delta_bytes: u64, dt: Duration) -> Option<f64> {
        if dt.is_zero() {
            return self.v;
        }
        if self.started.elapsed().as_millis() < SPEED_WARMUP_MS as u128 {
            return None;
        }
        let rate = delta_bytes as f64 / dt.as_secs_f64();
        let next = match self.v {
            None => rate,
            Some(v) => self.alpha * rate + (1.0 - self.alpha) * v,
        };
        self.v = Some(next);
        self.v
    }

    /// 当前速度估计（无样本或预热期为 None）。
    pub fn current(&self) -> Option<f64> {
        self.v
    }
}

/// 剩余时间（02 §4.5）：`eta < 10 s → AlmostDone`；`> 24 h → None`；
/// `done == total → AlmostDone`；速度缺失/为零 → None（防除零）。
pub fn eta_from(done: u64, total: u64, speed: Option<f64>) -> Option<Eta> {
    if done >= total {
        return Some(Eta::AlmostDone);
    }
    let speed = speed?;
    if speed <= 0.0 {
        return None;
    }
    let remaining = (total - done) as f64 / speed;
    if remaining < 10.0 {
        Some(Eta::AlmostDone)
    } else if remaining > 24.0 * 3600.0 {
        None
    } else {
        Some(Eta::Seconds(remaining.ceil() as u64))
    }
}

/// 10 Hz 窗口合并器（02 §4.5）：窗口内多条 `Bytes` 累加为一条快照。
#[derive(Debug)]
pub struct Throttle {
    done: u64,
    prev_done: u64,
    total: u64,
    entries_done: u64,
    last_flush: Instant,
    ewma: SpeedEwma,
    finished: bool,
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

impl Throttle {
    /// 新建（窗口起点即创建时刻）。
    pub fn new() -> Self {
        Self {
            done: 0,
            prev_done: 0,
            total: 0,
            entries_done: 0,
            last_flush: Instant::now(),
            ewma: SpeedEwma::new(),
            finished: false,
        }
    }

    /// 吞入一个事件，可能立即产出一条快照（窗口到期或边界事件）。
    /// `JobFinished` 后不再产生快照。
    pub fn push(&mut self, ev: &ProgressEvent) -> Option<ProgressSnapshot> {
        if self.finished {
            return None;
        }
        match ev {
            ProgressEvent::JobStarted { total_bytes, .. } => {
                self.total = *total_bytes;
                self.done = 0;
                self.prev_done = 0;
                self.last_flush = Instant::now();
                Some(self.snapshot_no_speed())
            }
            ProgressEvent::JobTotal { total_bytes, .. } => {
                // 总量补报：立即冲刷（total 生效 → 进度百分比立即可算）
                self.total = *total_bytes;
                Some(self.snapshot_no_speed())
            }
            ProgressEvent::Bytes { delta } => {
                self.done = self.done.saturating_add(*delta);
                self.flush_if_due()
            }
            ProgressEvent::EntryDone { bytes, .. } => {
                self.entries_done += 1;
                // 条目边界事件：立即冲刷（不延迟）
                self.done = self.done.max(*bytes);
                Some(self.flush_with_speed())
            }
            ProgressEvent::EntryStarted { .. } => None,
            ProgressEvent::JobFinished { .. } => {
                self.finished = true;
                Some(self.flush_with_speed())
            }
        }
    }

    /// 已完成字节数。
    pub fn done(&self) -> u64 {
        self.done
    }

    /// 已处理条目数。
    pub fn entries_done(&self) -> u64 {
        self.entries_done
    }

    /// 未采样速度的快照（JobStarted 复位用）。
    fn snapshot_no_speed(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            done: self.done,
            total: self.total,
            speed: None,
            eta: eta_from(self.done, self.total, None),
        }
    }

    /// 窗口到期则冲刷。
    fn flush_if_due(&mut self) -> Option<ProgressSnapshot> {
        let now = Instant::now();
        if now.duration_since(self.last_flush).as_millis() as u64 >= WINDOW_MS {
            Some(self.flush_with_speed())
        } else {
            None
        }
    }

    /// 立即冲刷：以距上次冲刷的窗口增量采样 EWMA 并输出快照。
    fn flush_with_speed(&mut self) -> ProgressSnapshot {
        let now = Instant::now();
        let dt = now.duration_since(self.last_flush);
        let delta = self.done.saturating_sub(self.prev_done);
        self.prev_done = self.done;
        let speed = self.ewma.sample(delta, dt);
        self.last_flush = now;
        ProgressSnapshot {
            done: self.done,
            total: self.total,
            speed,
            eta: eta_from(self.done, self.total, speed),
        }
    }
}

#[cfg(test)]
#[path = "tests/throttle.rs"]
mod tests;
