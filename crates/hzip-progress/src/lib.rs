//! hzip-progress：进度事件总线与协作式取消（02 §4）。
//!
//! 两个基础设施（02 §4.1）：
//!
//! - **取消**：[`token::CancellationToken`] 以「原子标志 + 代际计数 + 父链」实现
//!   幂等取消与父子级联（02 §4.2）。`is_cancelled()` 单条 relaxed load，满足
//!   格式层 `ctx.checkpoint()?` < 5 ns 的开销要求。
//! - **进度**：[`event::ProgressEvent`] / [`event::Outcome`] 类型、
//!   [`bus::ProgressBus`] 多订阅者广播（CLI stderr NDJSON、GUI 10 Hz 节流、
//!   core 状态迁移三个消费者，02 §4.3）、GUI 侧 [`throttle::Throttle`]
//!   10 Hz 合并器与 EWMA 速度计算（02 §4.5）。
//!
//! 依赖红线（02 §2.1）：本 crate 仅依赖 `hzip-error` 与 crossbeam；自身是纯
//! Rust 基础库，无平台差异。
//!
//! # 承载类型占位
//!
//! 02 §7.2 的 `EntryName` 正式归属 hzip-format（T20 交付）；本 crate 先以
//! [`event::EntryName`]（`SmolStr` 别名）占位，T20 冻结前统一 re-export 对齐。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod bus;
pub mod consts;
pub mod event;
pub mod throttle;
pub mod token;

pub use bus::{EventReceiver, ProgressBus, RecvTimeout};
pub use event::{EntryFailure, EntryName, JobId, JobStats, Outcome, ProgressEvent};
pub use throttle::{Eta, ProgressSnapshot, SpeedEwma, Throttle};
pub use token::CancellationToken;
