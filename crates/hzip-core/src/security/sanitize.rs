//! `sanitize_entry_path` 净化算法（02 §9.6 流程图逐分支；红线函数）。
//!
//! 输入为档案条目名（`EntryName`——T20 语法规范化已做规则 N1~N6，净化仍归本模块）。
//! 输出为「拒绝（`Security` 错误）或净化后的目标路径」；**绝不允许**任何输入
//! 把写入路径引到 `dest` 之外。实现为纯函数便于属性测试与 fuzz（T51 TC-FUZ-005）。

use std::path::{Component, Path, PathBuf};

use hzip_error::{HzipError, SecurityViolation};
use hzip_format::{EntryName, SanitizeConfig};

/// Windows 保留设备名全表（02 §9.6 X4 节点；22 个）。
///
/// 仅 Windows 通道消费（unix 恒放行，见 [`reserved_on_win`]）。
#[cfg(windows)]
const RESERVED_DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", //
    "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", //
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 段尾截断 warning（02 §9.6 L→M 节点：win 截断并记原段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncateWarning {
    /// 原始段名
    pub original: String,
    /// 截断后的段名
    pub trimmed: String,
}

/// 净化结果（内部类型，`pub(crate)`——不升级为公开 API）。
#[derive(Debug, Clone)]
pub struct SanitizedEntry {
    /// `dest.join(rel)`（win 已注入 `\\?\`，供文件系统 API 直接使用）
    pub dest_path: PathBuf,
    /// 净化后的相对路径（unix 语义；`rel.is_empty()` = dest 本身）
    pub rel: PathBuf,
    /// 截断 warning（win 段尾空格/句点截断；unix 恒空）
    pub warnings: Vec<TruncateWarning>,
}

/// Windows 保留设备名判定（02 §9.6：含带扩展名形式，大小写不敏感）。
/// `CON`/`con.txt`/`NUL.bin`/`COM1`/`lpt9.log` 命中；`CONtroller`/`CONX.txt`
/// 不命中；`CON.x.txt` 命中（仅看首个 `.`）。
///
/// 仅 Windows 通道消费（unix 恒放行，03 §1.3 规则 1：平台差异写成对照断言）。
#[cfg(windows)]
pub(crate) fn is_reserved_device_name(seg: &str) -> bool {
    let stem = seg.split('.').next().unwrap_or(seg);
    RESERVED_DEVICE_NAMES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem))
}

/// 段内是否含 NUL/C0/C1 控制字符（02 §9.6 X3 节点）。
fn has_control_chars(seg: &str) -> bool {
    seg.chars().any(|ch| {
        let c = ch as u32;
        ch == '\0' || (c <= 0x1F) || c == 0x7F || (0x80..=0x9F).contains(&c)
    })
}

