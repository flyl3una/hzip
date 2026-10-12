//! 解压服务完整管道（02 §9.4 流程图逐框；T32 独占）+ test 服务薄包装。
//!
//! # 管道（§9.4 图逐框）
//!
//! ```text
//! A JobKind::Extract → B sniff → C open_reader（registry 委托）
//!   → D 预检（目标盘可用 ≥ Σsize_raw × SPACE_HEADROOM 1.05，不足 Err 1002）
//!   → E smart_extract 判定 → 实际目标目录（Direct/Wrap；默认保真原样）
//!   → F 遍历 Selection 条目
//!     → G sanitize_entry_path 净化（T31 红线函数）
//!     → H 符号链接? → L1 SymlinkGuard 决策分支
//!     → I 目标已存在? → L2 冲突决策器
//!     → J 创建父目录链
//!     → K 流拷贝 read→write 1 MiB/次（COPY_CHUNK_BYTES）
//!       → K1 检查点每 4 MiB：QuotaGuard::checkpoint（T31）+ 每 chunk 取消检查
//!       → K2 条目完（CRC 由 reader 流内校验，见下）→ K3 best_effort 分支
//!   → R 聚合 ExtractReport → Outcome::Done / Partial
//! ```
//!
//! # 关键接线语义（T32 任务卡 §3.1）
//!
//! - **预检 D 在 smart 判定 E 之前**：Selection 范围内 `Σ size_raw × 1.05` 判缺盘
//!   （dest_root 最近存在祖先的 `available_bytes`；TC-SEC-014 core 半场）；
//! - **冲突 I 在净化 G 之后**：以净化后真实落盘路径判存在性（含大小写折叠）；
//! - **目录条目合并语义**（rustdoc 注明）：目录条目路径已存在且为目录 → 直接
//!   合并（`create_dir_all` 幂等，不触发冲突决策——与 7-Zip/Bandizip 一致，避免
//!   重解压到既有目录时对目录层轰炸式 Ask）；已存在为**文件** → 冲突决策器
//!   正常参与（Overwrite 先删文件再建目录；Skip/改名按决策；子条目落盘由
//!   `create_parent_chain` 兜底）。
//! - **K1**：`QuotaGuard::checkpoint`（4 MiB 门控 + 4501→4502→4503）+ 每 chunk
//!   `ctx.cancel.throw_if_cancelled`（02 §4.2）；命中 → 删除本条目半写文件
//!   （K1 QE/QC 回滚本条目；已完整条目保留于 written 台账）；
//! - **K2 失败分类**：`Checksum`/`Password`/`Format` 可累积；`Security`/`Quota`
//!   **无论 best_effort 一律 fail-fast**（安全语义不可「尽力而为」）；
//! - **K3**：best_effort 且可累积类 → 记 failures 继续；否则 fail-fast 原样上抛
//!   （不包 Partial——Partial 只描述「继续执行后的部分失败」，02 §3.5）；
//! - **R**：failures 空 → `Outcome::Done(stats)`；非空 → `Outcome::Partial(stats,
//!   failures)`（T30 JobManager 收敛 PartialDone；失败明细携带真实条目名）。
//!
//! # CRC 校验（K2 框）接线说明（rustdoc）
//!
//! 02 §7.3 契约红线 3：crc32 已知格式（zip）在 `read_entry` **流内增量校验**，
//! CRC 不符以 `Err Checksum` 在读取端报出（核心层拿不到未校验字节）；tar/single
//! `crc32` 恒 `None`（格式无该域）。故 K2 复核由 reader 侧承担、本层信任
//! （zip CRC 坏样本测试仍以 `Err Checksum` 到达 K3 分支——TC-CORE-007/008 半场）。
//! 若未来某格式 read_entry 不流内校验，K2 补 core 复核点。
//!
//! # 配额 packed 近似（rustdoc 注明，T32 §4.6）
//!
//! 多数格式 `read_entry` 返回解码后流，packed 逐 chunk 不可得——以
//! `size_packed × n / size_raw` 线性插值；`size_raw == 0`（single 流式未知）以
//! packed=raw 代理防 ratio ∞ 误伤（4501/4502 精确按 raw；4503 对 single 不适用）。
//! 条目尺度 4504 由 T31 `entry_start` 复核。
//!
//! # 出处
//!
//! 02 §9.4 图、§7.3 `ArchiveReader`/`ExtractCtx`、§3.5 边界规则、§4.2 取消传播、
//! 附录 B.1 `extract.*`、T31 security、T30 `JobCtx`/`JobRequest` 冻结通道。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use hzip_error::HzipError;
use hzip_format::{
    ArchiveReader, ConflictPolicy, Encoding, EntryId, EntryInfo, ExtractReport, FormatId,
    PasswordRef, PasswordSource, QuotaLimits, SanitizeConfig, Selection,
};
use hzip_progress::{JobStats, Outcome, ProgressEvent};
use zeroize::Zeroizing;

use crate::conflict::{ConflictAction, ConflictResolver};
use crate::consts::SPACE_HEADROOM;
use crate::encoding::{self, EncodingPolicy};
use crate::jobs::{JobCtx, JobRequest, JobResponse, PasswordReason};
use crate::security::{LinkAction, QuotaGuard, SanitizedEntry, SymlinkGuard, sanitize_entry_path};
use crate::smart_extract::{self, strip_suffixes};

/// 密码重试上限（B.2 `PASSWORD_MAX_RETRIES=3` 的 core 镜像；超限收敛 Password 错）。
/// 编辑服务（[`crate::services::edit`]）密码询问链同口径共用。
pub(crate) const PASSWORD_MAX_RETRIES: u32 = 3;

// ===================== 公开解压选项 =====================

