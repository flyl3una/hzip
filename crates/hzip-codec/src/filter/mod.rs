//! 可执行文件分支过滤器（02 §6.4）：BCJ（x86 单流）与 BCJ2（四流）**解码器**。
//!
//! 编码器（`bcj2_encode.rs`）归 T46 独占，本目录不创建（任务卡 §2/§4.9）。
//! 解码器 `Bcj2Decoder::new` 的流参数语义即四流对称验证的基准（7-Zip 容器子流序
//! MAIN/CALL/JUMP/RC，见 [`bcj2`] 文档），T46 编码器须与之对称。
//!
//! **本文件只做模块声明与再导出**（T-STD 2026-10-08 用户口径）——id 常量与
//! 「已交付清单」在 [`ids`]。

pub mod bcj;
pub mod bcj2;
pub mod bcj2_encode;

mod ids;

pub use bcj::BcjDecoder;
pub use bcj2::Bcj2Decoder;
pub use bcj2_encode::{Bcj2Encoder, Bcj2Stats};
pub use ids::{FILTER_BCJ, FILTER_BCJ2, supported_ids};
