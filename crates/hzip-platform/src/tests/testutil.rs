//! 单元测试共享工具（仅 `cfg(test)` 编译）。
//!
//! 提供临时目录助手（不引入 tempfile 等测试第三方依赖，05 §1.2 纪律）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 在系统临时目录下创建唯一测试目录，Drop 时安全删除。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// 便捷拼接子路径（`path().join(seg)`）。
    pub fn join(&self, seg: impl AsRef<std::path::Path>) -> PathBuf {
        self.path.join(seg)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = crate::fs_util::remove_all_no_follow(&self.path);
    }
}

/// 创建唯一临时目录（前缀命名：`hzip-test-{tag}-{pid}-{seq}`）。
pub fn temp_dir(tag: &str) -> TempDir {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("hzip-test-{tag}-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&base).expect("创建测试临时目录");
    TempDir { path: base }
}
