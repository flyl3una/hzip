//! hzip 任务编排核心（02 §9）。
//!
//! 模块演进：
//! - T30：`engine`/`jobs`（Engine/JobManager/JobSpec 骨架分派）；
//! - T31：`security`（sanitize/符号链接/配额）+ `consts`（附录 B.2 core 常量）；
//! - T32：`services::extract`（解压管道 §9.4）+ `smart_extract`（§9.7）+
//!   `conflict`（§9.8）+ `encoding`（§8.1 core 半场）。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![forbid(unsafe_code)]

pub mod conflict;
pub mod consts;
pub mod encoding;
pub mod engine;
pub mod jobs;
pub mod report;
pub mod security;
pub mod services;
pub mod smart_extract;

// 统一压缩级别/编码器方法经本 crate 再导出供 CLI/GUI 消费（02 §2.1 红线 2：
// CLI/GUI 禁止直接依赖 hzip-codec——级别参数与编码器标识一律经 core 间接获得。
// T40 追加 `Level`；T-PKG 追加 GUI 侧用到的 `level`/`CodecMethod`/`consts`）。
pub use hzip_codec::level::Level;
// 注意：core 自己已有 `pub mod consts`（配额/头寸常量），故 codec 的同名模块必须改名再导出。
pub use hzip_codec::{consts as codec_consts, level, traits::CodecMethod};