/// 解压任务选项（`JobSpec.extract` 载体；02 §7.3 ExtractCtx 字段 + B.1 extract.*）。
///
/// 字段集「非冻结」——T32~T34 追加 services 所需参数。`Default` = 全默认
/// （`selection=All`、`conflict=Ask`、`smart=false` 恒原样、`best_effort=false`、
/// `sanitize`/`quota` 默认、`codepage=None`、`dest_root=None` → 源所在目录）。
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// 解压目标根（None → 源档案所在目录）。目标根不存在时**自动创建**。
    pub dest_root: Option<PathBuf>,
    /// 选择范围（CLI `--select`；默认全部）。
    pub selection: Selection,
    /// 冲突策略（02 §9.8；默认 Ask）。
    pub conflict: ConflictPolicy,
    /// `--smart` 优雅解压（v2；默认 false = 恒原样直展，不剥不包）。
    /// true = 自动判别：多顶层/散文件（会污染目标根）→ 包档案 stem 壳目录；
    /// 单顶层目录/单文件 → 原样直展。旧 §9.7「单顶层剥壳」已移除。
    pub smart: bool,
    /// best-effort 模式（02 §3.5；默认 false = fail-fast）。
    pub best_effort: bool,
    /// 净化策略配置（T31 消费；默认 DenyEscape）。
    pub sanitize: SanitizeConfig,
    /// 配额限制（默认 32 GiB/128 GiB/200/1e6，02 §7.3 QuotaLimits::default）。
    pub quota: QuotaLimits,
    /// 手动指定文件名编码（02 §8.1 B0；默认 None = 自动检测）。
    pub codepage: Option<Encoding>,
    /// 解压密码（可选）：**首个候选**——core 先用它打开/预检，成功即用；未填或
    /// 不正确则回落到现有询问链（`ctx.need(JobRequest::Password)`）。与
    /// `CompressSpec.password` 同口径（`Zeroizing` 持有，不落日志）。
    pub password: Option<Zeroizing<String>>,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            dest_root: None,
            selection: Selection::All,
            conflict: ConflictPolicy::Ask,
            smart: false,
            best_effort: false,
            sanitize: SanitizeConfig::default(),
            quota: QuotaLimits::default(),
            codepage: None,
            password: None,
        }
    }
}

impl ExtractOptions {
    /// 从 JobSpec 提取（`extract: None` → 默认）。
    pub(crate) fn from_job(spec: &crate::jobs::JobSpec) -> Self {
        spec.extract.clone().unwrap_or_default()
    }

    /// 解析实际解压根（dest_root 优先，其次 JobSpec.dest 兼容回退，最后源目录）。
    fn effective_root(&self, spec: &crate::jobs::JobSpec) -> PathBuf {
        if let Some(root) = &self.dest_root {
            return root.clone();
        }
        if let Some(d) = &spec.dest {
            return d.clone();
        }
        spec.source
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

// ===================== 服务入口（services::execute 分派目标） =====================

/// JobKind::Extract 实现体（02 §9.4 全管道）。
pub(crate) fn run_extract(ctx: &JobCtx, spec: &crate::jobs::JobSpec) -> Outcome {
    match extract_impl(ctx, spec) {
        Ok(outcome) => outcome,
        Err(HzipError::Cancelled) => Outcome::Cancelled,
        Err(e) => Outcome::Failed(e),
    }
}

/// JobKind::Test 实现体（02 §7.3 `ArchiveReader::test` 薄包装；无 dest/净化/冲突）。
pub(crate) fn run_test(ctx: &JobCtx, spec: &crate::jobs::JobSpec) -> Outcome {
    let started = Instant::now();
    // 头加密档案：打开期问密码重开（JobRequest 链；任何进度事件之前）
    let seed_pw = ExtractOptions::from_job(spec).password;
    let reader = match open_archive_retry(ctx, &spec.source, seed_pw.as_ref()) {
        Ok(r) => r,
        Err(e) => return Outcome::Failed(e),
    };
    let pw = PwRouter::new(ctx);
    let cancel = ctx.cancel.clone();
    let ectx = hzip_format::ExtractCtx {
        cancel: &cancel,
        progress: ctx.progress(),
        passwords: &pw,
        conflicts: ConflictPolicy::Overwrite,
        sanitize: &SanitizeConfig::default(),
        quota: &QuotaLimits::default(),
        best_effort: false,
        codepage: None,
    };
    match reader.test(&ectx) {
        Ok(report) => {
            let mut stats = JobStats::new(report.bytes_written, report.entries_done as u64);
            stats.entries_done = report.entries_done as u64;
            stats.bytes_done = report.bytes_written;
            crate::report::stamp_stats(&mut stats, started);
            Outcome::Done(stats)
        }
        Err(HzipError::Cancelled) => Outcome::Cancelled,
        Err(e) => Outcome::Failed(e),
    }
}

/// 管道主体（02 §9.4 A→R 全框；返回已收敛 Outcome）。
fn extract_impl(ctx: &JobCtx, spec: &crate::jobs::JobSpec) -> Result<Outcome, HzipError> {
    // 任务实际开始时刻（终局 stats 回填用，见 report::stamp_stats）
    let started = Instant::now();
    let opts = ExtractOptions::from_job(spec);
    // 解压入口（selection 概要；密码/敏感内容不落日志）
    tracing::info!(
        job = ctx.job_id,
        src = %spec.source.display(),
        dest = ?opts.dest_root.as_deref().map(|p| p.display().to_string()),
        smart = opts.smart,
        "解压任务开始"
    );
    // B→C：registry sniff + open_reader（组合链 registry 内装配；02 §7.6）。
    // 头加密（HZ-3102）在打开期经 JobRequest::Password 问密码重开——
    // 任何进度事件之前完成（GUI 密码弹窗先于进度窗，设计稿 11 §4.2）。
    let __t = std::time::Instant::now();
    let reader = match open_archive_retry(ctx, &spec.source, opts.password.as_ref()) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(job = ctx.job_id, src = %spec.source.display(), err = %e, "解压打开档案失败");
            return Err(e);
        }
    };
    tracing::debug!(
        __probe = "open",
        ms = __t.elapsed().as_millis() as u64,
        "解压相位计时"
    );
    let __t = std::time::Instant::now();
    // C2：entries（红线 1：不读数据区；头加密 → Password{Header} 上抛）
    let entries = match reader.entries() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(job = ctx.job_id, src = %spec.source.display(), err = %e, "解压读取条目失败");
            return Err(e);
        }
    };
    // 名字一致性层（§8.1 core 半场；当前形态以保留标注为主——见 encoding 模块）
    let policy = EncodingPolicy::new(opts.codepage);
    let (entries, _encoding_outcome) = encoding::reconcile_names(entries, &policy);
    tracing::debug!(
        __probe = "entries+reconcile",
        ms = __t.elapsed().as_millis() as u64,
        "解压相位计时"
    );
    let __t = std::time::Instant::now();
    // 预统计（Selection 解析）
    let selected_ids = match resolve_selection(&entries, &opts.selection) {
        Ok(ids) => ids,
        Err(e) => {
            tracing::error!(job = ctx.job_id, err = %e, "解压选择集解析失败");
            return Err(e);
        }
    };
    if selected_ids.is_empty() {
        tracing::info!(job = ctx.job_id, "解压选择集为空，直接完成");
        return Ok(Outcome::Done(JobStats::new(0, 0)));
    }
    // 加密预热 + 有界预检（设计稿 11 §3.5/§4.2 落地修订）：所选含加密条目 →
    // 在首个 EntryStarted 之前经 ctx.need 取档密码并真读一个有界加密条目
    // （read_entry 在流交付前完成密码验证；>64 MiB 无有界条目 → 采纳不验，
    // 降级为进度中 WrongPassword 重问链）。密码弹窗因此严格先于进度窗；
    // 错密码在弹窗内重问（≤3 次）后才进入管道。
    tracing::debug!(
        __probe = "selection",
        ms = __t.elapsed().as_millis() as u64,
        "解压相位计时"
    );
    let __t = std::time::Instant::now();
    let early_pw = resolve_early_password(
        ctx,
        &entries,
        &selected_ids,
        reader.as_ref(),
        opts.password.as_ref(),
    )?;
    tracing::debug!(
        __probe = "early_pw",
        ms = __t.elapsed().as_millis() as u64,
        "解压相位计时"
    );
    let __t = std::time::Instant::now();
    // D：预检——目标盘空间 ≥ Σ size_raw × 1.05（02 §9.4 / TC-SEC-014 core 半场）
    let dest_root = opts.effective_root(spec);
    if let Err(e) = precheck_disk(&entries, &selected_ids, &dest_root) {
        tracing::error!(job = ctx.job_id, dest = %dest_root.display(), err = %e, "解压预检失败（磁盘余量）");
        return Err(e);
    }
    // 总量补报（GUI 进度条 / CLI 百分比数据源）：JobStarted 骨架期 total=0，
    // 选择集解析完成后此处补发真实 Σ size_raw + 条目数——进度 0%→100% 跳变的根因修复。
    let (total_bytes, total_entries) = selected_ids
        .iter()
        .filter_map(|&id| entries.get(id))
        .fold((0u64, 0u64), |(b, n), e| {
            (b.saturating_add(e.size_raw), n + 1)
        });
    ctx.progress().send(ProgressEvent::JobTotal {
        job_id: ctx.job_id,
        total_bytes,
        total_entries,
    });
    // E：smart_extract 判定 → 实际目标目录（Direct/Wrap）+ 可剥前缀（恒 None）。
    //    目标根不存在 → 自动创建（用户 -o 语义；缺省源目录必存在）。
    if !dest_root.exists() {
        std::fs::create_dir_all(&dest_root).map_err(|e| {
            tracing::error!(job = ctx.job_id, dest = %dest_root.display(), err = %e, "解压目标目录创建失败");
            HzipError::Io {
                source: e,
                path: Some(dest_root.clone()),
            }
        })?;
    }
    let archive_stem = archive_stem_of(&spec.source);
    let (dest, strip_top) =
        smart_extract::resolve_dest(&entries, &dest_root, Some(&archive_stem), opts.smart)?;
    tracing::debug!(
        __probe = "precheck+total+resolve_dest",
        ms = __t.elapsed().as_millis() as u64,
        "解压相位计时"
    );
    let __t = std::time::Instant::now();
    // 卷敏感性（每任务一次实测；冲突决策器缓存复用）
    let case_sensitive = hzip_platform::fs_util::is_case_sensitive(&dest);
    tracing::debug!(
        __probe = "case_sensitive",
        ms = __t.elapsed().as_millis() as u64,
        "解压相位计时"
    );
    // F~K3 + R
    let mut pipeline = ExtractPipeline::new(ctx, &opts, dest, strip_top, case_sensitive, early_pw);
    let extract_result = pipeline.extract_selection(reader.as_ref(), &selected_ids, &entries);
    match extract_result {
        Ok(report) => {
            tracing::info!(
                job = ctx.job_id,
                entries = report.entries_done,
                failed = report.failures.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "解压任务完成"
            );
            Ok(outcome_of_report(report, &entries, started))
        }
        Err(e) => {
            // 中止原因留痕（配额/密码取消/写失败等；条目名不涉密）
            tracing::error!(
                job = ctx.job_id,
                done = pipeline.done,
                elapsed_ms = started.elapsed().as_millis() as u64,
                err = %e,
                "解压任务中止"
            );
            Err(e)
        }
    }
}

