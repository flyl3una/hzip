//! 转换服务（02 §9.2 convert 职责 + §7.5 组合写入；T33 独占）。
//!
//! # 语义（读写管道对接）
//!
//! ```text
//! 读端: Source::File → registry.open_reader（组合链自动解层 .tar.gz → tar）
//!   → entries()（组合 reader 内层条目）
//! 写端: registry.handler(to).create_writer（目标格式 + 级别/线程）
//!   → 逐条目: EntryInfo → EntryMeta（name/is_dir/is_symlink/symlink_target/
//!     mtime/mode/attrs 逐字段；size_raw 直通——读端已知大小）
//!   → read_entry(id) → next_entry(meta) → 1 MiB 桥接循环（不落盘）
//!   → finish（自内向外逐层收尾）
//! ```
//!
//! # 规则与边界（rustdoc）
//!
//! - **能力校验前置**：目标格式 `caps.write=false`（rar）→ `Err Unsupported`
//!   HZ-5001（TC-CLI-006 前置；rar 的 create_writer 亦恒拒，双保险）；
//! - **单文件流目标**（gz/bz2/xz/zst）恰一条目——多条目源 → `next_entry`
//!   第二次调用时报 5001（single writer 语义）；core 转换不预筛（错误语义
//!   透明上抛，条目数超限属格式能力约束）；
//! - **元数据保真**：`size_raw` 直通（读端已知大小，fmt 层无临时文件两遍法，
//!   T22 §4.5 流式未知分支不触发）；组合源（`.tar.gz → .zip`）的读端即 T20
//!   组合 reader——条目序与内容透传，压缩层信息丢弃（转换语义即重新打包）；
//! - **best_effort**：条目读取失败（含密码错未提供）→ 记 EntryFailure 继续；
//!   fail-fast 默认删半写产物（out 文件；Split 卷集见 compress 同型清理）。
//!   密码：加密源条目经 [`JobCtx::need`] 上抛（与 extract 同 PwRouter 语义）；
//! - **7z 头回写与管道输出**：写 `Sink::Stdout` 遇 7z 的临时文件中转是 fmt
//!   层内部行为（02 §8.2/§13.2）；core 的流式桥不因此加中间文件；
//! - **取消/进度**：桥接循环内每 1 MiB 检查取消令牌（同解压 K 框语义）。
//!
//! 出处：02 §9.2 convert、02 §7.5 组合写入 H 节点、§7.3 契约、§4.2 取消传播。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use hzip_error::HzipError;
use hzip_format::{CreateOptions, EntryMeta, PasswordRef, Sink, WriteReport};
use hzip_progress::{Outcome, ProgressEvent};
use zeroize::Zeroizing;

use crate::jobs::{JobCtx, JobRequest, JobResponse, JobSpec, PasswordReason};
use crate::report;

/// 转换任务选项（JobSpec 载体）。
///
/// 字段集「非冻结」。`Default` = 目标 tar、级别 Normal、线程自动。
#[derive(Debug, Clone)]
pub struct ConvertSpec {
    /// 源档案路径（`JobSpec.source` 亦可，二者等价）
    pub source: PathBuf,
    /// 目标格式 ID
    pub to: hzip_format::FormatId,
    /// 输出路径（Sink::File）
    pub out: PathBuf,
    /// 统一压缩级别
    pub level: hzip_codec::level::Level,
    /// 线程数（0 = 自动）
    pub threads: u32,
    /// 写向 best_effort（默认 false = fail-fast）
    pub best_effort: bool,
}

impl Default for ConvertSpec {
    fn default() -> Self {
        Self {
            source: PathBuf::new(),
            to: hzip_format::FormatId::from("tar"),
            out: PathBuf::new(),
            level: hzip_codec::level::Level::Normal,
            threads: 0,
            best_effort: false,
        }
    }
}

impl ConvertSpec {
    /// 从 JobSpec 提取（`compress: None` → 默认 + source 回退 JobSpec.source）。
    pub(crate) fn from_job(spec: &JobSpec) -> Self {
        match &spec.compress {
            Some(c) => ConvertSpec {
                source: c
                    .inputs
                    .first()
                    .cloned()
                    .unwrap_or_else(|| spec.source.clone()),
                to: c.format.clone(),
                out: c.out.clone(),
                level: c.level,
                threads: c.threads,
                best_effort: c.best_effort,
            },
            None => ConvertSpec {
                source: spec.source.clone(),
                ..Default::default()
            },
        }
    }
}

/// JobKind::Convert 实现体（02 §9.2 + §7.5）。
pub(crate) fn run_convert(ctx: &JobCtx, spec: &JobSpec) -> Outcome {
    match convert_impl(ctx, spec) {
        Ok(outcome) => outcome,
        Err(HzipError::Cancelled) => Outcome::Cancelled,
        Err(e) => Outcome::Failed(e),
    }
}

