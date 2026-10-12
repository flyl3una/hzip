//! `--json` 双流输出 schema（02 §10.4；TC-CLI-003/010 反序列化锁定目标）。
//!
//! # 冻结声明（02 §13.1）
//!
//! 本模块的字段命名与结构属冻结范围：`--json` 时 stdout **只有**最终单文档
//! 结果，进度走 stderr NDJSON；schema 破坏性变更需 RFC。测试以
//! `serde_json::from_str::<ListResultJson>` 等反序列化逐字段锁定。
//!
//! # 结构
//!
//! - [`ListResultJson`]：`l` 子命令结果（TC-CLI-003）；
//! - [`TestResultJson`]：`t` 子命令结果（OK / corrupted 双变体，内部 tag）；
//! - [`JobResultJson`]：`c/x/cv` 共用（含 stats + 失败明细）；
//! - [`ProgressLineJson`]：stderr NDJSON 进度行（`ts`+`job_id` + flatten 事件）。
//!
//! 出处：02 §10.4 表与 JSON 约定、任务卡 §4.5 schema 起点。

use serde::{Deserialize, Serialize};

/// 列表结果（`hzip l --json`；TC-CLI-003）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListResultJson {
    /// 嗅探/注册格式描述（`zip`/`7z`/`tar`/…）
    pub format: String,
    /// 条目总数
    pub entry_count: u64,
    /// 条目明细（含隐式目录展开语义的平铺视图）
    pub entries: Vec<EntryJson>,
    /// 是否因结果过大截断展示（当前列表不截断——恒 false，schema 预留）
    pub truncated: bool,
}

/// 单条目 JSON（TC-CLI-003 锁定字段全）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryJson {
    /// 条目 ID（0 起连续，entries() 下标）
    pub id: usize,
    /// 条目名（`/` 分隔，已规范化）
    pub name: String,
    /// 是否目录
    pub is_dir: bool,
    /// 解压后原始字节数
    pub size_raw: u64,
    /// 打包字节数
    pub size_packed: u64,
    /// 修改时间（RFC3339 UTC；格式无该域时 null）
    pub mtime: Option<String>,
    /// 是否加密
    pub encrypted: bool,
    /// CRC32（"%08x"；格式无该域时 null）
    pub crc32: Option<String>,
    /// 文件名编码状态（"utf-8"/"suspect"/"unknown" 及检出编码）
    pub name_encoding: String,
}

/// 测试结果（`hzip t --json`；TC-CLI-005/007 退出码 1 载体）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TestResultJson {
    /// 全部条目 CRC 通过
    Ok {
        /// 档案路径
        archive: String,
        /// 通过条目数
        entries: u64,
        /// 校验字节数
        bytes: u64,
        /// CRC 通过条目数（与 entries 恒等——ok 即全通过）
        crc_ok: u64,
    },
    /// 存在损坏（退出码 1）
    Corrupted {
        /// 档案路径
        archive: String,
        /// 失败明细
        failed: Vec<FailureJson>,
    },
}

/// 单条目失败明细（t 的 corrupted / c·x·cv 的 failures 共用）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureJson {
    /// 失败条目名
    pub entry: String,
    /// HZ-xxxx 错误码数字部分
    pub code: u32,
    /// 错误文案
    pub message: String,
}

/// 压缩/解压/转换任务结果（c/x/cv 共用）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobResultJson {
    /// 子命令名（"compress"/"extract"/"convert"）
    pub command: String,
    /// CLI 退出码（0/1/2/130；与进程退出码一致）
    pub exit_code: i32,
    /// 聚合统计（预检失败/取消时为 None）
    pub stats: Option<StatsJson>,
    /// 逐条目失败明细（Partial 时非空）
    pub failures: Vec<FailureJson>,
}

/// 聚合统计（JobStats 映射）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatsJson {
    /// 已处理条目数
    pub entries: u64,
    /// 输入字节
    pub bytes_in: u64,
    /// 输出/写出字节
    pub bytes_out: u64,
    /// 任务耗时（毫秒）
    pub duration_ms: u64,
}

/// stderr NDJSON 进度行（每行一个 JSON 文档，02 §10.4「进度走 stderr NDJSON」；
/// TC-CLI-010 逐行反序列化 + 末行为 finished）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProgressLineJson {
    /// 时间戳（RFC3339 UTC）
    pub ts: String,
    /// 任务 ID
    pub job_id: u64,
    /// 事件变体（started / entry / bytes / finished；flatten 入本行）
    #[serde(flatten)]
    pub event: ProgressEventJson,
}

/// 进度事件变体（stdout 结果 JSON 之外的 stderr 侧 schema；冻结）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ProgressEventJson {
    /// 任务开始（总字节/条目）
    Started {
        /// 总字节
        total_bytes: u64,
        /// 总条目
        total_entries: u64,
    },
    /// 条目开始
    EntryStarted {
        /// 条目名
        entry: String,
        /// 声明字节
        bytes: u64,
    },
    /// 条目结束
    EntryDone {
        /// 条目名
        entry: String,
        /// 实际字节
        bytes: u64,
        /// CRC 通过
        crc_ok: bool,
    },
    /// 字节增量
    Bytes {
        /// 增量
        delta: u64,
    },
    /// 任务结束
    Finished {
        /// 终局 outcome（"done"/"partial"/"failed"/"cancelled"/"rolled_back"）
        outcome: String,
    },
}

#[cfg(test)]
#[path = "tests/json_schema.rs"]
mod tests;
