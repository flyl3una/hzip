//! 冲突决策器（02 §9.8；T32 独占）。
//!
//! 解压管道逐条目调用：目标路径（净化后）已存在时按策略决策。策略来自
//! B.1 `extract.conflict`（默认 `ask`），经 `hzip_format::ConflictPolicy`
//! 承载（02 §7.3，冻结）。
//!
//! # 五策略（02 §9.8 图 P 节点）
//!
//! | 策略 | 行为 |
//! |---|---|
//! | Overwrite | 直接覆盖（Windows 只读属性文件先清只读位，02 §5.3.2） |
//! | Skip | 跳过并记录 |
//! | NewerOnly | 源 mtime 更新才覆盖；源/现存任一 mtime 为 None → Skip（保守，规则②） |
//! | RenameAuto | 探测下一个可用名 `a (2).txt`（`{stem} ({n}){ext}`，n 从 2 起） |
//! | Ask | 经 `JobCtx.need(JobRequest::Conflict)` 上抛；超时/无应答 → 默认 Skip |
//!
//! Ask 应答（T30 冻结 `ConflictDecision`）：`Overwrite`/`Skip`/`RenameAuto`/
//! `Abort`。`Abort` → 任务级取消路径（`Cancelled`）。会话记忆（CLI/GUI
//! 「全部应用」）：首个非 Abort 决策缓存，后续冲突直接复用。
//!
//! # 大小写卷折叠（02 §9.8 A2/A3；TC-SEC-011）
//!
//! 大小写不敏感卷（Windows/macOS 默认）上 `A.txt` 与 `a.txt` 指向同一文件
//! ——存在性检查须按大小写折叠比较，命中即冲突。卷敏感性以
//! [`hzip_platform::fs_util::is_case_sensitive`] 实测（每任务缓存一次）。
//!
//! # RenameAuto 幂等（02 §9.8：`a→a (2)→a (3)`；TC-CORE-004）
//!
//! [`probe_next_free`]：n=2 起候选 `{stem} ({n}){ext}`，命中 `exists()` /
//! 大小写折叠冲突 / 本任务 in-flight 台账已分配 → n+1，首个空闲候选返回。
//! 探测与使用分离（不创建文件）；台账登记防**本任务内**并发窗口（多线程
//! 各条目分配零重名）。**跨任务/跨进程不承诺**——OS `create` 独占失败重探测
//! 一次兜底（rustdoc 注明，避免过度设计文件锁）。串行重复探测恒等（属性②）。
//!
//! 出处：02 §9.8 图、02 §7.3 `ConflictPolicy`、T30 `JobRequest::Conflict`/
//! `ConflictDecision` 冻结通道。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hzip_error::HzipError;
use hzip_format::{ConflictPolicy, EntryInfo};

use crate::jobs::{ConflictDecision, FileBrief, JobCtx, JobRequest, JobResponse};
use crate::security::SanitizedEntry;

/// 冲突决策动作（管道按结果执行；02 §9.8 表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConflictAction {
    /// 覆盖（管道负责清只读位后写）。
    Overwrite,
    /// 跳过本条目（管道记录 skipped）。
    Skip,
    /// 改名到新目标（RenameAuto 探测结果）。
    RenameTo(PathBuf),
    /// 中止任务（Ask → Abort → 取消路径）。
    Abort,
}

/// in-flight 台账（本任务内已分配的目标路径；RenameAuto 并发互斥）。
///
/// **幂等并发安全**：探测与登记在同一临界区内原子完成（`allocate`）——多线程
/// 各自调用不可能拿到同一候选（TC-CORE-004 proptest 属性 ①）。作用域 = 单任务
/// 内；跨任务/跨进程不承诺（OS create 独占失败重探测兜底，见模块文档）。
#[derive(Debug, Default)]
pub(crate) struct AllocatedNames {
    map: Mutex<HashMap<PathBuf, ()>>,
}

impl AllocatedNames {
    /// 空台账。
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 原子分配：n=2 起探测候选，命中 `exists`（含折叠回调）或台账已占 → n+1；
    /// 首个空闲候选**在锁内登记**并返回（不创建文件）。
    pub(crate) fn allocate(&self, base: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
        let mut map = self.map.lock().unwrap_or_else(|x| x.into_inner());
        let mut n = 2u32;
        loop {
            let cand = insert_suffix(base, n);
            if !(exists(&cand) || map.contains_key(&cand)) {
                map.insert(cand.clone(), ());
                return cand;
            }
            n = n.saturating_add(1);
        }
    }
}

