//! 人读输出渲染 + stderr NDJSON 进度流（02 §10.4 / §4.6）。
//!
//! # 双流规则
//!
//! - `--json`：stdout **只有**最终单文档结果 JSON；进度以 [`ProgressLineJson`]
//!   每行一个文档写 stderr（NDJSON）。人读通道的进度条关停。
//! - 人读模式：列表/tree/统计直接写 stdout；进度条写 stderr（不污染 stdout
//!   管道，`p` 预览二进制直出 stdout 不受影响）。
//!
//! # 进度条
//!
//! 消费 `hzip-progress` 的 10 Hz 节流器（`Throttle`）——不自建节流
//! （02 §4.3 C1 消费者定位）。显示规则（02 §4.5）：EWMA 速度、`<10 s`
//! 「即将完成」、`>24 h` 显示 `--`、启动 500 ms 不显示速度。
//!
//! 出处：02 §10.4、§4.3/§4.5、任务卡 §4.6。

use std::io::Write as _;
use std::time::Instant;

use hzip_core::services::browse::TreeNode;
use hzip_format::{EntryInfo, NameEncoding};
use hzip_progress::{Outcome, ProgressEvent, Throttle};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::json_schema::{
    EntryJson, FailureJson, JobResultJson, ProgressEventJson, ProgressLineJson, StatsJson,
};

/// 是否禁用 ANSI（`--no-color` 或 `NO_COLOR` 环境变量；当前渲染未用 ANSI，
/// 本函数供 GUI/未来主题化消费并锁定 NO_COLOR 约定，02 §4.6）。
#[allow(dead_code)]
pub(crate) fn color_disabled(no_color: bool) -> bool {
    no_color || std::env::var_os("NO_COLOR").is_some()
}

/// RFC3339 UTC 时间戳（进度行 ts；mtime 序列化同用）。
pub(crate) fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

/// 人类可读字节（进度条/报告；KiB/MiB… 二进制单位）。
pub(crate) fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0usize;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

/// 人类可读速率（B/s 自适应）。
pub(crate) fn human_rate(bps: f64) -> String {
    if !bps.is_finite() || bps < 0.0 {
        return "--".into();
    }
    if bps < 1024.0 {
        return format!("{bps:.0} B/s");
    }
    format!("{}/s", human_bytes(bps as u64))
}

/// EntryInfo → JSON 条目（02 §10.4 schema；TC-CLI-003 字段全）。
pub(crate) fn entry_json(e: &EntryInfo) -> EntryJson {
    EntryJson {
        id: e.id,
        name: e.name.to_string(),
        is_dir: e.is_dir,
        size_raw: e.size_raw,
        size_packed: e.size_packed,
        mtime: e.mtime.map(|t| t.format(&Rfc3339).unwrap_or_default()),
        encrypted: e.encrypted,
        crc32: e.crc32.map(|c| format!("{c:08x}")),
        name_encoding: name_encoding_str(&e.name_encoding),
    }
}

/// NameEncoding → 字符串（schema 稳定值）。
fn name_encoding_str(ne: &NameEncoding) -> String {
    match ne {
        NameEncoding::Utf8 => "utf-8".into(),
        NameEncoding::Suspect(enc) => format!("suspect:{enc:?}"),
        NameEncoding::Unknown => "unknown".into(),
        // #[non_exhaustive] 兜底
        _ => "unknown".into(),
    }
}

/// JobStats/Outcome → 人读统计行。
pub(crate) fn stats_line(entries: u64, bytes_in: u64, bytes_out: u64, ms: u64) -> String {
    format!(
        "条目 {entries}，输入 {}，输出 {}，耗时 {ms} ms",
        human_bytes(bytes_in),
        human_bytes(bytes_out)
    )
}

/// JobStats → StatsJson（duration 以 now 推算——JobStats.finished 由 core 置位）。
pub(crate) fn stats_json(s: &hzip_progress::JobStats) -> StatsJson {
    let started = s.started;
    let dur_ms = s
        .finished
        .map(|f| f.duration_since(started).as_millis() as u64)
        .unwrap_or_else(|| started.elapsed().as_millis() as u64);
    StatsJson {
        entries: s.entries_done,
        bytes_in: s.bytes_total,
        bytes_out: s.bytes_done,
        duration_ms: dur_ms,
    }
}

/// 由最终 Outcome 构造 JobResultJson（stats None = 预检/取消无统计）。
pub(crate) fn job_result_json(command: &str, exit_code: i32, o: &Outcome) -> JobResultJson {
    let (stats, failures) = match o {
        Outcome::Done(s) => (Some(stats_json(s)), vec![]),
        Outcome::Partial(s, fs) => (
            Some(stats_json(s)),
            fs.iter()
                .map(|f| FailureJson {
                    entry: f.entry.to_string(),
                    code: f.error.code(),
                    message: f.error.to_string(),
                })
                .collect(),
        ),
        _ => (None, vec![]),
    };
    JobResultJson {
        command: command.into(),
        exit_code,
        stats,
        failures,
    }
}

