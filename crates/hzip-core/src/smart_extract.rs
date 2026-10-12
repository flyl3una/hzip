//! 解压目录形态判定（T-RE 语义重构：默认保真原样展开）。
//!
//! # 语义（v2，替代旧 §9.7 智能解包剥壳）
//!
//! 输入为 `entries()` 的条目名集合（净化前——判定只看形态不看路径安全），
//! 输出为解包落盘根：
//!
//! - **默认（`wrap=false`）**：恒 `dest_root` 原样展开——条目名即相对路径，
//!   不剥任何目录层、不额外包壳（`x.zip` 内 `a/b` → `dest/a/b`）。
//! - **`--smart`（`wrap=true`）**：仅当档内**会污染目标根**（多个顶层条目：
//!   散文件、或混排目录+文件、或 2+ 顶层目录）时，自动在 `dest_root` 下以
//!   档案 stem 建一个目录再展开（`x.zip` 内 `a/`+`b.txt` → `dest/x/a/...`）；
//!   恰 1 个顶层目录或恰 1 个文件（不会污染）→ 恒原样展开（不包壳、不剥壳）。
//!
//! 旧 §9.7 的「单顶层目录剥壳直展（root/a → a）」已移除——那正是用户报告的
//! 路径平铺根因。`--flat`（强制剥壳直展）一并废弃。
//!
//! 档案 stem（壳目录名）剥后缀：逐层剥 sniff 报告的压缩层（`.gz/.bz2/
//! .xz/.zst`）与最内层容器后缀（`.tar/.zip/.7z/.rar`），至无匹配为止
//! （`a.tar.gz → a`）。剥空 → 以 dest_root 文件名兜底，仍空 → `archive`。
//!
//! 出处：02 §9.7 图（v2 语义修订）、附录 B.1 `extract.smart`（重命名 wrap）。

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use hzip_error::HzipError;
use hzip_format::EntryInfo;

/// Wrap 壳目录撞名时的候选上限（`stem`、`stem (2)` … `stem (10)`）。
///
/// 上限只为防病态目录；正常一两次内必命中。用尽仍失败 → `Err Io`。
const WRAP_RENAME_MAX: usize = 10;

/// 解包落盘根判定产物（v2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DestPlan {
    /// 直接展开到 dest_root（默认恒此；条目名原样为 rel）。
    Direct,
    /// 在 dest_root 下以档案 stem 建壳目录再展开（--smart 且多顶层时）。
    Wrap { dir_name: OsString },
}

