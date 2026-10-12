//! cpio 集成测试公共夹具（`tests/*.rs` 各自 `mod support;` 引入）。
//!
//! 只放与「被测契约」无关的脚手架：临时目录、`ExtractCtx` 最小环境、密码源替身、
//! 读全流助手。档案素材一律来自 `hzip_format_testkit::cpio`（四变体字节流）。

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io::Read;
use std::path::{Path, PathBuf};

use hzip_format::{ConflictPolicy, ExtractCtx, ProgressBus, QuotaLimits, SanitizeConfig};

/// 密码源替身：cpio 无加密概念，恒 `None`。
#[derive(Default)]
pub struct NoPasswordSource;

impl hzip_format::PasswordSource for NoPasswordSource {
    fn password(
        &self,
        _entry: Option<&hzip_format::EntryName>,
    ) -> Option<zeroize::Zeroizing<String>> {
        None
    }
}

/// `ExtractCtx` 借用组件集（必须先于 ctx 构造，故独立成 struct）。
pub struct Env {
    pub cancel: hzip_progress::CancellationToken,
    pub bus: ProgressBus,
    pw: NoPasswordSource,
    sanitize: SanitizeConfig,
    quota: QuotaLimits,
}

impl Default for Env {
    fn default() -> Self {
        Self {
            cancel: Default::default(),
            bus: ProgressBus::default(),
            pw: NoPasswordSource,
            sanitize: SanitizeConfig::default(),
            quota: QuotaLimits::default(),
        }
    }
}

impl Env {
    pub fn new() -> Self {
        Self::default()
    }

    /// 解压上下文（`best_effort = false`，冲突策略 overwrite）。
    pub fn ctx(&self) -> ExtractCtx<'_> {
        ExtractCtx {
            cancel: &self.cancel,
            progress: &self.bus,
            passwords: &self.pw,
            conflicts: ConflictPolicy::Overwrite,
            sanitize: &self.sanitize,
            quota: &self.quota,
            best_effort: false,
            codepage: None,
        }
    }
}

/// 新建临时目录（带唯一后缀；调用方自行清理或依赖 OS 回收）。
pub fn tmp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "hzip-cpio-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).expect("建临时目录");
    p
}

/// 把档案字节落盘（返回路径）。
pub fn write_fixture(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("落盘档案");
    p
}

/// 读全一个流。
pub fn read_all(mut r: Box<dyn Read + Send>) -> Vec<u8> {
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).expect("读流");
    buf
}

/// 条目名列表（顺序即 `entries()` 顺序）。
pub fn names(r: &dyn hzip_format::ArchiveReader) -> Vec<String> {
    r.entries()
        .expect("entries")
        .into_iter()
        .map(|e| e.name.as_str().to_string())
        .collect()
}