/// 预检（02 §9.4 D 框；TC-SEC-014 core 半场）。
///
/// `need = Σ size_raw（Selection 范围）× SPACE_HEADROOM(1.05)`；可用空间取
/// dest_root 最近存在祖先。不足 → `Err Io{ENOSPC 语义}`（`code()` → io_code →
/// 1002，UserAction FreeSpaceOrChangeTarget）。`need == 0`（空选择/全目录）跳过。
fn precheck_disk(
    entries: &[EntryInfo],
    ids: &[EntryId],
    dest_root: &Path,
) -> Result<(), HzipError> {
    let need: u64 = ids
        .iter()
        .filter_map(|&id| entries.get(id))
        .map(|e| e.size_raw)
        .sum();
    if need == 0 {
        return Ok(());
    }
    let need_headroom = (need as f64 * SPACE_HEADROOM) as u64;
    let Some(probe_dir) = nearest_existing(dest_root) else {
        return Err(HzipError::Io {
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "dest 根不可达"),
            path: Some(dest_root.to_path_buf()),
        });
    };
    let avail = hzip_platform::fs_util::available_bytes(&probe_dir).map_err(HzipError::from)?;
    if avail < need_headroom {
        return Err(crate::services::space::disk_full_error(Some(
            dest_root.to_path_buf(),
        )));
    }
    Ok(())
}