fn convert_impl(ctx: &JobCtx, spec: &JobSpec) -> Result<Outcome, HzipError> {
    let c = ConvertSpec::from_job(spec);
    // 任务实际开始时刻（终局 stats 回填用，见 report::stamp_stats）
    let started = Instant::now();
    // 读端（组合链 registry 内装配；02 §7.5）
    let reader = ctx.registry.open_reader(
        hzip_format::Source::File(c.source.clone()),
        &Default::default(),
    )?;
    let entries = reader.entries()?;
    // 能力校验：目标 caps.write=false → Unsupported 5001（rar）
    let handler = ctx
        .registry
        .handler(c.to.as_str())
        .ok_or_else(|| HzipError::Unsupported {
            what: format!("格式 {} 未注册（feature 门控）", c.to).into(),
        })?;
    if !handler.caps().write {
        return Err(HzipError::Unsupported {
            what: format!("格式 {} 不支持压缩（caps.write=false，HZ-5001）", c.to).into(),
        });
    }
    // 写端
    let opts = CreateOptions {
        level: c.level,
        threads: c.threads,
        ..Default::default()
    };
    let mut writer = handler.create_writer(Sink::File(c.out.clone()), &opts)?;
    // 逐条目桥接（entries 原序；元数据保真映射）
    let mut failures: Vec<hzip_progress::EntryFailure> = Vec::new();
    let mut pw_holder: Option<Zeroizing<String>> = None;
    for info in &entries {
        let meta = EntryMeta {
            name: info.name.clone(),
            is_dir: info.is_dir,
            is_symlink: info.is_symlink,
            symlink_target: info.symlink_target.clone(),
            size_raw: Some(info.size_raw), // 读端已知大小：直通（§3.2-①）
            mtime: info.mtime,
            mode: info.mode,
            attrs: info.attributes,
            encrypted: false, // 目标非加密（加密源 → 明文重写；02 §9.2 规则②）
        };
        // 读端密码（加密条目；一次成功缓存——zip reader 单档密码语义）
        if info.encrypted && pw_holder.is_none() {
            pw_holder = Some(ask_password(ctx, info)?);
        }
        let pw_ref = match pw_holder.as_ref() {
            Some(pw) => PasswordRef::new(pw),
            None => PasswordRef::none(),
        };
        let entry_smol: hzip_progress::EntryName = info.name.as_str().into();
        ctx.progress().send(ProgressEvent::EntryStarted {
            entry: entry_smol.clone(),
            bytes: info.size_raw,
        });
        let copy_result = (|| -> Result<u64, HzipError> {
            let mut r = reader.read_entry(info.id, pw_ref)?;
            let mut sink = writer.next_entry(&meta, pw_ref)?;
            bridge(&mut r, &mut sink, ctx, &c.out)
        })();
        match copy_result {
            Ok(bytes) => {
                ctx.progress().send(ProgressEvent::EntryDone {
                    entry: entry_smol.clone(),
                    bytes,
                    crc_ok: true,
                });
            }
            Err(e) if c.best_effort => {
                ctx.progress().send(ProgressEvent::EntryDone {
                    entry: entry_smol.clone(),
                    bytes: 0,
                    crc_ok: false,
                });
                failures.push(hzip_progress::EntryFailure {
                    entry: entry_smol,
                    error: Box::new(e),
                });
            }
            Err(e) => {
                cleanup_failed(&c.out);
                return Err(e);
            }
        }
    }
    let wreport = writer.finish().inspect_err(|_| cleanup_failed(&c.out))?;
    Ok(finish_outcome(wreport, failures, started))
}

/// 1 MiB 桥接（流式不落盘；每 chunk 取消检查）。
fn bridge(
    r: &mut dyn Read,
    sink: &mut dyn Write,
    ctx: &JobCtx,
    out: &Path,
) -> Result<u64, HzipError> {
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        ctx.cancel.throw_if_cancelled()?;
        let n = r.read(&mut buf).map_err(|e| HzipError::Io {
            source: e,
            path: Some(out.to_path_buf()),
        })?;
        if n == 0 {
            break;
        }
        sink.write_all(&buf[..n]).map_err(|e| HzipError::Io {
            source: e,
            path: Some(out.to_path_buf()),
        })?;
        total += n as u64;
    }
    Ok(total)
}

/// 加密条目密码询问（JobRequest 通道；重试收敛 Password 错）。
fn ask_password(
    ctx: &JobCtx,
    info: &hzip_format::EntryInfo,
) -> Result<Zeroizing<String>, HzipError> {
    let mut reason = PasswordReason::Initial;
    for _ in 0..3 {
        match ctx.need(JobRequest::Password {
            entry: Some(info.name.clone()),
            reason,
        })? {
            JobResponse::Password(pw) => return Ok(pw),
            _ => reason = PasswordReason::WrongPassword,
        }
    }
    Err(HzipError::Password {
        target: hzip_error::PasswordTarget::Entry,
    })
}

/// 终局收敛（failures 空 → Done；非空 → Partial）。stats 回填实际开始时刻
/// （report 聚合新建 → 耗时恒 0 ms，见 [`report::stamp_stats`]）。
fn finish_outcome(
    wreport: WriteReport,
    failures: Vec<hzip_progress::EntryFailure>,
    started: Instant,
) -> Outcome {
    if failures.is_empty() {
        let mut o = report::outcome_of_write(wreport);
        if let Outcome::Done(s) = &mut o {
            report::stamp_stats(s, started);
        }
        o
    } else {
        let mut stats = report::stats_from_write(&wreport);
        stats.entries_total = stats.entries_done + failures.len() as u64;
        report::stamp_stats(&mut stats, started);
        Outcome::Partial(stats, failures)
    }
}

/// fail-fast 回滚：删除半写产物（删除失败仅 warning）。
fn cleanup_failed(out: &PathBuf) {
    if out.exists()
        && let Err(e) = std::fs::remove_file(out)
    {
        tracing::warn!(path = %out.display(), err = ?e, "转换失败产物清理 best-effort");
    }
}
