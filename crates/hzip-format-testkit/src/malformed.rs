//! 畸形样本生成与 panic 计数（TC-CONF-008 红线 4；02 §8.6）。
//!
//! 位翻转用固定种子 xorshift（免 rand 依赖，可复现）；`PanicCounter` 以
//! `catch_unwind` 包裹被测操作并计数——TC-CONF-008 断言「计数为 0」。

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 固定种子 xorshift64* 位翻转（可复现，TC-CONF-008 复现性断言）。
pub fn bit_flip(buf: &mut [u8], count: usize, seed: u64) {
    if buf.is_empty() || count == 0 {
        return;
    }
    let mut state = seed.max(1);
    for _ in 0..count {
        // xorshift64*
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let pos = (state as usize) % buf.len();
        let bit = 1u8 << ((state >> 32) as u8 % 8);
        buf[pos] ^= bit;
    }
}

/// 截断辅助：按比例截断（返回截断后的副本语义由调用方切片实现）。
pub fn truncate_ratio(buf: &[u8], ratio_percent: usize) -> &[u8] {
    let cut = buf.len() * ratio_percent / 100;
    &buf[..buf.len() - cut]
}

/// panic 计数 + 包裹（TC-CONF-008：畸形输入下 catch_unwind 计数为 0）。
///
/// 实现要点：① 安静 panic hook（抑制 stderr 噪声，测后恢复）；② `AssertUnwindSafe`；
/// ③ panic 转为调用方可见的计数递增（不静默吞掉——由 `guard` 返回 `Err` 传播）。
#[derive(Debug, Default)]
pub struct PanicCounter {
    count: AtomicUsize,
}

impl PanicCounter {
    /// 空计数器。
    pub fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
        }
    }

    /// 当前 panic 次数。
    pub fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    /// 包裹一个被测操作：正常返回 `Ok`；panic → 计数 +1 并返回 `Err`（内容为
    /// panic 消息）——测试可断言「计数为 0」（畸形输入不得 panic）。
    pub fn guard<R>(&self, op: impl FnOnce() -> Result<R, HzipError>) -> Result<R, HzipError> {
        // 安静 hook：抑制被测 panic 的 stderr 噪声（测后恢复）
        let prev = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));
        let result = panic::catch_unwind(AssertUnwindSafe(op));
        panic::set_hook(prev);
        match result {
            Ok(r) => r,
            Err(payload) => {
                self.count.fetch_add(1, Ordering::SeqCst);
                let msg = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .unwrap_or("unknown panic");
                Err(HzipError::Unsupported {
                    what: format!("panic: {msg}").into(),
                })
            }
        }
    }
}

use hzip_error::HzipError;

#[cfg(test)]
#[path = "tests/malformed.rs"]
mod tests;
