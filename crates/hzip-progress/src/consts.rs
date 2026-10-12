//! 编译期常量（02 附录 B.2 归本 crate 的条目）。

/// GUI 侧节流频率（Hz）：`ProgressBus` 消费者之一按 10 Hz 合并展示（02 §4.1/§4.5）。
pub const PROGRESS_THROTTLE_HZ: u64 = 10;

/// EWMA 速度平滑系数 α（02 §4.5：v = 0.3*rate + 0.7*v）。
pub const SPEED_EWMA_ALPHA: f64 = 0.3;

/// 速度显示预热期（ms）：启动后该时长内不显示速度（02 §4.5，避免尖峰误导）。
pub const SPEED_WARMUP_MS: u64 = 500;
