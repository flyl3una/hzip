//! hzip-format-testkit：格式一致性测试套件（02 §8.6；dev crate）。
//!
//! 对任意 [`FormatHandler`] 自动展开 10 组一致性用例（TC-CONF-001..010，
//! 03 §2.1），验证 02 §7.3 的**四条契约红线**：
//!
//! 1. `entries()` 不读数据区（列表秒开，TC-CONF-010）；
//! 2. `read_entry` 独立/乱序/可重复（TC-CONF-003/004）；
//! 3. 错误密码返回 `Password`（禁乱码流，TC-CONF-007）；
//! 4. 任何输入不 panic（TC-CONF-008，catch_unwind 计数 0）。
//!
//! 同时提供 [`mock::MockHandler`]（可注入 panic/慢速/指定错误/受控进度）供
//! core 测试消费（T30：TC-CORE-005~014）。
//!
//! 接入方式：各 `hzip-fmt-*` crate 以 dev-dependency 引用本 crate，在自身
//! `tests/conformance.rs` 中数行注册（见 [`kit::run_conformance`]）。
//!
//! # 定位与红线
//!
//! dev crate：不进任何发行二进制依赖树；自身只依赖 `hzip-format`
//! （02 §2.1：TK --> FMTA 合法边），**不依赖任何 hzip-fmt-\***。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod cases;
pub mod cpio;
pub mod filter;
pub mod kit;
pub mod malformed;
pub mod mock;
pub mod stub;
/// 卷镜像夹具（磁盘镜像 → 分区 → 文件系统；计划 `docs/tasks/T-IMG/`）。
///
/// `vol` feature 门控：默认关闭，只有卷相关 crate 在 dev-dependencies 里开
/// `features = ["vol"]` 才编译 ⇒ 既有格式的测试构建不受影响。
#[cfg(feature = "vol")]
pub mod vol;

pub use filter::{ArchiveSource, CapsFilter, ConfCaseId, RunDecision};
pub use kit::{
    ConformanceSummary, DynOutcome, DynTest, DynTestList, KitCtx, run_cases, run_conformance,
};
pub use malformed::PanicCounter;
pub use mock::{Hook, MockEntry, MockHandler, Scripted};