/// dest_root 自身或最近存在祖先（可用空间探测目标）。
fn nearest_existing(p: &Path) -> Option<PathBuf> {
    let mut cur = p.to_path_buf();
    loop {
        if cur.exists() {
            return Some(cur);
        }
        match cur.parent() {
            Some(parent) if parent != cur => cur = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// 档案 stem（Wrap 目录名）：剥全部已知**档案**后缀（`a.tar.gz → a`、`a.tgz → a`）。
///
/// 后缀清单取自 `hzip_format::exts`（唯一扩展名事实来源：`FORMAT_EXTS`）而非本地
/// 小抄——旧实现只列 `["gz","bz2","xz","zst","tar","cpio","zip","7z","rar"]`，
/// **漏了别名** `.tgz/.tbz2/.txz/...`，于是 `bh_orm_service.itr2.x86_64.tgz` 的
/// stem 仍是全名，Wrap 壳目录与档案**同名撞车** ⇒ `create_dir_all` 报
/// 「当文件已存在时，无法创建该文件」(os 183) ⇒ 用户看到 HZ-1001「目标不可写」
/// （实测用户右键「智能解压」报此错、磁盘却充裕）。
///
/// 剥到无可剥为止（`a.tar.gz` 两轮）；纯扩展名（`.gz`）保持原名不剥空。
fn archive_stem_of(source: &Path) -> String {
    let name = source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    let exts: Vec<&str> = hzip_format::exts::all_exts().collect();
    strip_suffixes(&name, &exts)
}

/// Selection 解析（All / Ids 越界 Err / Names 匹配）。
fn resolve_selection(entries: &[EntryInfo], sel: &Selection) -> Result<Vec<EntryId>, HzipError> {
    match sel {
        Selection::All => Ok((0..entries.len()).collect()),
        Selection::Ids(ids) => {
            if ids.iter().any(|&i| i >= entries.len()) {
                return Err(HzipError::Format {
                    format: FormatId::from("?"),
                    kind: hzip_error::FormatErrorKind::BadHeader,
                });
            }
            Ok(ids.clone())
        }
        Selection::Names(names) => Ok(entries
            .iter()
            .filter(|e| names.iter().any(|n| n.as_str() == e.name.as_str()))
            .map(|e| e.id)
            .collect()),
        // #[non_exhaustive]（T20 冻结）
        _ => Err(HzipError::Unsupported {
            what: "未知 Selection 变体".into(),
        }),
    }
}

/// 报告 → Outcome（02 §3.5 二态收敛；Partial 失败明细携带真实条目名）。
/// stats 回填实际开始时刻（report 聚合新建 → 耗时恒 0 ms，见
/// [`crate::report::stamp_stats`]）。
fn outcome_of_report(report: ExtractReport, entries: &[EntryInfo], started: Instant) -> Outcome {
    let mut stats = JobStats::new(report.bytes_written, report.entries_done as u64);
    stats.entries_done = report.entries_done as u64;
    stats.bytes_done = report.bytes_written;
    crate::report::stamp_stats(&mut stats, started);
    if report.failures.is_empty() {
        Outcome::Done(stats)
    } else {
        let failed = report
            .failures
            .into_iter()
            .filter_map(|(id, e)| {
                entries.get(id).map(|info| hzip_progress::EntryFailure {
                    entry: info.name.as_str().into(),
                    error: e,
                })
            })
            .collect();
        Outcome::Partial(stats, failed)
    }
}

// ===================== 管道执行器 =====================

/// §9.4 F~K3 主循环状态。
struct ExtractPipeline<'a> {
    ctx: &'a JobCtx,
    opts: &'a ExtractOptions,
    dest: PathBuf,
    strip_top: Option<String>,
    quota: QuotaGuard,
    symlinks: SymlinkGuard,
    conflicts: ConflictResolver<'a>,
    /// 本任务已完整落盘路径台账
    written: Vec<PathBuf>,
    /// best_effort 逐条目失败（K3→SK 累积）
    failures: Vec<(EntryId, Box<HzipError>)>,
    done: usize,
    bytes_written: u64,
    /// 目录 mtime 延后恢复（tar 目录条目先于子条目出现——先建后写会让子条目
    /// 更新父目录 mtime；结束时统一恢复，bsdtar 同语义）。
    dir_times: Vec<(PathBuf, time::OffsetDateTime)>,
    /// 预热阶段取得并（有界时）验证过的档密码：管道内 `password_for` 直接
    /// 复用，不再二次弹窗；错密码已在预热/预检阶段拦截。
    early_pw: Option<Zeroizing<String>>,
}

impl<'a> ExtractPipeline<'a> {
    fn new(
        ctx: &'a JobCtx,
        opts: &'a ExtractOptions,
        dest: PathBuf,
        strip_top: Option<String>,
        case_sensitive: bool,
        early_pw: Option<Zeroizing<String>>,
    ) -> Self {
        let conflicts = ConflictResolver::new(opts.conflict, ctx, Some(case_sensitive));
        Self {
            ctx,
            opts,
            dest,
            strip_top,
            quota: QuotaGuard::new(opts.quota),
            symlinks: SymlinkGuard::new(opts.sanitize.symlink),
            conflicts,
            written: Vec::new(),
            failures: Vec::new(),
            done: 0,
            bytes_written: 0,
            dir_times: Vec::new(),
            early_pw,
        }
    }

    /// F→R：遍历 Selection 条目逐条 extract。
    fn extract_selection(
        &mut self,
        reader: &dyn ArchiveReader,
        ids: &[EntryId],
        entries: &[EntryInfo],
    ) -> Result<ExtractReport, HzipError> {
        for &id in ids {
            let Some(info) = entries.get(id) else {
                continue;
            };
            // 4504 条目数检查点（T31 entry_start 不等 4 MiB；Quota 类不论
            // best_effort 一律 fail-fast）
            self.quota.entry_start()?;
            match self.extract_one(reader, info) {
                Ok(()) => {}
                Err(e) if self.opts.best_effort && best_effort_eligible(&e) => {
                    // K3 是·失败 → SK：记录失败 继续
                    self.failures.push((id, Box::new(e)));
                }
                // skip 策略档的逃逸链接：策略表第二行「跳过」语义（02 §9.6）
                Err(e) if is_skip_symlink(&e, self.opts.sanitize.symlink) => {
                    self.failures.push((id, Box::new(e)));
                }
                Err(e) => return Err(e), // K3 否·失败 → FF（含全部安全/配额类）
            }
        }
        // 目录 mtime 延后统一恢复（子条目写入不再覆盖父目录时间）
        for (path, mtime) in std::mem::take(&mut self.dir_times) {
            let _ = filetime_apply(&path, Some(mtime));
        }
        Ok(self.build_report())
    }

    /// 单条目（02 §9.4 G~K2 逐框）。
    fn extract_one(
        &mut self,
        reader: &dyn ArchiveReader,
        info: &EntryInfo,
    ) -> Result<(), HzipError> {
        // 名字层应用可剥前缀（Bare 单顶层目录：root/a → a；02 §9.7 B 节点）
        let rel_name = smart_extract::apply_strip(info.name.as_str(), self.strip_top.as_deref());
        let entry_name = hzip_format::EntryName::new(rel_name);
        // G：sanitize_entry_path 净化（T31 红线函数；4001/4003/4004 → Security 类
        // 无论 best_effort 一律 fail-fast）
        let san = sanitize_entry_path(&entry_name, &self.dest, &self.opts.sanitize)?;
        // 符号链接组合攻击防线（T31 策略表第三行；不可配置）
        self.symlinks.check_under_linked(&san.rel)?;
        // H：符号链接条目 → L1 分支（tar 目标在元数据；zip 无目标字段——目标文本
        // 即数据，回落普通数据写）
        if info.is_symlink
            && info
                .symlink_target
                .as_deref()
                .map(|t| !t.as_os_str().is_empty())
                .unwrap_or(false)
        {
            return self.write_symlink(info, &san);
        }
        // 目录条目（合并语义见模块文档：已存在目录直接合并；已存在文件走冲突）
        if info.is_dir {
            return self.write_dir(info, &san);
        }
        // I：目标已存在（真 exists + 大小写折叠）→ L2 冲突决策器
        let exists = san.dest_path.exists() || self.conflicts.case_fold_exists(&san)?;
        if exists {
            let mut decided_target = san.dest_path.clone();
            match self.conflicts.decide(&san, info)? {
                ConflictAction::Overwrite => clear_readonly(&san.dest_path)?,
                ConflictAction::Skip => return Ok(()), // 跳过记录（不增 done）
                ConflictAction::RenameTo(p) => decided_target = p,
                ConflictAction::Abort => return Err(HzipError::Cancelled),
            }
            if decided_target != san.dest_path {
                let new_san = SanitizedEntry {
                    dest_path: decided_target,
                    rel: san.rel.clone(),
                    warnings: san.warnings.clone(),
                };
                return self.finish_entry(reader, info, &new_san);
            }
        }
        self.finish_entry(reader, info, &san)
    }

    /// J→K2 数据落盘（净化后目标已定）。
    fn finish_entry(
        &mut self,
        reader: &dyn ArchiveReader,
        info: &EntryInfo,
        san: &SanitizedEntry,
    ) -> Result<(), HzipError> {
        let __t = std::time::Instant::now();
        create_parent_chain(san)?;
        tracing::trace!(
            __probe = "parent_chain",
            ms = __t.elapsed().as_millis() as u64,
            "解压相位计时"
        );
        let __t = std::time::Instant::now();
        // 事件名两处都要用（Started/Done）⇒ 构造一次 + `clone()` 一次：
        // `hzip_progress::EntryName` = `smol_str::SmolStr`，其堆变体是 `Arc<str>`
        // （smol_str 0.3.6 `Repr::Heap`）⇒ `clone()` 只递增引用计数、**不分配**。
        // 不要改成两处各自 `info.name.as_str().into()`：长名（> INLINE_CAP 23B）
        // 会各分配一份 `Arc<str>`，比「一次构造 + 一次 clone」更贵。
        let entry_name: hzip_progress::EntryName = info.name.as_str().into();
        self.ctx.progress().send(ProgressEvent::EntryStarted {
            entry: entry_name.clone(),
            bytes: info.size_raw,
        });
        // 密码（加密条目经 ctx.need 上抛；PASSWORD_MAX_RETRIES 收敛）
        let pw = self.password_for(info)?;
        let pw_ref = pw
            .as_ref()
            .map(PasswordRef::new)
            .unwrap_or(PasswordRef::none());
        let mut r = reader.read_entry(info.id, pw_ref)?;
        tracing::trace!(
            __probe = "read_entry_open",
            ms = __t.elapsed().as_millis() as u64,
            "解压相位计时"
        );
        let __t = std::time::Instant::now();
        let out = std::fs::File::create(&san.dest_path).map_err(|e| HzipError::Io {
            source: e,
            path: Some(san.dest_path.clone()),
        })?;
        tracing::trace!(
            __probe = "File::create",
            ms = __t.elapsed().as_millis() as u64,
            "解压相位计时"
        );
        let __t = std::time::Instant::now();
        let copy_result = self.stream_copy(&mut r, out, info, &san.dest_path);
        tracing::trace!(
            __probe = "stream_copy",
            ms = __t.elapsed().as_millis() as u64,
            "解压相位计时"
        );
        let crc_ok = copy_result.is_ok();
        self.ctx.progress().send(ProgressEvent::EntryDone {
            entry: entry_name,
            bytes: info.size_raw,
            crc_ok,
        });
        match copy_result {
            Ok(bytes) => {
                self.written.push(san.dest_path.clone());
                self.done += 1;
                self.bytes_written += bytes;
                let _ = self.apply_attrs(info, &san.dest_path);
                Ok(())
            }
            Err(e) => {
                // K1 QE/QC：删除本条目半写文件；完整条目保留（written 台账）
                let _ = std::fs::remove_file(&san.dest_path);
                Err(e)
            }
        }
    }

    /// K 框：read→write 1 MiB/次；每 chunk 取消检查（02 §4.2）；4 MiB 配额检查点。
    fn stream_copy(
        &mut self,
        r: &mut dyn Read,
        mut out: std::fs::File,
        info: &EntryInfo,
        path: &Path,
    ) -> Result<u64, HzipError> {
        let mut buf = vec![0u8; hzip_format::consts::COPY_CHUNK_BYTES as usize];
        let mut total: u64 = 0;
        loop {
            self.ctx.cancel.throw_if_cancelled()?;
            let n = r.read(&mut buf).map_err(|e| unattach_io(e, path))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| HzipError::Io {
                source: e,
                path: Some(path.to_path_buf()),
            })?;
            total += n as u64;
            self.ctx
                .progress()
                .send(ProgressEvent::Bytes { delta: n as u64 });
            // K1：配额检查点（4 MiB 门控在 QuotaGuard 内；packed 线性插值）
            let packed_est = packed_estimate(info, n as u64);
            self.quota.checkpoint(n as u64, packed_est)?;
        }
        // 条目收尾补检（< 4 MiB 条目也要触发 4501/4502/4503 判定）
        self.quota.check_now()?;
        out.flush().map_err(|e| HzipError::Io {
            source: e,
            path: Some(path.to_path_buf()),
        })?;
        Ok(total)
    }

    /// 目录条目落盘（合并语义：已存在目录不动；存在文件 → 冲突已在上层决策）。
    fn write_dir(&mut self, info: &EntryInfo, san: &SanitizedEntry) -> Result<(), HzipError> {
        // 已存在路径为普通文件（dir 与 file 撞）：Overwrite 语义清场
        if san.dest_path.exists() && !san.dest_path.is_dir() {
            let exists = true;
            match self.conflicts.decide(san, info)? {
                ConflictAction::Overwrite => {
                    let _ = std::fs::remove_file(&san.dest_path);
                }
                ConflictAction::Skip => return Ok(()),
                ConflictAction::RenameTo(p) => {
                    // 目录改名：子条目仍落原 rel 路径（create_parent_chain 兜底建原
                    // 目录）——改名仅影响本目录条目自身（元数据落盘），rustdoc 注明
                    let new_san = SanitizedEntry {
                        dest_path: p,
                        rel: san.rel.clone(),
                        warnings: san.warnings.clone(),
                    };
                    return self.mkdir(Some(info), &new_san);
                }
                ConflictAction::Abort => return Err(HzipError::Cancelled),
            }
            let _ = exists;
        }
        self.mkdir(Some(info), san)
    }

    fn mkdir(&mut self, info: Option<&EntryInfo>, san: &SanitizedEntry) -> Result<(), HzipError> {
        if !san.dest_path.is_dir() {
            std::fs::create_dir_all(&san.dest_path).map_err(|e| HzipError::Io {
                source: e,
                path: Some(san.dest_path.clone()),
            })?;
        }
        self.written.push(san.dest_path.clone());
        self.done += 1;
        if let Some(mtime) = info.and_then(|i| i.mtime) {
            self.dir_times.push((san.dest_path.clone(), mtime));
        }
        Ok(())
    }

    /// L1：符号链接策略分支（02 §9.6 表；T31 SymlinkGuard 决策）。
    fn write_symlink(&mut self, info: &EntryInfo, san: &SanitizedEntry) -> Result<(), HzipError> {
        let target_raw = info
            .symlink_target
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        match self.symlinks.decide(&target_raw) {
            LinkAction::Deny => {
                // 默认 deny-escape：逃逸目标 → Err 4002（安全类，best_effort 不跳过）
                Err(HzipError::Security {
                    violation: hzip_error::SecurityViolation::SymlinkEscape,
                })
            }
            LinkAction::Skip => {
                // 跳过（skip 策略档）：逃逸链接不计入成功条目——以 Security 类
                // 错误上抛，由 extract_selection 的 skip 分支记入失败明细
                //（策略表第二行 skip 语义：不中止其余条目，02 §9.6）。
                Err(HzipError::Security {
                    violation: hzip_error::SecurityViolation::SymlinkEscape,
                })
            }
            LinkAction::CreateSymlink => {
                create_parent_chain(san)?;
                let link_rel = san.rel.clone();
                // 平台降级：特权不足 → Reject（显式错误引导开启开发者模式）
                let pol = hzip_platform::symlink::SymlinkPolicy {
                    fallback: hzip_platform::symlink::Fallback::Reject,
                };
                match hzip_platform::symlink::create_symlink(
                    Path::new(&target_raw),
                    &san.dest_path,
                    &pol,
                ) {
                    Ok(_) => {
                        // 创建成功 → 登记台账（组合攻击防线，T31 第三行）
                        self.symlinks.register_created(link_rel);
                        self.written.push(san.dest_path.clone());
                        self.done += 1;
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            LinkAction::FlattenStore => {
                // 展平：链接目标内容以普通文件写入（流式拷贝，防大目标 OOM）
                let content_path = resolve_flatten_target(&target_raw, &self.dest);
                create_parent_chain(san)?;
                let src = std::fs::File::open(&content_path).map_err(|e| HzipError::Io {
                    source: e,
                    path: Some(content_path.clone()),
                })?;
                let out = std::fs::File::create(&san.dest_path).map_err(|e| HzipError::Io {
                    source: e,
                    path: Some(san.dest_path.clone()),
                })?;
                let fake = EntryInfo {
                    size_raw: std::fs::metadata(&content_path)
                        .map(|m| m.len())
                        .unwrap_or(0),
                    crc32: None,
                    ..info.clone()
                };
                let copied = self.stream_copy(
                    &mut std::io::BufReader::new(src),
                    out,
                    &fake,
                    &san.dest_path,
                )?;
                self.written.push(san.dest_path.clone());
                self.done += 1;
                self.bytes_written += copied;
                Ok(())
            }
        }
    }

    /// 加密条目密码（预热已取 → 直接复用；否则经 ctx.need 上抛，重试收敛）。
    fn password_for(&self, info: &EntryInfo) -> Result<Option<Zeroizing<String>>, HzipError> {
        if !info.encrypted {
            return Ok(None);
        }
        if let Some(pw) = &self.early_pw {
            return Ok(Some(pw.clone()));
        }
        let mut reason = PasswordReason::Initial;
        for _ in 0..PASSWORD_MAX_RETRIES {
            match self.ctx.need(JobRequest::Password {
                entry: Some(info.name.clone()),
                reason,
            })? {
                JobResponse::Password(pw) => return Ok(Some(pw)),
                _ => reason = PasswordReason::WrongPassword,
            }
        }
        Err(HzipError::Password {
            target: hzip_error::PasswordTarget::Entry,
        })
    }

    /// 属性恢复（mtime/mode/只读；best-effort 不阻塞解压）。
    fn apply_attrs(&self, info: &EntryInfo, path: &Path) -> Result<(), HzipError> {
        let _ = filetime_apply(path, info.mtime);
        let _ = apply_mode(path, info.mode);
        if info.attributes.read_only
            && let Ok(md) = std::fs::metadata(path)
            && md.is_file()
        {
            let mut perms = md.permissions();
            perms.set_readonly(true);
            let _ = std::fs::set_permissions(path, perms);
        }
        Ok(())
    }

    fn build_report(&self) -> ExtractReport {
        ExtractReport {
            entries_done: self.done,
            bytes_written: self.bytes_written,
            failures: self.failures.clone(),
        }
    }
}

/// fmt 读流错误 → `HzipError`（**保留 fmt 层的语义错误**）。
///
/// fmt 的 `read_entry` 返回的是 `Box<dyn Read>`——流式解码途中发现的语义错误
/// （zip 直读路径的 CRC 尾验 `Checksum 3001` 是最典型的一个）只能以
/// `io::Error` 形式冒出来。若原样按 `Io` 包装，错误码会从 3001 掉到 1001、
/// 且丢掉 `best_effort` 的可累积语义（`best_effort_eligible` 认 `Checksum`）。
/// 故此处取回 io 负载里的 `HzipError`（`Crc32Checked` 等按此约定构造）；
/// 其余 io 错误按 `Io` + 路径上下文包装（原行为不变）。
fn unattach_io(e: std::io::Error, path: &Path) -> HzipError {
    if let Some(inner) = e.get_ref().and_then(|r| r.downcast_ref::<HzipError>()) {
        return inner.clone();
    }
    HzipError::Io {
        source: e,
        path: Some(path.to_path_buf()),
    }
}

/// best_effort 可累积失败类（K2：Checksum/Password/Format；其余含 Io fail-fast）。
fn best_effort_eligible(e: &HzipError) -> bool {
    matches!(
        e,
        HzipError::Checksum { .. } | HzipError::Password { .. } | HzipError::Format { .. }
    )
}

/// skip 策略档的逃逸符号链接（L1 `LinkAction::Skip` 臂上抛；`extract_selection`
/// 记失败明细继续——全管线 Security 类中唯一可跳过路径）。Deny 与 Skip 同码
/// 4002，区分依据：管道构造时记录策略档——仅 `SymlinkPolicy::Skip` 下 Skip 臂
/// 可达；DenyEscape/DenyAll 的逃逸恒走 Deny 臂 fail-fast。
fn is_skip_symlink(e: &HzipError, policy: hzip_format::SymlinkPolicy) -> bool {
    matches!(
        e,
        HzipError::Security {
            violation: hzip_error::SecurityViolation::SymlinkEscape,
        }
    ) && matches!(policy, hzip_format::SymlinkPolicy::Skip)
}

/// packed 线性插值（流式/固实成员的比率输入代理；rustdoc 注明近似语义）。
///
/// 三态：
/// - `size_raw == 0`（single 流式未知）→ packed=raw（ratio 1:1，防 ∞ 误伤；
///   4501/4502 仍精确，4503 对 single 不适用）；
/// - `size_packed == 0 && size_raw > 0`（7z **solid folder 非首条目**——sevenz-rust
///   只给 folder 首文件 packed，其余恒 0，见 T25 §7 记录）→ packed=raw
///   （ratio 1:1 代理：固实成员的单条目比率不可知，喂 0 会伪造 ∞ 炸弹误伤
///   正当 solid 档案——T31 §7-2 张力项按「core 接线不误伤」裁定；真实炸弹的
///   packed 恒非 0，4503 仍对其生效；4501/4502 精确不受影响）；
/// - 正常（packed > 0）→ `size_packed × n / size_raw` 线性插值。
fn packed_estimate(info: &EntryInfo, n: u64) -> u64 {
    if info.size_raw == 0 || info.size_packed == 0 {
        return n;
    }
    (info.size_packed as u128 * n as u128 / info.size_raw as u128) as u64
}

/// Overwrite 前置清只读位（02 §5.3.2 Windows 语义；std Permissions 通用）。
fn clear_readonly(path: &Path) -> Result<(), HzipError> {
    let md = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(()), // 不存在（竞态）→ 无位可清
    };
    if md.is_file() && md.permissions().readonly() {
        clear_readonly_impl(path, md.permissions()).map_err(|e| HzipError::Io {
            source: e,
            path: Some(path.to_path_buf()),
        })?;
    }
    Ok(())
}

/// 平台化清只读位：win 直接 set_readonly(false)（std 语义 = 清 FILE_ATTRIBUTE_READONLY）；
/// unix 需经 `PermissionsExt` 精确回写 mode 位（`set_readonly(false)` 会把文件变
/// 全体可写 0o666——clippy `permissions-set-readonly-false` 拦截）。清位仅恢复
/// 写权（在原 mode 上加 0o200），不扩大其他位。
#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)] // win std 语义 = 清 FILE_ATTRIBUTE_READONLY（平台正确）
fn clear_readonly_impl(path: &Path, perms: std::fs::Permissions) -> std::io::Result<()> {
    let mut perms = perms;
    perms.set_readonly(false);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(windows))]
fn clear_readonly_impl(path: &Path, perms: std::fs::Permissions) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = perms.mode();
    // 清只读 = 恢复属主写位（其余位保持原样，不扩大）
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o200))
}

