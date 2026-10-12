//! 进程级**会话密码缓存**（用户口径 2026-09-28 / 2026-09-29）。
//!
//! # 语义（用户原话）
//!
//! > 这个密码应该输入一次后缓存下来，后面只要 hzip-gui 程序没有关闭，则应该继续
//! > 使用上一次的密码……重新打开压缩包时如果检查到该压缩包密码已缓存，则应该
//! > 继续使用上一次的密码。
//!
//! 于是键 = 档案路径、值 = 该档案的密码，**进程内存活**（不落盘、无过期）：
//! 同一档案的后续「打开文件 / 解压 / 编辑（新建、重命名、删除）」直接复用，
//! 不再弹密码框——即用户说的「该压缩包处于解密中状态」。
//!
//! # 为什么是**进程级**（而不是每窗口一份）
//!
//! 多窗口下每个窗口一份 `AppData`（设计 §5.1 裁定 2），但用户口径是「程序没关
//! 就继续用上一次的密码」——窗口 B 打开窗口 A 刚解锁过的档案，同样不该再问。
//!
//! # 清空规则（用户口径 2026-09-29）
//!
//! > 这个缓存只影响 hzip-gui 的界面操作；基于操作系统文件资源管理器的右键菜单
//! > 触发的 hzip-gui 加解密，应该都跟 hzip-gui 重新打开一样会清空密码缓存。
//!
//! - 新进程：缓存本就为空（不落盘）；
//! - 已在跑的实例收到**二实例转发**（右键菜单/关联动词）→ [`Self::clear`] 一次，
//!   该次加解密等于「重新打开」，不吃旧缓存；
//! - 密码被证伪（读该档案报 `Password` 错）→ [`Self::forget`] 单条丢弃并重问。
//!
//! # 安全
//!
//! 一律 `Zeroizing<String>` 持有（02 §1.2）：明文只在内存、退出即清零，绝不落盘、
//! 不进日志（`Debug` 手写实现不打印内容）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use zeroize::Zeroizing;

/// 进程级会话密码缓存（见模块文档）。
#[derive(Default)]
pub struct PasswordCache {
    map: Mutex<HashMap<PathBuf, Zeroizing<String>>>,
}

impl std::fmt::Debug for PasswordCache {
    /// 只报条数——**密码明文绝不进日志/诊断**（02 §1.2 红线）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordCache")
            .field("entries", &self.len())
            .finish()
    }
}

impl PasswordCache {
    /// 空缓存。
    pub fn new() -> Self {
        Self::default()
    }

    /// 取该档案的缓存密码（`None` = 尚未解锁 → 读加密内容会 3101，需过密码门）。
    pub fn get(&self, archive: &Path) -> Option<Zeroizing<String>> {
        let map = self.map.lock().ok()?;
        map.get(archive).cloned()
    }

    /// 记住**已验证正确**的密码（覆盖旧值：改密码后重输即刷新）。
    pub fn remember(&self, archive: &Path, pw: Zeroizing<String>) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(archive.to_path_buf(), pw);
        }
    }

    /// 丢弃单条（密码失效自愈 / 嵌套会话退出）。
    pub fn forget(&self, archive: &Path) {
        if let Ok(mut map) = self.map.lock()
            && map.remove(archive).is_some()
        {
            tracing::info!(archive = %archive.display(), "会话密码缓存：已丢弃");
        }
    }

    /// 清空全部（二实例转发 = 右键菜单触发的加解密：与「重新打开」同语义）。
    pub fn clear(&self) {
        if let Ok(mut map) = self.map.lock() {
            let n = map.len();
            map.clear();
            if n > 0 {
                tracing::info!(entries = n, "会话密码缓存：已清空（外部触发的加解密）");
            }
        }
    }

    /// 缓存条数（诊断/测试用；不暴露键集合即不暴露档案路径）。
    pub fn len(&self) -> usize {
        self.map.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
#[path = "tests/password_cache.rs"]
mod tests;
