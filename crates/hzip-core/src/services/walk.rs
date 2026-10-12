//! 压缩输入展开 + 过滤链（02 §9.5 B 框；T33 独占）。
//!
//! # 展开规则（B 框逐步）
//!
//! - **目录输入**：walkdir 递归（`follow_links = filters.follow_symlinks`——
//!   跟随开启时 walkdir 自行探测环并报错；不跟随 = 默认 symlink 条目收编，
//!   `rel` 依 [`WalkedFile`] 语义）。目录条目本身收编为**零长目录条目**
//!   （压缩 `next_entry` 的 `is_dir` 分支）；目录**本体**不产自身条目
//!   （多输入时顶层目录按名字收编——`root/a` 的相对名为 `root/a`；
//!   02 §9.5「目录递归」语义 + CLI inputs 契约）。
//! - **文件输入**：单条目（`rel = file_name`）。
//! - **输入不存在** → `Err Io{NotFound}`（HZ-5004 语义；T01 ENOENT 归类）。
//!
//! # 过滤链顺序
//!
//! walkdir 产出 → ① glob 排除（`exclude_globs`，对 `rel` 逐 pattern 任一命中即
//! 剔除，目录命中即**剪枝子树**）→ ② 隐藏文件开关（`exclude_hidden`：默认
//! false——只按 glob；platform::metadata 的 hidden 语义：win 属性位 / unix
//! 点前缀，02 §5.3.2）。排除目录后其下文件不出现（TC-CLI-011 core 半场）。
//!
//! 顺序恒定：目录输入按 walkdir 深度优先（目录先于内容）+ 文件名排序
//! （`sort_by_file_name`）——roundtrip 与预期条目序断言的前提；同目录内多次
//! 展开顺序可复现。
//!
//! # 元数据
//!
//! 每条目携带 [`FileMeta`]（platform 统一模型：hidden/mtime/mode 位由
//! 消费方 [`crate::services::compress`] 映射进 `EntryMeta`）。
//!
//! 出处：02 §9.5 图 B/C 框、附录 B.1 `compress.*`、02 §5.3.2 元数据模型。

use std::io;
use std::path::{Path, PathBuf};

use hzip_error::HzipError;
use hzip_platform::metadata::{FileMeta, read_meta};
use walkdir::WalkDir;

/// 单条已收集输入（保留绝对路径 + 档案相对名 + 平台元数据）。
#[derive(Debug, Clone)]
pub(crate) struct WalkedFile {
    /// 绝对路径
    pub abs: PathBuf,
    /// 相对名（`/` 分隔；目录为合成名）
    pub rel: String,
    /// 平台统一元数据（mtime/hidden/executable…；02 §5.3.2）
    pub meta: FileMeta,
}

/// 过滤链参数（02 §9.5 B + 附录 B.1）。
#[derive(Debug, Clone, Default)]
pub(crate) struct InputFilters {
    /// `--exclude` 多 pattern（`"*.tmp"` 按段、`"sub/*"` 按路径前缀，任一命中剔除）
    pub exclude_globs: Vec<String>,
    /// 排除隐藏文件（默认 false——只按 glob；02 §9.5「三项并列」语义）
    pub exclude_hidden: bool,
    /// 链接策略（security.follow_symlinks；B.1 默认 false）
    pub follow_symlinks: bool,
}

/// 预统计（C 框 total 源头）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreStats {
    /// 条目数
    pub entries: u64,
    /// 总原始字节（普通文件 size_raw 求和；目录/链接 0）
    pub total_bytes: u64,
    /// 符号链接条目数
    pub symlinks: u64,
}

/// 展开 + 过滤（§9.5 B 框完整实现）。
///
/// `inputs`：一个或多个源（目录递归/文件单条）。全部展开后按过滤链剔除；
/// 结果为空时由调用方（compress）收敛为带语义的空报告/警告——本层不报错。
pub(crate) fn collect(inputs: &[PathBuf], f: &InputFilters) -> Result<Vec<WalkedFile>, HzipError> {
    let mut out = Vec::new();
    for input in inputs {
        collect_one(input, f, &mut out)?;
    }
    Ok(out)
}