/// mode 恢复（unix 权限位；Windows no-op——无 unix 权限语义）。
#[cfg(not(windows))]
fn apply_mode(path: &Path, mode: Option<u32>) -> Result<(), HzipError> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(m) = mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(m & 0o777)).map_err(
            |e| HzipError::Io {
                source: e,
                path: Some(path.to_path_buf()),
            },
        )?;
    }
    Ok(())
}

#[cfg(windows)]
fn apply_mode(_path: &Path, _mode: Option<u32>) -> Result<(), HzipError> {
    Ok(())
}

/// 创建父目录链（净化目标父目录；dest 由 resolve_dest 保证存在）。
fn create_parent_chain(san: &SanitizedEntry) -> Result<(), HzipError> {
    if let Some(parent) = san.dest_path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent).map_err(|e| HzipError::Io {
            source: e,
            path: Some(parent.to_path_buf()),
        })?;
    }
    Ok(())
}

/// Flatten 展平目标解析：相对目标（dest 内）→ dest.join；绝对/含 `..` → 原样
/// （flatten 策略显式授权读取——安全边界由策略配置承担，02 §9.6 表）。
fn resolve_flatten_target(target_raw: &str, dest: &Path) -> PathBuf {
    let t = Path::new(target_raw);
    if t.is_absolute() {
        return t.to_path_buf();
    }
    if t.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return t.to_path_buf();
    }
    dest.join(t)
}