/// 人读模式单条目行（l 子命令平铺）。
pub(crate) fn entry_line(e: &EntryInfo) -> String {
    let dir = if e.is_dir { "d" } else { "-" };
    let enc = if e.encrypted { "E" } else { "-" };
    let crc = e
        .crc32
        .map(|c| format!("{c:08x}"))
        .unwrap_or_else(|| "-".into());
    format!(
        "{dir}{enc} {size:>12} {crc}  {name}",
        size = e.size_raw,
        name = e.name.as_str()
    )
}

/// tree 渲染（02 §9.10 TreeNode；`l --tree`）。
pub(crate) fn render_tree(root: &TreeNode, out: &mut String) {
    render_tree_node(root, 0, out);
}

fn render_tree_node(node: &TreeNode, depth: usize, out: &mut String) {
    if depth > 0 {
        out.push_str(&"  ".repeat(depth.saturating_sub(1)));
        out.push_str(node.name.as_str());
        if node.entry.is_none() {
            out.push('/'); // 隐式目录（无条目 id）标注
        }
        out.push('\n');
    }
    for child in node.children.values() {
        render_tree_node(child, depth + 1, out);
    }
}

/// 单行进度条（stderr；`\r` 原地刷新）。显示规则：done/total 百分比 + 速率 +
/// 剩余（02 §4.5：<10s 即将完成 / >24h `--` / 预热期不显速度）。
pub(crate) fn render_progress_line(snap: &hzip_progress::ProgressSnapshot, label: &str) -> String {
    let pct = if snap.total > 0 {
        format!("{:5.1}%", snap.done as f64 * 100.0 / snap.total as f64)
    } else {
        format!("{} 字节", snap.done)
    };
    let speed = match snap.speed {
        Some(s) => human_rate(s),
        None => "--".into(),
    };
    let eta = match &snap.eta {
        Some(hzip_progress::Eta::AlmostDone) => "即将完成".into(),
        Some(hzip_progress::Eta::Seconds(s)) => format!("{s}s"),
        None => "--".into(),
    };
    format!(
        "{label} {pct}  {}/{}  {speed}  剩余 {eta}",
        human_bytes(snap.done),
        human_bytes(snap.total)
    )
}

/// stderr NDJSON 一行（--json 模式进度；02 §10.4）。
pub(crate) fn ndjson_line(job_id: u64, ev: &ProgressEvent) -> Option<String> {
    let event = match ev {
        ProgressEvent::JobStarted {
            total_bytes,
            total_entries,
            ..
        } => ProgressEventJson::Started {
            total_bytes: *total_bytes,
            total_entries: *total_entries,
        },
        ProgressEvent::EntryStarted { entry, bytes } => ProgressEventJson::EntryStarted {
            entry: entry.to_string(),
            bytes: *bytes,
        },
        ProgressEvent::EntryDone {
            entry,
            bytes,
            crc_ok,
        } => ProgressEventJson::EntryDone {
            entry: entry.to_string(),
            bytes: *bytes,
            crc_ok: *crc_ok,
        },
        ProgressEvent::Bytes { delta } => ProgressEventJson::Bytes { delta: *delta },
        ProgressEvent::JobFinished { outcome, .. } => ProgressEventJson::Finished {
            outcome: outcome_tag(outcome),
        },
        // #[non_exhaustive] 兜底：未知事件不入 NDJSON
        _ => return None,
    };
    let line = ProgressLineJson {
        ts: now_rfc3339(),
        job_id,
        event,
    };
    serde_json::to_string(&line).ok()
}

fn outcome_tag(o: &Outcome) -> String {
    match o {
        Outcome::Done(_) => "done".into(),
        Outcome::Partial(..) => "partial".into(),
        Outcome::Failed(_) => "failed".into(),
        Outcome::Cancelled => "cancelled".into(),
        Outcome::RolledBack(_) => "rolled_back".into(),
        _ => "unknown".into(),
    }
}

/// 进度条渲染器（人读模式 stderr；json 模式停用）。
pub(crate) struct ProgressPrinter {
    json: bool,
    th: Throttle,
    label: String,
    last: Instant,
}

impl ProgressPrinter {
    pub(crate) fn new(json: bool, label: String) -> Self {
        Self {
            json,
            th: Throttle::new(),
            label,
            last: Instant::now(),
        }
    }

    /// 吞事件；需要刷新时写 stderr。
    pub(crate) fn push(&mut self, ev: &ProgressEvent) {
        if self.json {
            // --json：进度全走 NDJSON（不画条）
            if let Some(line) = ndjson_line(0, ev) {
                let mut err = std::io::stderr().lock();
                let _ = writeln!(err, "{line}");
            }
            return;
        }
        if let Some(snap) = self.th.push(ev) {
            // 距上次 >200ms 才画（10 Hz 节流器已窗口化；边界事件立即）
            let now = Instant::now();
            if now.duration_since(self.last).as_millis() >= 200
                || matches!(ev, ProgressEvent::JobFinished { .. })
            {
                let mut err = std::io::stderr().lock();
                let _ = write!(err, "\r{}", render_progress_line(&snap, &self.label));
                let _ = err.flush();
                self.last = now;
            }
        }
        if matches!(ev, ProgressEvent::JobFinished { .. }) {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err);
        }
    }
}

#[cfg(test)]
#[path = "tests/render.rs"]
mod tests;