fn collect_one(input: &Path, f: &InputFilters, out: &mut Vec<WalkedFile>) -> Result<(), HzipError> {
    let meta = std::fs::symlink_metadata(input).map_err(|e| not_found_err(input, e))?;
    if meta.is_dir() {
        // 目录：递归展开；过滤剪枝在 filter_entry（不深入已排除目录）。
        // 目录本体收编为自身条目 + 子条目带 `{dir}/` 前缀（7-Zip 同语义）：
        // 压 `src` → 档内 `src`、`src/a.txt`、`src/sub/`、`src/sub/b.txt`。
        let dname = input
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| input.display().to_string());
        let prefix = format!("{dname}/");
        // 前缀化 rel 的辅助（filter_entry 剪枝与条目 rel 共用）
        let prefixed =
            |abs: &Path| -> Option<String> { rel_of(input, abs).map(|r| format!("{prefix}{r}")) };
        let mut wd = WalkDir::new(input).sort_by_file_name().min_depth(0);
        if f.follow_symlinks {
            wd = wd.follow_links(true);
        }
        // 逐层过滤：命中 glob 的目录剪枝（含其下全部），命中 glob 的文件剔除
        let walker = wd.into_iter().filter_entry(|e| {
            if e.depth() == 0 {
                return true; // 根目录自身恒保留（递归载体，非条目）
            }
            let Some(rel) = prefixed(e.path()) else {
                return false;
            };
            !glob_hit(&rel, &f.exclude_globs)
        });
        for entry in walker {
            let entry = entry.map_err(|e| walkdir_err(input, e))?;
            if entry.depth() == 0 {
                // 根目录自身收编为目录条目（dir 根 → rel = 输入目录名）
                if (!f.exclude_hidden || !is_hidden(input)?) && !glob_hit(&dname, &f.exclude_globs)
                {
                    let fm = read_meta(input).unwrap_or_default();
                    out.push(WalkedFile {
                        abs: input.to_path_buf(),
                        rel: dname.clone(),
                        meta: fm,
                    });
                }
                continue;
            }
            let abs = entry.path().to_path_buf();
            let Some(rel) = prefixed(&abs) else {
                continue;
            };
            // 隐藏过滤（目录在 filter_entry 无法判隐藏——需 read_meta；此处补）
            if f.exclude_hidden && is_hidden(&abs)? {
                continue;
            }
            let ft = entry.file_type();
            if ft.is_symlink() {
                // 不跟随：链接条目收编（meta 读链接自身）
                out.push(walked_symlink(&abs, rel)?);
                continue;
            }
            if ft.is_dir() {
                let fm = read_meta(&abs).unwrap_or_default();
                out.push(WalkedFile { abs, rel, meta: fm });
                continue;
            }
            if ft.is_file() {
                let fm = read_meta(&abs).unwrap_or_default();
                out.push(WalkedFile { abs, rel, meta: fm });
            }
        }
        Ok(())
    } else {
        // 文件输入（含「文件型符号链接」输入本体——以 symlink_metadata 判定：
        // 链接指向文件时按链接语义收编，见 walked_symlink 分支）
        if meta.file_type().is_symlink() && !f.follow_symlinks {
            let abs = input.to_path_buf();
            let rel = input
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| input.display().to_string());
            out.push(walked_symlink(&abs, rel)?);
            return Ok(());
        }
        let rel = input
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| input.display().to_string());
        if f.exclude_hidden && is_hidden(input)? {
            return Ok(());
        }
        if glob_hit(&rel, &f.exclude_globs) {
            return Ok(());
        }
        let fm = read_meta(input).unwrap_or_default();
        out.push(WalkedFile {
            abs: input.to_path_buf(),
            rel,
            meta: fm,
        });
        Ok(())
    }
}

/// 符号链接条目构造：读链接目标（不跟随语义；悬空链接照样记录 target——
/// 解压侧按 T31 策略处理，02 §9.5 K/J3）。
fn walked_symlink(abs: &Path, rel: String) -> Result<WalkedFile, HzipError> {
    let meta = read_meta(abs).unwrap_or_default();
    Ok(WalkedFile {
        abs: abs.to_path_buf(),
        rel,
        meta,
    })
}

/// 相对名（目录根 → 子路径）；失败（越出根）→ None。
fn rel_of(root: &Path, p: &Path) -> Option<String> {
    p.strip_prefix(root)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

/// glob 排除命中（`*.tmp` 按段、`sub/*` 按子路径前缀，02 §9.5 注）。
///
/// 匹配对象：整 rel、rel 的**每个后缀子路径**（`a/b/c` → `a/b/c`、`b/c`、`c`）
/// 与每个单段。后缀子路径保证 `sub/*` 对带目录前缀的 rel
/// （`{dir}/sub/b.txt`）也命中——目录剪枝与文件剔除语义一致（v2 保目录名后
/// 条目带输入目录前缀，无此前缀的 pattern 须能命中深层路径）。
fn glob_hit(rel: &str, pats: &[String]) -> bool {
    if rel.is_empty() {
        return false;
    }
    pats.iter().any(|p| {
        let Ok(g) = glob::Pattern::new(p) else {
            return false;
        };
        if g.matches(rel) {
            return true;
        }
        let segs: Vec<&str> = rel.split('/').collect();
        // 后缀子路径（a/b/c → b/c、c）+ 单段
        for start in 0..segs.len() {
            let sub = segs[start..].join("/");
            if g.matches(&sub) {
                return true;
            }
        }
        false
    })
}

/// 隐藏判定（platform 元数据 hidden 语义；失败保守 false——不因读属性错误
/// 误杀文件）。
fn is_hidden(abs: &Path) -> Result<bool, HzipError> {
    Ok(read_meta(abs).map(|m| m.hidden).unwrap_or(false))
}

/// 源不存在（ENOENT → HZ-5004 语义；T01 归类：Io NotFound + path 上下文）。
fn not_found_err(input: &Path, e: io::Error) -> HzipError {
    HzipError::Io {
        source: e,
        path: Some(input.to_path_buf()),
    }
}

/// walkdir 遍历错误（IO 类：读目录权限等）。
fn walkdir_err(input: &Path, e: walkdir::Error) -> HzipError {
    let io_err = match e.io_error() {
        Some(ie) => {
            if let Some(c) = ie.raw_os_error() {
                io::Error::from_raw_os_error(c)
            } else {
                io::Error::new(ie.kind(), ie.to_string())
            }
        }
        None => io::Error::other(e.to_string()),
    };
    HzipError::Io {
        source: io_err,
        path: Some(input.to_path_buf()),
    }
}

/// 预统计（§9.5 C 框：一次遍历累加；size_raw 仅普通文件计——目录 0/链接 0）。
pub(crate) fn pre_stats(inputs: &[WalkedFile]) -> PreStats {
    let mut s = PreStats::default();
    for f in inputs {
        s.entries += 1;
        match std::fs::symlink_metadata(&f.abs) {
            Ok(md) if md.is_file() => s.total_bytes += md.len(),
            _ => {}
        }
    }
    s
}

#[cfg(test)]
#[path = "tests/walk.rs"]
mod tests;
