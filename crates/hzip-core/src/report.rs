//! 报告聚合层（02 §9.1 services/report 职责行；T33 独占）。
//!
//! 把 fmt 层产出的 [`ExtractReport`] / [`WriteReport`] 聚合为 [`JobStats`]
//! （T10 载体，字段非冻结）+ [`EditDiff`]（02 §9.9 新旧对比报告）。
//!
//! # 收敛语义（02 §3.5 二态）
//!
//! - `failures` 空 → `Outcome::Done(stats)`；
//! - `failures` 非空 → `Outcome::Partial(stats, failures)`——明细携带真实
//!   条目名（[`hzip_progress::EntryFailure`]）。
//!
//! 本层只做**纯函数聚合**（无 IO、无 ctx）；写向 side 的部分失败收敛在
//! `services::compress/convert` 内直接以本层结构装配（02 §9.1 report 行）。
//!
//! 出处：02 §9.1 report、§3.5、§9.9 尾注（新旧大小/条目数对比）。

use std::time::Instant;

use hzip_format::{EntryInfo, ExtractReport, WriteReport};
use hzip_progress::{EntryFailure, JobStats, Outcome};

/// 编辑前后对比（02 §9.9 `JobFinished` 报告内容）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EditDiff {
    /// 编辑前条目大小
    pub old_size: u64,
    /// 编辑后条目大小
    pub new_size: u64,
    /// 编辑前档案条目数
    pub old_entries: usize,
    /// 编辑后档案条目数
    pub new_entries: usize,
}

/// ExtractReport → JobStats（entries_done/bytes_written 映射）。
///
/// T50 golden 黑盒断言消费面；本任务 extract 侧聚合经 T32
/// `outcome_of_report`（extract.rs 内私有）——此通用入口预留，标注跟踪。
#[allow(dead_code)] // T50 黑盒消费面
pub(crate) fn stats_from_extract(r: &ExtractReport) -> JobStats {
    let mut s = JobStats::new(r.bytes_written, r.entries_done as u64);
    s.entries_done = r.entries_done as u64;
    s.bytes_done = r.bytes_written;
    s
}

/// WriteReport → JobStats（entries_written/bytes_in/bytes_out；progress total
/// 语义按写向：total_bytes = 输入字节，done = 已写字节）。
pub(crate) fn stats_from_write(r: &WriteReport) -> JobStats {
    let mut s = JobStats::new(r.bytes_in, r.entries_written as u64);
    s.entries_done = r.entries_written as u64;
    s.bytes_done = r.bytes_out;
    s
}

/// ExtractReport → Outcome（02 §3.5 二态收敛；条目名经 `entries` 表回填）。
#[allow(dead_code)] // 预留：解压向统一收敛入口（T32 outcome_of_report 为私有等价）
pub(crate) fn outcome_of_extract(report: ExtractReport, entries: &[EntryInfo]) -> Outcome {
    let stats = stats_from_extract(&report);
    if report.failures.is_empty() {
        Outcome::Done(stats)
    } else {
        Outcome::Partial(stats, failures_of(&report.failures, entries))
    }
}

/// WriteReport → Outcome（无失败明细——写向失败是 fail-fast/IO 错误路径，
/// 由调用方以 `Err` 收敛；本函数恒 Done）。
pub(crate) fn outcome_of_write(report: WriteReport) -> Outcome {
    let mut stats = stats_from_write(&report);
    stats.finished = Some(Instant::now());
    Outcome::Done(stats)
}

/// 回填统计时间戳（服务层终局收敛用）：stats 由报告聚合时新建，`started`
/// 落于聚合时刻 → CLI/GUI 耗时恒 0 ms；调用方捕获任务实际开始时刻后回填。
pub(crate) fn stamp_stats(s: &mut JobStats, started: Instant) {
    s.started = started;
    s.finished = Some(Instant::now());
}

/// fmt 失败明细（`(EntryId, Box<HzipError>)`）→ core `EntryFailure` 列表。
#[allow(dead_code)] // outcome_of_extract 消费面
fn failures_of(
    failures: &[(usize, Box<hzip_error::HzipError>)],
    entries: &[EntryInfo],
) -> Vec<EntryFailure> {
    failures
        .iter()
        .filter_map(|(id, e)| {
            entries.get(*id).map(|info| EntryFailure {
                entry: info.name.as_str().into(),
                error: e.clone(),
            })
        })
        .collect()
}

/// 编辑前后 diff（编辑目标条目旧信息 + 重写报告）。
pub(crate) fn edit_diff(before: &EntryInfo, after: &WriteReport) -> EditDiff {
    EditDiff {
        old_size: before.size_raw,
        new_size: after.bytes_in,
        old_entries: 1,
        new_entries: after.entries_written,
    }
}
