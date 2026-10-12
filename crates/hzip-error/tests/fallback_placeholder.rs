//! 占位测试（任务卡 §6 用例 15 / 05 §1.2 占位纪律）：
//! 未知变体 HZ-0000 兜底的 `_` 分支位于 crate 外（02 §3.5），
//! 待 T40 CLI 实现后联测，禁止静默砍功能。

#![allow(clippy::unwrap_used, clippy::expect_used)]

/// HZ-0000 兜底（CLI/GUI 对 `#[non_exhaustive]` 未知变体的 `_` 分支归一化为
/// HZ-0000 + `UserAction::ReportIssue` + 退出码 2）由消费方（T40 CLI）实现后联测（02 §3.5）。
#[test]
#[ignore = "HZ-0000 兜底由消费方（T40 CLI）实现后联测（02 §3.5）"]
#[allow(clippy::todo)]
fn non_exhaustive_fallback_placeholder() {
    todo!("T40 CLI 落地 err.rs 的 `_` 兜底分支后，联测 HZ-0000 + ReportIssue + 退出码 2")
}
