//! hzip-format：全格式层公共契约与注册表（02 §7）。
//!
//! 模块（02 §7.1）：`types`（数据类型）/ `caps`（能力矩阵）/ `handler` /
//! `reader`/`writer`/`editor`（四契约 trait 与上下文）/ `registry`（注册表+
//! 嗅探+组合装配）/ `password`（密码承载与来源）/ `sniffer`/`consts`。
//!
//! # 契约冻结声明（02 §13.1）
//!
//! 本 crate 交付后，公开 API（四个契约 trait、上下文与支撑类型、`EntryName`
//! 构造入口、`FormatRegistry`）进入**冻结范围**：
//!
//! - 后续任务只能**新增带默认实现的 trait 方法**，不得破坏既有签名/语义/错误契约；
//! - `#[non_exhaustive]` 结构只允许加字段、枚举只允许加变体；消费方匹配必须
//!   保留 `_` 分支；
//! - 设计缺陷走 02 §13.1 RFC 流程修订文档后以追加补丁任务修复，不得并行就地改。
//!
//! # 依赖红线
//!
//! 本 crate → hzip-error / hzip-progress / hzip-codec；**禁止依赖任何 `hzip-fmt-*`**
//! （02 §2.1 红线 3，依赖倒置）。`default-formats` feature 仅为编译期标记
//! （02 §2.2 与红线 3 冲突裁定：真正的格式聚合注册在 hzip-core，见任务卡 §7-2）。
//!
//! # 四条契约红线（testkit TC-CONF-001..010 逐条验证）
//!
//! 1. `entries()` 除头部加密外不读数据区——大档案列表秒开；
//! 2. `read_entry` 任意 id 独立可用、可乱序、可重复；
//! 3. 错误密码必须返回 `Password` 错误，禁止静默产出乱码流；
//! 4. 任何输入不 panic：畸形数据 → `Format` 错误。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![forbid(unsafe_code)]

use hzip_error::HzipError;

pub mod caps;
pub mod consts;
pub mod editor;
pub mod exts;
pub mod handler;
pub mod password;
pub mod reader;
pub mod registry;
pub mod sniffer;
pub mod types;
pub mod writer;

pub use caps::{AesKey, EncryptionCaps, EncryptionRequest, FormatCaps};
pub use editor::{ArchiveEditor, Change, ChangeSet};
pub use handler::{CreateOptions, FormatHandler, OpenOptions, Signature, Sink, Source};
pub use password::{PasswordRef, PasswordSource};
pub use reader::{
    ArchiveReader, ConflictPolicy, ExtractCtx, ExtractReport, QuotaLimits, SanitizeConfig,
    Selection, SymlinkPolicy,
};
pub use registry::FormatRegistry;
pub use sniffer::{CompositeTail, Confidence, Sniffed};
pub use types::{
    Encoding, EntryAttrs, EntryId, EntryInfo, EntryMeta, NameEncoding, is_root_entry_name,
    parse_entry_name,
};
pub use writer::{ArchiveWriter, EntrySink, EntrySinkBackend, WriteCtx, WriteReport};
// EntryName / FormatId re-export（02 §7.2；承载于 hzip-error，T01 §7 约定）
pub use hzip_error::{EntryName, FormatId};

// 进度事件 re-export（writer/ctx 签名可见性；hzip-progress 为依赖）
pub use hzip_progress::{ProgressBus, ProgressEvent};

/// io::Error → HzipError 映射（嗅探/解码链错误统一入口）。
pub(crate) fn map_io(e: std::io::Error) -> HzipError {
    HzipError::from(e)
}
