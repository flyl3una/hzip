//! 配额检查点（02 §9.6 代码块；四类判定顺序 4501 → 4502 → 4503 → 4504）。
//!
//! 默认值（02 §9.6 / 附录 B.1）：`max_file=32 GiB`、`max_total=128 GiB`、
//! `max_ratio=200`、`max_entries=1_000_000`（`hzip_format::QuotaLimits::default`）。
//!
//! 判定顺序即代码顺序；4504（条目数）在 `entry_start` 处检查（不等 4 MiB 粒度，
//! 条目数炸弹在计数即拦截，TC-SEC-008）。4501/4503 按条目尺度（每条目开始重置），
//! 4502 按任务累计（不重置）。

use hzip_error::{HzipError, QuotaKind};
use hzip_format::QuotaLimits;

use crate::consts::QUOTA_CHECK_INTERVAL;

/// 配额运行时状态（检查点间累计）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaState {
    /// 当前条目累计解压字节
    pub raw_done: u64,
    /// 任务累计解压字节（不重置）
    pub total_done: u64,
    /// 当前条目累计读入压缩字节
    pub packed_read: u64,
    /// 已见条目数
    pub entries_seen: u64,
    /// 距上次 4 MiB 检查的累计解压字节
    pub accrual: u64,
}

impl QuotaState {
    /// 空状态。
    pub fn new() -> Self {
        Self::default()
    }
}

/// 配额守卫（T32 流拷贝循环接线）。
#[derive(Debug, Clone)]
pub struct QuotaGuard {
    limits: QuotaLimits,
    state: QuotaState,
}

impl QuotaGuard {
    /// 新建（限额来自 `ExtractCtx.quota`——B.1 配置链注入）。
    pub fn new(limits: QuotaLimits) -> Self {
        Self {
            limits,
            state: QuotaState::new(),
        }
    }

    /// 当前状态（测试/报告用）。
    pub fn state(&self) -> &QuotaState {
        &self.state
    }

    /// 当前状态可变引用（T32 报告进度）。
    pub fn state_mut(&mut self) -> &mut QuotaState {
        &mut self.state
    }

    /// 流拷贝循环内调用（T32 接线）：`raw_delta`/`packed_delta` 为 chunk 增量。
    ///
    /// 增量**立即入账**（raw_done/total_done/packed_read）；`accrual` 累计解压字节，
    /// 达 4 MiB 粒度（`QUOTA_CHECK_INTERVAL`）才执行一次比较判定。
    /// 判定顺序：4501 → 4502 → 4503（→ 4504 归 `entry_start`）。
    pub fn checkpoint(&mut self, raw_delta: u64, packed_delta: u64) -> Result<(), HzipError> {
        self.state.raw_done = self.state.raw_done.saturating_add(raw_delta);
        self.state.total_done = self.state.total_done.saturating_add(raw_delta);
        self.state.packed_read = self.state.packed_read.saturating_add(packed_delta);
        self.state.accrual = self.state.accrual.saturating_add(raw_delta);
        if self.state.accrual < QUOTA_CHECK_INTERVAL {
            return Ok(());
        }
        self.state.accrual = 0;
        self.check_order()
    }

    /// 强制检查（供测试绕过粒度门控 / T32 条目收尾补检）。
    pub fn check_now(&mut self) -> Result<(), HzipError> {
        self.state.accrual = 0;
        self.check_order()
    }

    /// 条目开始处调用（4504 不等 4 MiB；重置条目尺度 4501/4503 计数）。
    pub fn entry_start(&mut self) -> Result<(), HzipError> {
        self.state.entries_seen = self.state.entries_seen.saturating_add(1);
        self.state.raw_done = 0;
        self.state.packed_read = 0;
        self.state.accrual = 0;
        if self.state.entries_seen > self.limits.max_entries {
            return Err(HzipError::Quota {
                quota: QuotaKind::MaxEntries,
            });
        }
        Ok(())
    }

    /// 四类顺序判定主体（4501 → 4502 → 4503）。
    fn check_order(&self) -> Result<(), HzipError> {
        // ① HZ-4501：单文件超限
        if self.state.raw_done > self.limits.max_file_bytes {
            return Err(HzipError::Quota {
                quota: QuotaKind::MaxFileBytes,
            });
        }
        // ② HZ-4502：总量超限
        if self.state.total_done > self.limits.max_total_bytes {
            return Err(HzipError::Quota {
                quota: QuotaKind::MaxTotalBytes,
            });
        }
        // ③ HZ-4503：比率超限（packed=0 且 raw>0 视为 ∞ 触发）
        let packed = self.state.packed_read;
        if (packed == 0 && self.state.raw_done > 0)
            || self.state.raw_done / packed.max(1) > self.limits.max_ratio as u64
        {
            return Err(HzipError::Quota {
                quota: QuotaKind::MaxRatio,
            });
        }
        Ok(())
    }
}
