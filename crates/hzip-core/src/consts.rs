//! 02 附录 B.2 中「定义位置 = `hzip-core::consts`」的编译期常量。
//!
//! 变更规则（B.2）：改动需在 PR 中引用出处章节。归属其他模块的 B.2 条目
//! 见各任务：`REQUEST_TIMEOUT_SECS`/`REQUEST_CHANNEL_CAP` → jobs（T30）、
//! `JOB_POOL_SIZE` → engine（T30）。

/// 配额检查点间隔（02 §9.6：解压流拷贝循环内每 4 MiB 检查一次）。
pub const QUOTA_CHECK_INTERVAL: u64 = 4 * 1024 * 1024;

/// 磁盘预检系数（02 §9.4：解压前按 1.05 系数校验可用空间余量）。
pub const SPACE_HEADROOM: f64 = 1.05;

/// glob 排除模式语法校验（CLI `--exclude` 值解析器消费；T40 追加——
/// hzip-cli 禁止直接依赖 glob crate，经 core 间接获得校验，02 §2.1 红线外）。
pub fn validate_glob(pattern: &str) -> Result<(), String> {
    glob::Pattern::new(pattern)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
