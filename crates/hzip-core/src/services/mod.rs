//! services：JobKind 业务分派（02 §9.1 services 职责行）。
//!
//! T30 建分派骨架；T32 替换 Extract/Test 臂为真实现（`extract.rs`）；
//! T33 替换 Compress/Convert/EditSave 臂（`compress.rs`/`convert.rs`/`edit.rs`）；
//! browse/preview **非 JobKind**（02 §9.10 同步轻量，不占池）——归 T34。
//!
//! **本文件只做模块声明与再导出**（T-STD 2026-10-08 用户口径）——分派实现见
//! [`dispatch`]。
//!
//! ```text
//! | JobKind | 实现文件 |
//! |---|---|
//! | Extract / Test | `services/extract.rs`（T32） |
//! | Compress / Convert / EditSave | `services/compress.rs` / `convert.rs` / `edit.rs`（T33） |
//! ```

pub mod browse;
pub mod compress;
pub mod convert;
pub mod edit;
pub mod extract;
pub mod preview;
pub mod space;
pub mod walk;

mod dispatch;

#[cfg(test)]
mod tests;

pub(crate) use dispatch::execute;
