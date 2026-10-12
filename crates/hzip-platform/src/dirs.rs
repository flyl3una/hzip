//! 配置/数据/缓存/日志/临时目录解析（02 §5.3.10）。
//!
//! | 用途 | 函数 | Windows | macOS | Linux |
//! |---|---|---|---|---|
//! | 配置 | `config_dir` | `%APPDATA%\hzip` | `~/Library/Application Support/hzip` | `$XDG_CONFIG_HOME/hzip`（缺省 `~/.config`） |
//! | 数据 | `data_dir` | `%LOCALAPPDATA%\hzip` | `~/Library/Application Support/hzip` | `$XDG_DATA_HOME/hzip`（缺省 `~/.local/share`） |
//! | 缓存 | `cache_dir` | `%LOCALAPPDATA%\hzip\cache` | `~/Library/Caches/hzip` | `$XDG_CACHE_HOME/hzip`（缺省 `~/.cache`） |
//! | 日志 | `log_dir` | `%LOCALAPPDATA%\hzip\logs` | `~/Library/Logs/hzip` | `$XDG_STATE_HOME/hzip`（缺省 `~/.local/state`） |
//! | 临时 | `temp_root` | `GetTempPath()/hzip` | `$TMPDIR/hzip`（缺省 `/tmp`） | `XDG_RUNTIME_DIR 或 /tmp` 下 `hzip` |
//! | 桌面 | `desktop_dir` | `%USERPROFILE%\Desktop` | `~/Desktop` | `$XDG_DESKTOP_DIR`（缺省 `~/Desktop`） |
//!
//! **注**：macOS 上 `config_dir` 与 `data_dir` 同址（`~/Library/Application Support/hzip`）
//! ——App Support 同时承担两者角色，属平台约定；不要据此判定「目录解析出错」。
//!
//! 实现可用 `dirs` crate 取基准目录再拼 `hzip`（减少 FFI 面），但
//! `XDG_STATE_HOME` 等 XDG 细节自实现（`dirs` crate 不覆盖 STATE，02 §5.3.10）。
//! 唯 `desktop_dir` 例外：桌面是用户自有目录，**不拼 `hzip` 子目录**。
//! 全部函数**只创建路径不强制 mkdir**（创建时机由调用方决定）；`HZIP_CONFIG`
//! 覆盖逻辑归 hzip-config（不在本 crate）。

use std::path::PathBuf;

/// 配置目录（02 §5.3.10）：`hzip` 子目录下放 `config.toml`（02 §12.1）。
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(fallback_home)
        .join("hzip")
}

/// 数据目录（02 §5.3.10 语义扩展：win LOCALAPPDATA / mac Library / linux XDG_DATA）。
pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        dirs::data_local_dir()
            .unwrap_or_else(fallback_home)
            .join("hzip")
    }
    #[cfg(not(windows))]
    {
        dirs::data_dir().unwrap_or_else(fallback_home).join("hzip")
    }
}

/// 缓存目录（02 §5.3.10 语义扩展）。
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(fallback_home)
        .join("hzip")
        .join("cache")
}

/// 日志目录（02 §5.3.10）。
pub fn log_dir() -> PathBuf {
    #[cfg(windows)]
    {
        dirs::data_local_dir()
            .unwrap_or_else(fallback_home)
            .join("hzip")
            .join("logs")
    }
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir()
            .unwrap_or_else(fallback_home)
            .join("Library")
            .join("Logs")
            .join("hzip")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        xdg_state_home().join("hzip")
    }
}

/// 临时根目录（02 §5.3.10）：会话目录（`{tmp}/hzip/{uuid}`，附录 B.2）的父级。
pub fn temp_root() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::temp_dir().join(crate::consts::TEMP_SESSION_DIR)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(crate::consts::TEMP_SESSION_DIR)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir())
            .join(crate::consts::TEMP_SESSION_DIR)
    }
}

/// 桌面目录（用户可见的 Desktop）：`None` = 取不到（调用方据此置灰菜单项，
/// **不回落**到 home，避免静默解压到意料之外的位置）。
/// 与既有函数不同：**不拼 `hzip` 子目录**（桌面不是 hzip 自有目录）。
pub fn desktop_dir() -> Option<PathBuf> {
    dirs::desktop_dir()
}

/// Linux XDG state home（02 §5.3.10 自实现：`$XDG_STATE_HOME` 缺省 `~/.local/state`）。
#[cfg(all(unix, not(target_os = "macos")))]
fn xdg_state_home() -> PathBuf {
    if let Some(v) = std::env::var_os("XDG_STATE_HOME") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    fallback_home().join(".local").join("state")
}

/// 兜底家目录（dirs 查询失败时）。
fn fallback_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
#[path = "tests/dirs.rs"]
mod tests;
