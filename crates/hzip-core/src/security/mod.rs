//! 安全决策核心（02 §9.1 services/security 职责行；02 §9.6）。
//!
//! 模块职责（T31）：
//!
//! - [`sanitize`]：`sanitize_entry_path` 净化算法（红线函数）——绝不允许任何
//!   条目名把解压写到 `dest` 之外；
//! - [`symlink`]：符号链接策略表三行（02 §9.6 表）+ 已建链接台账的
//!   「后续条目落入链接之下」组合攻击检测；
//! - [`quota`]：`QuotaGuard` 配额检查点（4 MiB 粒度，四类判定顺序
//!   4501 → 4502 → 4503 → 4504）。
//!
//! 语义上界：本模块只产出「拒绝（`Security`/`Quota` 错误）或净化后的目标路径」。
//! 配额的流拷贝调用点、符号链接实际创建由 T32 解压管道接线（02 §9.4 G 节点）。
//!
//! 依赖红线：仅消费 hzip-format 冻结类型（`EntryName`/`SanitizeConfig`/
//! `QuotaLimits`/`SymlinkPolicy`）+ hzip-platform（`long_path`）+ hzip-error。
//! 不 import 任何 fmt crate 与 codec（02 §2.1 红线 3）。

pub mod quota;
pub mod sanitize;
pub mod symlink;

pub use quota::{QuotaGuard, QuotaState};
pub use sanitize::{SanitizedEntry, TruncateWarning, sanitize_entry_path};
pub use symlink::{LinkAction, SymlinkGuard};

#[cfg(test)]
#[path = "tests/props.rs"]
mod props;
#[cfg(test)]
mod tests;