/// 净化算法（02 §9.6 流程图逐分支翻译）。
///
/// # 规则
///
/// - 绝对路径前缀三查（`/` 或 `\` 开头 / `C:` 盘符 / `\\`UNC 或 `//`）→ 4001；
/// - `..` 段**不做抵消**，直接拒绝 4001；空段与 `.` 丢弃；
/// - 控制字符段 → 4004；
/// - Windows 保留名：win → 4003；unix → 放行；
/// - 段尾空格/句点：win 截断 + warning（对截断结果复检保留名，双检闭合
///   `CON. ` → 截断 → `CON` → 4003）；unix 保留；截断为空 → 丢弃该段；
/// - 全部段被丢弃 → 语义为 dest 本身（不报错，warning 一条）。
pub fn sanitize_entry_path(
    raw: &EntryName,
    dest: &Path,
    cfg: &SanitizeConfig,
) -> Result<SanitizedEntry, HzipError> {
    let raw_str = raw.as_str();
    let sec = |v: SecurityViolation| HzipError::Security { violation: v };

    // —— 分支 J：首段绝对路径标志（三查按序，命中即 4001）——
    if raw_str.starts_with('/') || raw_str.starts_with('\\') {
        return Err(sec(SecurityViolation::PathTraversal(raw_str.into())));
    }
    let bytes = raw_str.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(sec(SecurityViolation::PathTraversal(raw_str.into())));
    }
    if raw_str.starts_with("\\\\") || raw_str.starts_with("//") {
        return Err(sec(SecurityViolation::PathTraversal(raw_str.into())));
    }

    // —— 分支 B：按 '/' 与 '\' 切分（EntryName 语义上无 '\'，双保险）——
    let segs: Vec<&str> = raw_str.split(['/', '\\']).collect();

    // —— 分支 C/D：逐段判定 ——
    let mut kept: Vec<String> = Vec::new();
    for seg in segs {
        if seg.is_empty() || seg == "." {
            continue; // E：丢弃
        }
        if seg == ".." {
            // X2：不做抵消，直接拒绝
            return Err(sec(SecurityViolation::PathTraversal(raw_str.into())));
        }
        if has_control_chars(seg) {
            // X3：NUL + C0/C1
            return Err(sec(SecurityViolation::ControlChars));
        }
        // X4：保留名（win 拒绝；unix 放行）
        if reserved_on_win(seg) {
            return Err(sec(SecurityViolation::ReservedDeviceName));
        }
        kept.push(seg.to_string());
    }

    // —— 分支 L：段尾空格/句点（win 截断 + 复检；unix 保留）——
    let mut warnings = Vec::new();
    let mut finalized: Vec<String> = Vec::with_capacity(kept.len());
    let cfg_used = cfg.symlink; // 语义保持：本函数消费 cfg（截断策略属净化配置族）
    let _ = cfg_used;
    for seg in kept {
        let trimmed = trim_win_trailing(&seg);
        #[cfg(windows)]
        {
            if trimmed.len() != seg.len() {
                // 双检：截断后复跑保留名判定（"CON. " → "CON" → 4003）
                if reserved_on_win(&trimmed) {
                    return Err(sec(SecurityViolation::ReservedDeviceName));
                }
                if trimmed.is_empty() {
                    continue; // "   " 之类截空 → 丢弃
                }
                warnings.push(TruncateWarning {
                    original: seg.clone(),
                    trimmed: trimmed.clone(),
                });
                finalized.push(trimmed);
            } else {
                finalized.push(seg);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = trimmed;
            finalized.push(seg);
        }
    }

    // —— kept 为空（全被丢弃，如 "." 或 ""）→ dest 本身 + warning ——
    if finalized.is_empty() {
        warnings.push(TruncateWarning {
            original: raw_str.into(),
            trimmed: String::new(),
        });
    }

    // —— 分支 O：dest.join(rel)；win 经 long_path 注入 \\?\ ——
    let rel_str = finalized.join("/");
    let rel = if rel_str.is_empty() {
        PathBuf::new()
    } else {
        PathBuf::from(rel_str)
    };
    debug_assert!(!rel.is_absolute());
    debug_assert!(!rel.components().any(|c| matches!(c, Component::ParentDir)));
    let joined = dest.join(rel.clone());
    #[cfg(windows)]
    let dest_path = hzip_platform::long_path::to_extended(&joined);
    #[cfg(not(windows))]
    let dest_path = joined;

    Ok(SanitizedEntry {
        dest_path,
        rel,
        warnings,
    })
}

/// 保留名判定：win 通道真判；unix 恒放行（平台对照，03 §1.3 规则 1）。
#[cfg(windows)]
fn reserved_on_win(seg: &str) -> bool {
    is_reserved_device_name(seg)
}

#[cfg(not(windows))]
fn reserved_on_win(_seg: &str) -> bool {
    false
}

/// win 段尾剥离空格与句点；unix 原样返回（截断仅 win 语义，02 §9.6 L→M）。
#[cfg(windows)]
fn trim_win_trailing(seg: &str) -> String {
    seg.trim_end_matches([' ', '.']).to_string()
}

#[cfg(not(windows))]
fn trim_win_trailing(seg: &str) -> String {
    seg.to_string()
}
