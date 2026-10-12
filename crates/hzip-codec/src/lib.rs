//! hzip-codec 编解码层（02 §6）。
//!
//! 本 crate 提供统一的编解码抽象与各第三方压缩库封装：
//!
//! - [`traits`]：`CodecProvider` / `Encoder` / `Decoder` 三 trait 与 `CodecId`（02 §6.5，API 冻结）；
//! - [`level`]：统一 `Level` → 各库参数映射表（02 §6.3，表驱动）；
//! - [`deflate`] / [`bzip2`] / [`xz`] / [`zstd`]：第三方库封装（feature 门控）；
//! - [`filter`]：BCJ / BCJ2 可执行文件分支过滤器（feature `bcj2`；解码方向，编码归 T46）；
//! - [`crc`]：crc32 流式增量（02 §6.2 旁路累计）；
//! - [`store`]：`id()=="store"` 的恒等 provider（02 §6.3「—」列路由语义）。
//!
//! 依赖红线（02 §2.1）：本 crate 仅依赖 `hzip-error` 与各压缩库；不 import
//! hzip-format / hzip-progress。C 库（liblzma / libbz2 / libzstd）边界错误一律以
//! `io::Error` 报出，不 panic 穿越（任务卡 §7.4）。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![forbid(unsafe_code)]

pub mod consts;
pub mod crc;
pub mod level;
pub mod traits;

mod counting;
pub mod store;

#[cfg(feature = "bzip2")]
pub mod bzip2;
#[cfg(feature = "deflate")]
pub mod deflate;
#[cfg(feature = "bcj2")]
pub mod filter;
#[cfg(feature = "xz")]
pub mod xz;
#[cfg(feature = "zstd")]
pub mod zstd;

#[cfg(test)]
mod tests;