/// mtime 恢复（best-effort；经 `hzip_platform::metadata::set_mtime`）。
///
/// **Windows 语义（T32 实测修正）**：`std::fs::File::open`（GENERIC_READ）调
/// `set_times` 以 PermissionDenied（os 5）失败——`SetFileTime` 需
/// FILE_WRITE_ATTRIBUTES；且**目录**需 `FILE_FLAG_BACKUP_SEMANTICS` 句柄
/// （std `OpenOptions` 不可表达）。平台层 `set_mtime`（T32 授权例外新增，
/// 05 §5 平台 API 缺失路径）统一处理文件与目录；unix 侧 utimensat 仅时间
/// 不动权限。失败不阻塞解压（调用方吞错——T50 golden mtime 断言依赖）。
fn filetime_apply(path: &Path, mtime: Option<time::OffsetDateTime>) -> std::io::Result<()> {
    let Some(t) = mtime else { return Ok(()) };
    hzip_platform::metadata::set_mtime(path, t)
}

// ===================== 密码路由器（PasswordSource 经 ctx.need） =====================

/// 预检条目上限（64 MiB）：`read_entry` 物化整条目（zip 解密 spool / 7z 解码），
/// 超限则跳过验证（采纳不验的降级路径）。与 [`crate::services::browse::PREFLIGHT_MAX_BYTES`]
/// 同值——GUI 密码门（`preflight_password`）与本处用**同一判据**。
const PREFLIGHT_MAX_BYTES: u64 = crate::services::browse::PREFLIGHT_MAX_BYTES;

