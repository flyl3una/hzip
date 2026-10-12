//! 任务管理常量（02 附录 B.2 归属 jobs 的条目）。
//!
//! 变更规则（B.2）：改动需在 PR 中引用出处章节。

/// 请求-应答决策超时（02 §9.11 / 附录 B.2：`REQUEST_TIMEOUT_SECS = 120`）。
///
/// worker 在 `WaitingInput` 态阻塞于应答通道 `recv_timeout`；超时 → 任务
/// `Failed` + `Err 5002`（决策超时，以 `HzipError::Unsupported{what: "hz-5002 …"}`
/// 字符串带码承载——T30 §7.2 裁定，T40 消费时按码出文案）。
pub const REQUEST_TIMEOUT_SECS: u64 = 120;

/// 请求通道容量（02 §9.11 / 附录 B.2：`REQUEST_CHANNEL_CAP = 1`）。
///
/// 同时最多一个待决问题——worker 发出请求后阻塞等应答，天然不产生第二个
/// 并发请求；容量 1 保证 UI 侧未取走时 `need()` 不丢（send 短暂阻塞合法）。
pub const REQUEST_CHANNEL_CAP: usize = 1;
