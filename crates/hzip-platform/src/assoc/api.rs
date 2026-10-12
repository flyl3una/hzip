//! 文件关联的**数据面**：类型（`AssocSpec`/`ExtAssocState`）与常量、纯函数。
//!
//! 平台分发门面在 `facade.rs`；模块路径（`crate::assoc::*`）由 `mod.rs` 的再导出保持不变。

use std::path::PathBuf;

/// 文件关联规格（调用方填 `exe_path` = 本程序可执行文件绝对路径）。
pub struct AssocSpec {
    /// 勾选的扩展名（不带点，如 `["zip","7z"]`）。
    pub extensions: Vec<String>,
    /// Windows ProgId（如 `"hzip.Archive"`）。Linux/macOS 后端的标识是平台自己
    /// 定的（`hzip.desktop` / bundle id），此字段在那些平台上被忽略。
    pub prog_id: &'static str,
    /// 本程序可执行文件绝对路径。
    pub exe_path: PathBuf,
}

/// Windows ProgId（`AssocSpec.prog_id` 的**单一来源**；[`super::win::ext_state`] 用它
/// 判定「双击是否归我们」）。
pub const PROG_ID: &str = "hzip.Archive";

/// Windows ProgId 展示名（也用于 `Capabilities` 的 `ApplicationDescription`）。
#[cfg(windows)]
pub(super) const PROG_ID_DESC: &str = "hzip 压缩档案";

/// verb 命令串格式化：exe 与 `"%1"` 加引号（引号嵌套），flag 裸写。
///
/// 仅 Windows 注册表 verb 使用；`test` 下保留以便全平台跑引号嵌套断言
/// （见本模块 `command_quote_nesting`）。
#[cfg(any(windows, test))]
pub(super) fn format_verb_command(exe_path: &str, args: &[&str]) -> String {
    let mut parts = vec![format!("\"{exe_path}\"")];
    for arg in args {
        if *arg == "%1" {
            parts.push("\"%1\"".to_string());
        } else {
            parts.push((*arg).to_string());
        }
    }
    parts.join(" ")
}

/// 该扩展名在 Linux 后端的 MIME 表里是否有映射。
///
/// 给一致性测试用：文件关联页的候选扩展名（来自 `hzip-format` 的表）若漏了
/// MIME，Linux 上就会出现「能勾选、却不生效」的静默失效——本函数让那类漂移
/// 在测试里当场暴露（本 crate 不依赖 `hzip-format`，红线 4）。
pub fn mime_supports(ext: &str) -> bool {
    super::mime::mime_of(ext).is_some()
}

/// Windows：提权子进程的内部开关（`hzip-gui --hzip-elevate-clear-user-choice=<ext,…>`）。
///
/// 只有 `apply_association` 直删 `UserChoice` 被 DACL 拒时才用到它；**不进公开
/// argv 协议**（02 §11.5 冻结的那套），由 `main.rs` 在解析协议**之前**截获。
#[cfg(windows)]
pub const ELEVATE_FLAG: &str = "--hzip-elevate-clear-user-choice=";

/// 某扩展名的关联实况（回读用；UI 据此判断「双击到底归谁」）。
///
/// # 跨平台语义
///
/// 四个字段表达的是**平台无关的三态**：
///
/// | 现场 | `ours` | `pinned` | UI 该说 |
/// |---|---|---|---|
/// | 双击归我们 | true | — | 已生效 |
/// | 别人占着，但程序改得动（Linux/macOS，或 Windows 无 `UserChoice`） | false | false | 点「应用关联」即生效 |
/// | 别人占着且改不动（Windows `UserChoice`） | false | true | 改不动，请用「打开方式」点一次 |
///
/// 各平台**采集**这些字段的规则见模块文档的「三平台机制总览」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtAssocState {
    /// 双击**实际**会落到哪里（平台最高优先级层的标识）：Windows = `UserChoice`
    /// 的 ProgId（无则 `HKCU`/`HKLM` 的 `Classes\.<ext>` 默认）；Linux =
    /// `mimeapps.list` 的默认项（`.desktop` 文件名）；macOS = `LSHandlers` 的
    /// bundle id。`None` = 没被显式指派（Linux/Windows 会弹「打开方式」）。
    pub effective: Option<String>,
    /// `effective` 的**可读名**（Windows：ProgId 描述；Linux：`.desktop` 的
    /// `Name`；macOS：bundle 显示名）。查不到 → `None`（界面回落标识原文）。
    pub effective_name: Option<String>,
    /// 双击是否已归本程序（`effective` 是平台里我们自己的标识）。
    pub ours: bool,
    /// 该默认是否**程序改不动**：只有 Windows 的 `UserChoice` 为真（哈希保护）；
    /// Linux / macOS 直接改配置文件即可夺回 ⇒ 恒 `false`。
    pub pinned: bool,
}

impl ExtAssocState {
    /// 双击是否已归本程序。
    pub fn is_effective(&self) -> bool {
        self.ours
    }

    /// 是否被**别的程序的、程序改不动的**默认压着（Windows `UserChoice`）。
    ///
    /// 只有这种情形需要用户本人出手；Linux/macOS 上的「别人是默认」不算
    /// （点一下「应用关联」我们就能改过来）。
    pub fn blocked_by_other(&self) -> bool {
        !self.ours && self.pinned
    }

    /// 占用者的可读名（无 → 标识原文；再无不 → 空串）。
    pub fn owner_name(&self) -> Option<&str> {
        self.effective_name.as_deref().or(self.effective.as_deref())
    }
}