/// 打开档案：头加密（Password 错 / HZ-3102）在**任何进度事件之前**经
/// `JobRequest::Password { entry: None }` 问密码、带 `OpenOptions::password`
/// 重开（≤ `PASSWORD_MAX_RETRIES`；GUI 密码弹窗与 CLI 三通道同一条链）。
/// 未加密档案首轮即成功——零额外开销。
fn open_archive_retry(
    ctx: &JobCtx,
    path: &Path,
    seed: Option<&Zeroizing<String>>,
) -> Result<Box<dyn hzip_format::ArchiveReader>, HzipError> {
    // 参数窗预填密码优先：先直接用它开（成功 = 零弹窗）；不对再进询问链
    //（首个弹窗按「密码错误」呈现，别让用户以为没试过他填的那个）。
    let mut pw: Option<Zeroizing<String>> = seed.cloned();
    let mut reason = if seed.is_some() {
        PasswordReason::WrongPassword
    } else {
        PasswordReason::Initial
    };
    let mut attempt = 0u32;
    loop {
        let opts = hzip_format::OpenOptions {
            password: pw.clone(),
            ..Default::default()
        };
        match ctx
            .registry
            .open_reader(hzip_format::Source::File(path.to_path_buf()), &opts)
        {
            Ok(r) => return Ok(r),
            Err(HzipError::Password { .. }) if attempt < PASSWORD_MAX_RETRIES => {
                attempt += 1;
                match ctx.need(JobRequest::Password {
                    entry: None,
                    reason,
                })? {
                    JobResponse::Password(p) => {
                        pw = Some(p);
                        reason = PasswordReason::WrongPassword;
                    }
                    _ => reason = PasswordReason::WrongPassword,
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// 预热密码 + 有界预检（设计稿 11 §3.5 矩阵）：所选条目含加密 → 弹密码
/// （`Initial`；错则 `WrongPassword` 重问，≤ `PASSWORD_MAX_RETRIES`）。
///
/// 预检取首个「加密且 `size_packed ≤ PREFLIGHT_MAX_BYTES`」条目真读一次：
/// `read_entry` 在**流交付前**完成密码验证（zip KV/校验头、7z 解码、rar 密钥），
/// 错密码立即 `Password` 错 → 门内重问；无有界条目 → 采纳不验
/// （降级：进度中由既有 WrongPassword 重问链兜底；7z 数据加密 CRC 需全解）。
fn resolve_early_password(
    ctx: &JobCtx,
    entries: &[EntryInfo],
    ids: &[EntryId],
    reader: &dyn hzip_format::ArchiveReader,
    seed: Option<&Zeroizing<String>>,
) -> Result<Option<Zeroizing<String>>, HzipError> {
    let mut any_encrypted = false;
    let mut bounded: Option<&EntryInfo> = None;
    for &id in ids {
        let Some(e) = entries.get(id) else { continue };
        if !e.encrypted {
            continue;
        }
        any_encrypted = true;
        if bounded.is_none() && e.size_packed <= PREFLIGHT_MAX_BYTES {
            bounded = Some(e);
        }
    }
    if !any_encrypted {
        return Ok(None);
    }
    // 参数窗预填密码优先：有界条目真读验证（流交付前完成预验），通过即零弹窗；
    // 不对则落询问链。无有界条目 → 采纳不验（与下方降级路径同语义）。
    if let Some(seed) = seed {
        match bounded {
            Some(info) => match reader.read_entry(info.id, PasswordRef::new(seed)) {
                Ok(_stream) => return Ok(Some(Zeroizing::new(seed.as_str().to_owned()))),
                Err(HzipError::Password { .. }) => {} // 落询问链
                Err(e) => return Err(e),
            },
            None => return Ok(Some(Zeroizing::new(seed.as_str().to_owned()))),
        }
    }
    let mut reason = PasswordReason::Initial;
    for _ in 0..PASSWORD_MAX_RETRIES {
        let pw = match ctx.need(JobRequest::Password {
            entry: None,
            reason,
        })? {
            JobResponse::Password(p) => p,
            _ => {
                reason = PasswordReason::WrongPassword;
                continue;
            }
        };
        if let Some(info) = bounded {
            match reader.read_entry(info.id, PasswordRef::new(&pw)) {
                // 流交付 = 密码已验（读侧预验在交付前完成；立即丢弃物化结果）
                Ok(_stream) => return Ok(Some(pw)),
                Err(HzipError::Password { .. }) => {
                    reason = PasswordReason::WrongPassword;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        return Ok(Some(pw)); // 无有界条目：采纳不验（降级路径）
    }
    Err(HzipError::Password {
        target: hzip_error::PasswordTarget::Archive,
    })
}

/// 密码来源适配：fmt 层逐条问询 → `ctx.need(JobRequest::Password)`。
///
/// `password()` 无 Result 通道（T20 冻结 trait）——取消/超时以 `None` 表达，
/// 真实错误记入 `last_error` 供 run_test 收敛（Cancelled → Outcome::Cancelled）。
struct PwRouter<'a> {
    ctx: &'a JobCtx,
}

impl<'a> PwRouter<'a> {
    fn new(ctx: &'a JobCtx) -> Self {
        Self { ctx }
    }
}

impl PasswordSource for PwRouter<'_> {
    fn password(&self, entry: Option<&hzip_format::EntryName>) -> Option<Zeroizing<String>> {
        match self.ctx.need(JobRequest::Password {
            entry: entry.cloned(),
            reason: PasswordReason::Initial,
        }) {
            Ok(JobResponse::Password(pw)) => Some(pw),
            Ok(_) => None,
            Err(_) => None, // 取消/超时 → None（fmt 层视为无密码继续 → Password 错）
        }
    }
}
