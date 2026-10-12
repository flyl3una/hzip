//! 条目名推导（02 §8.5 规则表；与 T20 registry 共享同一推导语义）。
//!
//! 优先级（T23 任务卡 §3.1 表）：
//!
//! | 优先级 | 来源 | 规则 |
//! |---|---|---|
//! | 1 | gzip FNAME 头字段 | 存在且 `parse_entry_name` 通过 → 采用 |
//! | 2 | `OpenOptions.entry_name` | stdin/流的 `--entry-name` 覆盖 |
//! | 3 | 档案文件名 | 剥**一层**格式后缀（`a.txt.gz→a.txt`；`a.gz.gz→a.gz`） |
//! | 4 | 兜底 | stdin 无名 → `"stdin"`；均不可得 → 格式 id |
//!
//! 本模块同时被 hzip-format registry 的 `Composite{tail: Raw}` 路径（多层
//! `.tar.gz.gz` 剥多层后缀）共享设计语义（02 §7.5 规则③）；当前 fmt-single 侧
//! 只做单层（每层由外层 registry 逐层剥离）。规则以本模块为单一事实来源，
//! 与 T20 侧（任务卡 §7-6）以同一组单测锁定，禁止语义漂移。

use std::path::{Path, PathBuf};

use hzip_error::EntryName;
use hzip_format::{FormatId, parse_entry_name};

/// 名字推导所需的源形态（从 `Source` 提取的最小信息；与 T20 registry 共享
/// 推导语义，见模块文档）。
#[derive(Debug, Clone)]
pub(crate) enum NameShape {
    /// 有档案文件路径（可剥一层格式后缀）
    File(PathBuf),
    /// stdin 无名流
    Stdin,
    /// Stream / 其它无名流
    Stream,
}

impl NameShape {
    /// 从 `hzip_format::Source` 提取（只借用）。
    pub(crate) fn from_source(src: &hzip_format::Source) -> Self {
        match src {
            hzip_format::Source::File(p) | hzip_format::Source::Volumes { first: p, .. } => {
                Self::File(p.clone())
            }
            hzip_format::Source::Stdin => Self::Stdin,
            hzip_format::Source::Stream(_) => Self::Stream,
        }
    }
}

/// 单文件流的单层推导完整入口（`entries()` 用）：给定源形态与四格式参数返回
/// 最终条目名。`fmt_id`/`extensions` 均为格式静态参数。
pub(crate) fn derive_entry_name(
    shape: &NameShape,
    fmt_id: &FormatId,
    extensions: &[&str],
    fname: Option<&str>,
    override_name: Option<&EntryName>,
) -> EntryName {
    // 优先级 1：gzip FNAME（过 parse_entry_name）
    if let Some(f) = fname
        && let Ok(n) = parse_entry_name(f)
    {
        return n;
    }
    // 优先级 2：OpenOptions.entry_name
    if let Some(n) = override_name {
        return n.clone();
    }
    // 优先级 3：档案文件名剥一层格式后缀
    if let NameShape::File(path) = shape
        && let Some(base) = file_base_name(path)
    {
        let stripped = strip_one_ext(&base, extensions);
        let candidate = stripped.as_deref().unwrap_or(&base);
        if !candidate.is_empty()
            && let Ok(n) = parse_entry_name(candidate)
        {
            return n;
        }
    }
    // 优先级 4：兜底（stdin → "stdin"；其余 → 格式 id）
    match shape {
        NameShape::Stdin => EntryName::new("stdin"),
        _ => EntryName::new(fmt_id.as_str()),
    }
}

/// 档案文件名字节（Windows/Unix 路径均按 `Path` 语义取末段）。
fn file_base_name(path: &Path) -> Option<String> {
    let name = path.file_name()?;
    // 优先无损 UTF-8；非 UTF-8 文件名经 lossy（Windows 上 Path 常用 UTF-16，
    // 非 Unicode 罕见；lossy 后再过 parse_entry_name，若含替换字符仍可能合法落盘，
    // 净化归 core，本层不额外拦截）
    Some(name.to_string_lossy().into_owned())
}

/// 剥**一层**格式后缀：仅当文件名以 `.ext`（`ext ∈ extensions`，大小写不敏感）
/// 结尾时剥离并返回剩余名；否则 `None`（调用方使用原文件名）。
///
/// 只剥一层：`a.gz.gz → a.gz`（多层剥离归 T20 组合链，本层不递归）。
pub(crate) fn strip_one_ext(file_name: &str, extensions: &[&str]) -> Option<String> {
    let (stem, dot_ext) = split_one_ext(file_name);
    let ext = dot_ext?;
    if extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
        Some(stem.to_owned())
    } else {
        None
    }
}

/// 在最后一个 `.` 处切分文件名（`a.txt.gz → ("a.txt", Some("gz"))`；无点 → `None`；
/// `.gz` / `a.` 等前导点文件不切）。
fn split_one_ext(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], Some(&name[i + 1..])),
        _ => (name, None),
    }
}

#[cfg(test)]
#[path = "tests/name.rs"]
mod tests;
