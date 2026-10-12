//! hzip 命令行前端（02 §10）。
//!
//! # 命令分发总流程（02 §10.1）
//!
//! ```text
//! A hzip argv → B clap 解析（错误 → 帮助文本 + 退出 2）
//!   → C 全局初始化（tracing / 信号桥接 / i18n）
//!   → D Engine::new → E 子命令 → F 构造 JobSpec
//!   → G engine.submit → H 主线程（events 渲染进度 / requests 应答）
//!   → I wait 结果 → J 报告输出（人读 或 --json）→ K 退出码 0/1/2/130
//! ```
//!
//! # 退出码契约（02 §10.4 表 + §3.2 分类流程；冻结，§13.1）
//!
//! | 码 | 语义 |
//! |---|---|
//! | 0 | 全部成功（`Outcome::Done`） |
//! | 1 | 部分失败（`Outcome::Partial`，汇总报告） |
//! | 2 | 致命错误（clap 参数错 / HZ-5003 / Failed：格式·校验·IO·安全·配额·密码 终态 / HZ-0000 未知变体兜底） |
//! | 130 | 取消（含 Ctrl-C / SIGINT / SIGTERM / 回滚路径的 HZ-6001） |
//!
//! # `--json` 双流输出（02 §10.4；schema 冻结，§13.1）
//!
//! stdout 单文档最终结果 JSON；进度走 stderr NDJSON（每行一个
//! [`ProgressLineJson`]）。schema 由 tests/e2e.rs 反序列化锁定（TC-CLI-003/010）。
//!
//! # rustdoc 出处
//!
//! 各行为标注 02 §x.y 出处；退出码与 JSON schema 属冻结范围，破坏性变更走 RFC。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

// rust-i18n 加载 locales 目录（02 §12.2；须在 crate 根展开宏——CLI 与 GUI
// 共用文案源约定，locales/{zh-CN,en}.toml）。
rust_i18n::i18n!("locales", fallback = "zh-CN");

pub mod cli;
pub mod exit;
pub mod i18n;
pub mod json_schema;
pub mod ops;
pub mod password;
pub mod render;

pub use cli::{Cli, Cmd};
pub use exit::{exit_code_of_error, exit_code_of_result};
pub use json_schema::{
    EntryJson, FailureJson, JobResultJson, ListResultJson, ProgressEventJson, ProgressLineJson,
    StatsJson, TestResultJson,
};
pub use ops::{RunOutcome, dispatch};