/// 冲突决策器（02 §9.8；每任务一个实例）。
pub(crate) struct ConflictResolver<'a> {
    policy: ConflictPolicy,
    ctx: &'a JobCtx,
    /// 卷敏感性缓存（每任务一次实测；None = 未测定）。
    case_sensitive: Option<bool>,
    /// 会话级记忆（「全部应用」：首个非 Abort Ask 决策缓存）。
    session: Option<ConflictDecision>,
    /// 本任务已分配目标台账（RenameAuto）。
    pub allocated: AllocatedNames,
}

impl<'a> ConflictResolver<'a> {
    /// 新建。`case_sensitive`：管道预检已实测则传入（避免重复探测）。
    pub(crate) fn new(
        policy: ConflictPolicy,
        ctx: &'a JobCtx,
        case_sensitive: Option<bool>,
    ) -> Self {
        Self {
            policy,
            ctx,
            case_sensitive,
            session: None,
            allocated: AllocatedNames::new(),
        }
    }

    /// 卷敏感性（每任务一次实测缓存；02 §5.3.4）。
    fn case_sensitive(&mut self, dest: &Path) -> bool {
        if let Some(v) = self.case_sensitive {
            return v;
        }
        let v = hzip_platform::fs_util::is_case_sensitive(dest);
        self.case_sensitive = Some(v);
        v
    }

    /// 大小写折叠存在性检查（02 §9.8 A2/A3）。
    ///
    /// 敏感卷 → `path.exists()`；不敏感卷 → 扫描父目录，文件名按
    /// `eq_ignore_ascii_case` 折叠比较（目录条目不深入；父目录缺失 → false）。
    pub(crate) fn case_fold_exists(&mut self, san: &SanitizedEntry) -> Result<bool, HzipError> {
        let path = &san.dest_path;
        if self.case_sensitive(path) {
            return Ok(path.exists());
        }
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => return Ok(path.exists()),
        };
        let Some(base) = path.file_name() else {
            return Ok(path.exists());
        };
        let base_l = base.to_string_lossy().to_lowercase();
        match std::fs::read_dir(&parent) {
            Ok(rd) => Ok(rd
                .flatten()
                .any(|ent| ent.file_name().to_string_lossy().to_lowercase() == base_l)),
            Err(_) => Ok(false), // 父目录不存在 → 无既有文件可比
        }
    }

    /// 决策入口（02 §9.8 P 节点；逐策略分派）。
    ///
    /// 调用前提：`san` 目标已确认存在（`exists()` 或折叠命中）。
    /// `src` 提供 mtime（NewerOnly）与条目名（Ask 摘要）。
    pub(crate) fn decide(
        &mut self,
        san: &SanitizedEntry,
        src: &EntryInfo,
    ) -> Result<ConflictAction, HzipError> {
        match self.policy {
            ConflictPolicy::Overwrite => Ok(ConflictAction::Overwrite),
            ConflictPolicy::Skip => Ok(ConflictAction::Skip),
            ConflictPolicy::NewerOnly => {
                let src_mtime = src.mtime.map(|t| t.unix_timestamp());
                let dst_mtime = std::fs::metadata(&san.dest_path).ok().and_then(|m| {
                    m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                });
                match (src_mtime, dst_mtime) {
                    (Some(s), Some(d)) if s > d => Ok(ConflictAction::Overwrite),
                    // None → Skip（保守：无法比较即不覆盖，02 §9.8 规则②）
                    _ => Ok(ConflictAction::Skip),
                }
            }
            ConflictPolicy::RenameAuto => {
                let next = self.probe_rename(&san.dest_path);
                Ok(ConflictAction::RenameTo(next))
            }
            ConflictPolicy::Ask => self.decide_ask(san, src),
            // #[non_exhaustive]（T20 冻结）：未知策略默认最安全语义 = 跳过
            _ => Ok(ConflictAction::Skip),
        }
    }

    /// RenameAuto 原子分配（台账锁内探测+登记；单任务内并发互斥）。
    fn probe_rename(&mut self, path: &Path) -> PathBuf {
        let case_sensitive = self.case_sensitive(path);
        let allocated = &self.allocated;
        allocated.allocate(path, |p| {
            p.exists() || (!case_sensitive && fold_exists_dir(p))
        })
    }

    /// Ask 分支（02 §9.8 P→Ask；会话记忆 + 超时默认 Skip + Abort 取消）。
    fn decide_ask(
        &mut self,
        san: &SanitizedEntry,
        src: &EntryInfo,
    ) -> Result<ConflictAction, HzipError> {
        // 会话记忆（全部应用）命中直接复用
        if let Some(decision) = self.session {
            return Ok(self.action_from_decision(decision, san));
        }
        let existing = existing_brief(&san.dest_path, src);
        match self.ctx.need(JobRequest::Conflict {
            path: san.dest_path.clone(),
            existing,
        }) {
            Ok(JobResponse::Conflict(decision)) => {
                if decision != ConflictDecision::Abort {
                    self.session = Some(decision); // Abort 一次性，不缓存
                }
                Ok(self.action_from_decision(decision, san))
            }
            // 协议错配（应答了 Password 等）→ 保守 Skip
            Ok(JobResponse::Password(_)) => Ok(ConflictAction::Skip),
            // 决策超时（5002）/ 通道断 → 默认 Skip（02 §9.8 括号注；无 TTY 时
            // 不应阻塞任务——跳过冲突条目继续）
            Err(e) if e.code() == 5002 => Ok(ConflictAction::Skip),
            // 任务取消（6001）→ 上抛 Cancelled（用户取消优先于默认 Skip——
            // 任务整体取消路径，02 §4.2）
            Err(e) if e.code() == 6001 => Err(e),
            Err(e) => Err(e),
        }
    }

    /// 决策 → 动作。RenameAuto 决策经探测分配新名（与策略同幂等链）。
    fn action_from_decision(
        &mut self,
        decision: ConflictDecision,
        san: &SanitizedEntry,
    ) -> ConflictAction {
        match decision {
            ConflictDecision::Overwrite => ConflictAction::Overwrite,
            ConflictDecision::Skip => ConflictAction::Skip,
            ConflictDecision::RenameAuto => {
                let next = self.probe_rename(&san.dest_path);
                ConflictAction::RenameTo(next)
            }
            ConflictDecision::Abort => ConflictAction::Abort,
        }
    }
}

