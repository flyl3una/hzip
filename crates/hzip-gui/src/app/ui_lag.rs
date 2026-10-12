//! UI 线程滞后看门狗（2026-09-24 UI 线程阻塞审计的**测量面**）。
//!
//! # 为什么需要
//!
//! 「主界面卡了一会儿无响应」这类报障最难的地方不是修，而是**定位是哪一段同步
//! 代码吃掉了时间**：UI 线程上跑的东西五花八门（渲染、泵批处理、事件回调），
//! 静态看代码只能给出「可能慢」的清单，说不出是哪一条。
//!
//! 故给「不该久的同步块」套一个 RAII 计时器：超过阈值就留一条 **warn**（带块名
//! 与耗时，落 `hzip-YYYY-MM-DD.log`）。这样再现时日志会直接点名，而不是靠猜。
//!
//! # 用法
//!
//! ```ignore
//! let _lag = UiLag::new("泵批处理");
//! // …同步重活…
//! // 出作用域时自动判定（含 early return / `?` / panic 展开）
//! ```
//!
//! 阈值取用户口径「>0.5 s 必须可感知」的一半：`WARN_MS` 150 ms 起报（长帧），
//! `ERROR_MS` 500 ms 升级为 error（已经能被人眼看见的卡顿）。

use std::time::Instant;

/// 长帧阈值（warn）。
pub const WARN_MS: u128 = 150;
/// 卡顿阈值（error）。
pub const ERROR_MS: u128 = 500;

/// 计时守卫（`Drop` 时按耗时留痕）。
pub struct UiLag {
    /// 块名（日志里点名用；稳定、可 grep）
    name: &'static str,
    start: Instant,
}

impl UiLag {
    /// 开始计时。
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            start: Instant::now(),
        }
    }
}

impl Drop for UiLag {
    fn drop(&mut self) {
        let ms = self.start.elapsed();
        // 阈值用**毫秒整数**比较，避免浮点与 Duration 比较的噪音
        let millis = ms.as_millis();
        if millis >= ERROR_MS {
            tracing::error!(
                block = self.name,
                ms = millis,
                "UI 线程卡顿（>= {ERROR_MS} ms）：该同步块必须挪后台或加闸"
            );
        } else if millis >= WARN_MS {
            tracing::warn!(
                block = self.name,
                ms = millis,
                "UI 线程长帧（>= {WARN_MS} ms）"
            );
        }
    }
}

#[cfg(test)]
#[path = "tests/ui_lag.rs"]
mod tests;