/// 纯函数判定：条目名集合 → 是否需要包壳（`--smart` 的自动判别）。
///
/// 判定规则（v2，无剥壳）：
/// - `wrap=false` → 恒 `Direct`（默认保真原样）；
/// - 空条目集 → `Direct`；
/// - 恰 1 个顶层条目（目录或文件，即全部条目同属一个顶层名）→ `Direct`
///   （单顶层目录不剥不包；单文件原样）；
/// - 其余（散文件、混排、2+ 顶层目录）→ `Wrap`（防污染目标根）。
pub(crate) fn plan(entries: &[EntryInfo], wrap: bool) -> DestPlan {
    if !wrap {
        return DestPlan::Direct;
    }
    if entries.is_empty() {
        return DestPlan::Direct;
    }
    // A：全部顶层条目名（去重）
    let tops: BTreeSet<&str> = entries
        .iter()
        .map(|e| {
            e.name
                .as_str()
                .split('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or_default()
        })
        .collect();
    // 恰 1 个顶层名（含空名兜底）→ 单顶层目录或单文件形态 → 原样直展。
    // （顶层集合唯一即不污染目标根：无论该顶层是目录（root/…）还是文件
    // （a.txt）——剥壳已废弃，单文件/单目录都不包壳。）
    if tops.len() == 1 {
        return DestPlan::Direct;
    }
    DestPlan::Wrap {
        dir_name: OsString::new(), // 占位；resolve_dest 填入真实 stem
    }
}

/// 解析实际目标目录。
///
/// `dest_root`：用户指定解压根（None → 源所在目录，`effective_root` 已填）。
/// `archive_stem`：档案已剥全部已知后缀的基名（Wrap 目录名来源）。
/// `wrap`：`--smart` 开关（默认 false = 恒 Direct）。
///
/// 返回 `(目标目录, 可剥前缀)`：
/// - Direct → `(dest_root, None)`——条目名原样 rel，恒不剥；
/// - Wrap → `(dest_root.join(stem), None)`，**立即创建**该目录（含父链）。
pub(crate) fn resolve_dest(
    entries: &[EntryInfo],
    dest_root: &Path,
    archive_stem: Option<&str>,
    wrap: bool,
) -> Result<(PathBuf, Option<String>), HzipError> {
    match plan(entries, wrap) {
        DestPlan::Direct => Ok((dest_root.to_path_buf(), None)),
        DestPlan::Wrap { .. } => {
            let stem = archive_stem
                .filter(|s| !s.is_empty())
                .map(OsString::from)
                .unwrap_or_else(|| {
                    dest_root
                        .file_name()
                        .map(|f| f.to_os_string())
                        .unwrap_or_else(|| OsString::from("archive"))
                });
            let dir = create_unique_dir(dest_root, &stem)?;
            Ok((dir, None))
        }
    }
}

/// 在 `dest_root` 下取一个**可用作壳目录**的名字并创建它。
///
/// - `stem` 尚不存在，或已是目录（合并语义：目录存在即复用）→ 直接用；
/// - `stem` 被**同名文件**占着（或与其他进程竞态导致创建失败）→ 依次尝试
///   `stem (2)`、`stem (3)`…（上限 [`WRAP_RENAME_MAX`]）。
///
/// 为什么需要：`.tgz`/`.tbz2` 这类别名后缀在旧实现里没被剥掉，壳目录名恰好等于
/// **档案自己的文件名** ⇒ `create_dir_all` 报 os 183「当文件已存在时，无法创建该
/// 文件」，用户看到的是 HZ-1001「目标不可写」（磁盘其实充裕，实测用户报障）。
/// 剥后缀修了主因，本函数是兜底：同名文件/并发建目录都不该让解压直接失败。
///
/// 全部候选都失败 → `Err Io`（带最后尝试的路径；错误文案由上层展示）。
fn create_unique_dir(dest_root: &Path, stem: &OsStr) -> Result<PathBuf, HzipError> {
    let mut last_err: Option<(std::io::Error, PathBuf)> = None;
    for n in 0..WRAP_RENAME_MAX {
        let name = if n == 0 {
            stem.to_os_string()
        } else {
            let mut s = stem.to_os_string();
            s.push(format!(" ({})", n + 1));
            s
        };
        let dir = dest_root.join(&name);
        // 已存在且是目录 → 合并（与 7-Zip/Bandizip 一致，不打扰用户）
        if dir.is_dir() {
            return Ok(dir);
        }
        match std::fs::create_dir_all(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) => {
                // 同名文件占用（os 183/80）或竞态 → 换下一个候选；其余错误直接上抛
                let kind = e.kind();
                if matches!(kind, std::io::ErrorKind::AlreadyExists)
                    || matches!(e.raw_os_error(), Some(183) | Some(80))
                {
                    last_err = Some((e, dir));
                    continue;
                }
                return Err(HzipError::Io {
                    source: e,
                    path: Some(dir),
                });
            }
        }
    }
    let (e, dir) = last_err.unwrap_or_else(|| {
        (
            std::io::Error::new(std::io::ErrorKind::AlreadyExists, "壳目录候选均被占用"),
            dest_root.join(stem),
        )
    });
    Err(HzipError::Io {
        source: e,
        path: Some(dir),
    })
}

/// 应用可剥前缀（保留——历史 Bare 剥壳路径已移除，恒返回原名）。
///
/// v2 恒 `None`（不再剥顶层）；此函数保留供调用方兼容（strip 恒 None）。
pub(crate) fn apply_strip(name: &str, strip: Option<&str>) -> String {
    match strip {
        Some(top) if !top.is_empty() => {
            let prefix = format!("{top}/");
            name.strip_prefix(&prefix)
                .map(|s| s.to_string())
                .unwrap_or_else(|| name.to_string())
        }
        _ => name.to_string(),
    }
}

/// 剥已知压缩/容器后缀（Wrap 目录名：`a.tar.gz → a`；逐层至无可剥）。
///
/// 供 `services::extract` 依 sniff 层清单生成 Wrap 目录名。大小写不敏感；
/// 剥空（如纯 `.gz`）→ 回退原名。
pub(crate) fn strip_suffixes(name: &str, suffixes: &[&str]) -> String {
    let mut out = name.to_string();
    loop {
        let lower = out.to_ascii_lowercase();
        let mut stripped = false;
        for sfx in suffixes {
            let sfx_l = sfx.to_ascii_lowercase();
            let dot_sfx_l = format!(".{sfx_l}");
            if lower.ends_with(&dot_sfx_l) {
                let new_len = out.len() - dot_sfx_l.len();
                if new_len > 0 {
                    out.truncate(new_len);
                    stripped = true;
                }
                break;
            }
        }
        if !stripped {
            return out;
        }
    }
}

#[cfg(test)]
#[path = "tests/smart_extract.rs"]
mod tests;
