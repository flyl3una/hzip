//! `ErrorContext` 链式构造辅助（02 §3.1）。
//!
//! 用于在错误传播路径上附加失败上下文：`path` 仅 [`HzipError::Io`] 变体有字段
//! （02 §3.3），`entry` 供 core 聚合 `Partial` 明细用（02 §3.5）。

use std::path::PathBuf;

use crate::{EntryName, HzipError};

/// 错误上下文链式构造器（02 §3.1：`.with_path()`、`.with_entry()`）。
///
/// # Examples
///
/// ```
/// use hzip_error::{context::ErrorContext, HzipError};
///
/// let err = HzipError::from(std::io::Error::other("write failed"));
/// let err = ErrorContext::new().with_path("out/a.zip").attach(err);
/// match err {
///     HzipError::Io { path, .. } => assert!(path.is_some()),
///     _ => panic!("variant must be preserved"),
/// }
/// ```
#[derive(Debug, Default, Clone)]
pub struct ErrorContext {
    path: Option<PathBuf>,
    entry: Option<EntryName>,
}

impl ErrorContext {
    /// 空上下文。
    pub fn new() -> Self {
        Self::default()
    }

    /// 附加路径上下文（仅对 `Io` 变体生效，见 [`Self::attach`]）。
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// 附加条目名上下文（core 聚合 `Partial` 明细用，02 §3.5）。
    pub fn with_entry(mut self, entry: impl Into<EntryName>) -> Self {
        self.entry = Some(entry.into());
        self
    }

    /// 取回挂载的条目名（core 聚合 `Partial` 明细用，02 §3.5）。
    pub fn entry(&self) -> Option<&EntryName> {
        self.entry.as_ref()
    }

    /// 注入上下文：`path` 仅 `Io` 变体有字段（02 §3.3）；其余变体原样返回。
    pub fn attach(self, err: HzipError) -> HzipError {
        match (err, self.path) {
            (HzipError::Io { source, .. }, path) => HzipError::Io { source, path },
            (e, _) => e,
        }
    }
}
