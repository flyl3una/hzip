//! 退出码归并逻辑（02 §10.4 表 + §3.2 分类流程）。
//!
//! | Outcome / 错误来源 | 退出码 |
//! |---|---|
//! | `Outcome::Done(_)`（全成功） | 0 |
//! | `Outcome::Partial(..)`（部分失败，汇总报告） | 1 |
//! | `Outcome::Failed(e)`：格式/校验/IO/安全/配额/密码 终态 | 2 |
//! | `Outcome::Failed(HzipError::Cancelled)`（HZ-6001 回滚路径取消） | 130 |
//! | `Outcome::Cancelled` / `Outcome::RolledBack(_)` | 130 |
//! | `#[non_exhaustive]` 未知变体兜底 HZ-0000 | 2 |
//!
//! clap 解析失败由 clap 自身退出 2（帮助文本 + 错误，02 §10.1 B 节点）；
//! `--password-env` 变量缺失在提交前报 HZ-5003 即时退出 2。
//!
//! 冻结：退出码契约属 02 §13.1 冻结范围。

use hzip_error::HzipError;
use hzip_progress::Outcome;

/// `JobResult` → 进程退出码（02 §10.4 表逐行）。
pub fn exit_code_of_result(r: &Result<Outcome, HzipError>) -> i32 {
    match r {
        Ok(Outcome::Done(_)) => 0,
        Ok(Outcome::Partial(..)) => 1,
        Ok(Outcome::Cancelled) => 130,
        Ok(Outcome::RolledBack(_)) => 130,
        Ok(Outcome::Failed(e)) => exit_code_of_error(e),
        // #[non_exhaustive]：core Outcome 只增变体——未知终局按失败兜底
        Ok(_) => 2,
        Err(e) => exit_code_of_error(e),
    }
}

/// 错误 → 退出码（§3.2 分类流程 C1~C5 + 未知变体 HZ-0000 兜底）。
pub fn exit_code_of_error(e: &HzipError) -> i32 {
    match e {
        // C1：用户主动取消（HZ-6001，含回滚路径）→ 130
        HzipError::Cancelled => 130,
        // 其余（格式/校验/IO/安全/配额/密码/业务）→ 2
        _ => 2,
    }
}

#[cfg(test)]
#[path = "tests/exit.rs"]
mod tests;
