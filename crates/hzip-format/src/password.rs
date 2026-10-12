//! 密码承载与来源（02 §1.2/§7.1/§7.3；契约冻结）。

use zeroize::Zeroizing;

use crate::types::EntryName;

/// 密码只读引用（02 §1.2：密码一律 `Zeroizing` 包裹或借用传递；本类型不持有
/// 明文所有权，只借用 `Zeroizing<String>`）。
#[derive(Debug, Clone, Copy)]
pub struct PasswordRef<'a>(Option<&'a Zeroizing<String>>);

impl<'a> PasswordRef<'a> {
    /// 未加密路径的空引用。
    pub fn none() -> Self {
        Self(None)
    }

    /// 以 `Zeroizing<String>` 借用构造。
    pub fn new(pw: &'a Zeroizing<String>) -> Self {
        Self(Some(pw))
    }

    /// 明文访问（None = 未加密）。
    pub fn as_str(&self) -> Option<&'a str> {
        self.0.map(|z| z.as_str())
    }
}

impl Default for PasswordRef<'_> {
    fn default() -> Self {
        Self::none()
    }
}

/// 密码来源抽象（02 §7.3）：格式层只消费不构造；core 实现请求-应答
/// （02 §9.11 `JobRequest::Password`）。每次调用可返回新密码（重试语义）。
pub trait PasswordSource: Send + Sync {
    /// 取密码（`entry` 为 None 表示档案级询问）。
    fn password(&self, entry: Option<&EntryName>) -> Option<Zeroizing<String>>;
}

#[cfg(test)]
#[path = "tests/password.rs"]
mod tests;