/// 现存文件简要（Ask 请求携带）：读真实落盘目标元数据；元数据不可得时以
/// 源条目信息兜底（目录大小 0/mtime None）。
fn existing_brief(dest_path: &Path, src: &EntryInfo) -> FileBrief {
    let md = std::fs::metadata(dest_path);
    match md {
        Ok(m) => FileBrief {
            is_dir: m.is_dir(),
            size: if m.is_dir() { 0 } else { m.len() },
            mtime: m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64),
        },
        Err(_) => FileBrief::from_entry(src),
    }
}

/// 折叠目录扫描（无卷缓存时用；p 的父目录中是否有大小写变体命中）。
fn fold_exists_dir(p: &Path) -> bool {
    let Some(parent) = p.parent() else {
        return false;
    };
    let Some(base) = p.file_name() else {
        return false;
    };
    let base_l = base.to_string_lossy().to_lowercase();
    match std::fs::read_dir(parent) {
        Ok(rd) => rd
            .flatten()
            .any(|ent| ent.file_name().to_string_lossy().to_lowercase() == base_l),
        Err(_) => false,
    }
}

/// `{stem} ({n}){ext}` 命名（02 §9.8：`a.txt` → `a (2).txt`；无扩展名 `a` →
/// `a (2)`；点开头文件 `.env` → `.env (2)`——扩展名界定 = 文件名首点之后的
/// 最后一段，点开头文件视为无扩展名）。
pub(crate) fn insert_suffix(p: &Path, n: u32) -> PathBuf {
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = split_ext(&name);
    let new_name = match ext {
        Some(e) => format!("{stem} ({n}).{e}"),
        None => format!("{stem} ({n})"),
    };
    match p.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(new_name),
        _ => PathBuf::from(new_name),
    }
}

/// 文件名 → (stem, 扩展名)。`a.txt` → `("a", Some("txt"))`；`a` → `("a", None)`；
/// `.env` → `(".env", None)`（点开头）；`a.tar.gz` → `("a.tar", Some("gz"))`。
fn split_ext(name: &str) -> (&str, Option<&str>) {
    if name.starts_with('.') {
        return (name, None);
    }
    match name.rfind('.') {
        Some(idx) if idx > 0 => (&name[..idx], Some(&name[idx + 1..])),
        _ => (name, None),
    }
}

#[cfg(test)]
#[path = "tests/conflict.rs"]
mod tests;

/// proptest 属性测试（TC-CORE-004 三属性：重名互斥/幂等/格式规范）。
#[cfg(test)]
#[path = "tests/props.rs"]
mod props;
