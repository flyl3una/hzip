//! 标准消息弹窗的**载荷**（与业务无关）。
//!
//! 业务侧只构造「级别 + 标题 + 详情」三件套，由 UI 层
//! [`crate::ui::views::op_window::open_message_window`] 统一渲染成
//! **图标 + 标题 + 可滚动详情 + 关闭按钮**的标准窗口（可拖动调整大小）。
//!
//! # 为什么要收口
//!
//! 失败提示此前散在各处：直接模式失败画一个只有一行字的窗口（不可调大小、超长内容
//! 看不到），设置页关联失败只写状态行。用户口径（2026-09-29）：**弹窗应当是标准实现**
//! ——级别（信息/警告/错误）、标题、详细信息、关闭按钮，业务按需调用。
//! 于是把载荷独立在此（`viewmodel` 层，UI 与业务都能引用），渲染只有一处。

/// 消息级别（决定窗口的图标与配色；文案由调用方给）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLevel {
    /// 信息（蓝）：陈述事实，无风险。
    Info,
    /// 警告（黄）：部分成功、需要用户留意（如「某些格式没接管成功」）。
    Warning,
    /// 错误（红）：操作失败。
    Error,
}

/// 标准消息弹窗载荷。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDialog {
    /// 级别（图标 + 配色）
    pub level: MessageLevel,
    /// 标题（窗口标题栏 + 内容首行）
    pub title: String,
    /// 详细信息（可滚动；可含换行，逐条列出）
    pub detail: String,
}

impl MessageDialog {
    /// 信息弹窗。
    pub fn info(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: MessageLevel::Info,
            title: title.into(),
            detail: detail.into(),
        }
    }

    /// 警告弹窗（部分成功/需留意）。
    pub fn warning(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: MessageLevel::Warning,
            title: title.into(),
            detail: detail.into(),
        }
    }

    /// 错误弹窗（操作失败）。
    pub fn error(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: MessageLevel::Error,
            title: title.into(),
            detail: detail.into(),
        }
    }
}
