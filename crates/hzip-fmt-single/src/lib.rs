//! hzip-fmt-single：单文件压缩流格式实现（gz / bz2 / xz / zst）（02 §8.5）。
//!
//! # 定位
//!
//! 对四种**单文件流**格式提供 [`FormatHandler`]（hzip-format 契约）：读/写各恰
//! **一条** `EntryId == 0` 的条目。核心语义（02 §8.5 + T23 任务卡 §3）：
//!
//! - **条目名推导**（[`name`]）：gzip FNAME 头字段（过 `parse_entry_name`）>
//!   `OpenOptions.entry_name`（stdin 的 CLI `--entry-name`）> 档案文件名剥一层
//!   格式后缀（`a.txt.gz → a.txt`）> stdin/格式 id 兜底；
//! - **mtime 来源**（[`mtime`]）：gzip 头字段（`0` → `None`）；zstd 读端识别 hzip
//!   私有 skippable 帧（写端默认不写——字节兼容外部工具）；xz / bz2 恒 `None`；
//! - **multi-stream 串联**（[`streams`]）：多成员 gzip / bz2 / xz / zstd（含
//!   zstd skippable 帧）读取端按成员顺序拼接输出（GNU `gzip -d` 等语义）；任一
//!   成员尾校验失败 → 格式/校验错误（不静默 EOF，TC-CONF-008 红线）。
//!
//! # caps
//!
//! `read=true, write=true, append=false, edit=false, encryption=None,
//! split=false, solid=false, comment=false`。`create_writer(append=true)` 与
//! `next_entry` 二次调用 → `Err Unsupported`（HZ-5001）。
//!
//! # 与 T20 / T12 的分工
//!
//! - hzip-codec 的 deflate 是 zip 方法 8 **裸流**；gzip 容器帧（头部
//!   FNAME/mtime、尾部 CRC32+ISIZE）由本 crate 直接绑定 flate2 的
//!   `GzEncoder/GzDecoder/GzBuilder`（T20 §4.8-4 裁定）；bz2/xz/zstd 亦直接绑定
//!   对应库的**容器帧** API（本 crate 不复制 02 §6.3 级别表，一律复用
//!   `hzip_codec::level`）；
//! - 单条目语义经 `EntryInfo { id: 0 }` / `EntrySink` / `OpenOptions::entry_name`
//!   现有字段表达，未改 T20 冻结契约。
//!
//! # `size_raw` 语义（2026-09-24 修订）
//!
//! `entries()` 不读数据区（红线 1，TC-CONF-010）——但**容器元数据里写着原始大小
//! 时应当上报**（否则 core 进度层的 total 恒 0 ⇒ 解压大文件时百分比永远是 0/未知，
//! 用户实测缺陷）。故：
//!
//! | 格式 | `size_raw` | 来源 |
//! |---|---|---|
//! | gz | 尾部 `ISIZE` | 单成员 < 4 GiB 精确（`gzip -l` 同口径） |
//! | xz | 尾部索引求和 | 精确 |
//! | zst | 帧头 `Frame_Content_Size` | 写了才有（外部 zstd 默认写） |
//! | bz2 | `0`（未知） | 无该字段 |
//!
//! 未知仍为 `0`；探测只读元数据（有界 seek + 小读）——列表耗时仍与数据区大小无关。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod declared_size;
pub mod handler;
pub mod name;
pub mod reader;
pub mod streams;
pub mod writer;

mod mtime;

pub use handler::SingleHandler;

/// 编码器声明的解压后大小（core 组合链开档预检用；实现与单流 `size_raw` 同源）。
///
/// 见模块文档「原始大小探测」表：gz 尾 `ISIZE` / xz 尾索引 / zst 帧头 /
/// bz2 无字段。只读元数据（有界 seek + 小读）。
pub use declared_size::declared_payload_size;

/// bz2 单文件流 handler 工厂（feature 门控）。
#[cfg(feature = "bz2")]
pub use handler::bz2;
/// gz 单文件流 handler 工厂（feature 门控）。
#[cfg(feature = "gz")]
pub use handler::gz;
/// xz 单文件流 handler 工厂（feature 门控）。
#[cfg(feature = "xz")]
pub use handler::xz;
/// zst 单文件流 handler 工厂（feature 门控）。
#[cfg(feature = "zst")]
pub use handler::zst;

/// 错误统一入口：`io::Error → HzipError` 映射（读取/解码链）。
pub(crate) fn map_io(e: std::io::Error) -> hzip_error::HzipError {
    hzip_error::HzipError::from(e)
}
