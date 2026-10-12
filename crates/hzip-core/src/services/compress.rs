//! 压缩服务管道（02 §9.5 图逐框；T33 独占）。
//!
//! # 管道（§9.5 图逐框）
//!
//! ```text
//! A JobKind::Compress → B walk::collect 展开（目录递归/文件单条）
//!   → 过滤链（glob 排除/隐藏/链接策略，walk.rs）
//!   → C walk::pre_stats 预统计（total_bytes/total_entries 源头）
//!   → D 输出分卷? Sink::Split : Sink::File
//!   → G create_writer（级别/线程/加密映射经 CreateOptions）
//!   → H 调度：小文件 rayon 并行预读（4 MiB 阈值；写入序恒定）
//!   → I next_entry（目录=零长、symlink=记 target）
//!   → J 流拷贝（1 MiB）+ 进度 → K 链接策略分支（follow=true 读目标内容）
//!   → L finish（中央目录/尾块/卷收尾）
//!   → M WriteReport + 可选重开自检（compress.verify_after）
//! ```
//!
//! # 关键裁定与语义（rustdoc）
//!
//! - **写入串行是契约级约束**（rustdoc 注明，任务卡 §4.4）：`EntrySink<'_>`
//!   借用 writer，core 无法并行开多个 `next_entry`；「写入按 handler 能力
//!   并行」的实际落点 = 编码器内部线程（`CreateOptions.threads` 透传，
//!   02 §6.3：deflate/bzip2 多文件层、lzma2/zstd 编码器内部）。core 的 rayon
//!   只做 **H 框局部数据并行预读**（非任务池，T30 §1 裁定不冲突）。
//! - **写向 best_effort** 经 [`CompressSpec.best_effort`] 传参（WriteCtx 无此
//!   字段——不扩契约，登记裁定）；失败来源 = 源读取 IO 错 + 写入侧条目错。
//! - **fail-fast 回滚**：首错删除半写产物（out 文件 / Split 卷集全部卷）——
//!   写向产物不可用即整体作废（02 §9.5 无部分产物语义）；删除失败仅 warning
//!   不 Err（清理 best-effort）。产物路径由 [`tracked_volumes`] 持续登记
//!   （Sink::Split 的 fmt 层 volume_paths 只在 finish 报告——运行中需自行扫
//!   `{prefix}.*` 前缀残留）。
//! - **空输入**（过滤后 0 条目）→ `Err Unsupported{hz-5003 empty-input}`
//!   （core 语义错误；CLI 层转 warning——TC-CLI-011 由调用层选择）。
//! - **预读窗口**：`W = 2 × threads`（threads 缺省 clamp 到可用核）；
//!   PREFETCH_SMALL_BYTES = 4 MiB 阈值。大文件不预读（直接流式防 RSS 膨胀，
//!   TC-ALG-011 语义）。窗口内小文件**写完即释放**（`cache[idx].take()`）——
//!   峰值 = 窗口内活跃小文件（≤ W × 4 MiB），与输入总量无关。
//! - **verify_after**（B.1 默认 true）：finish 后重开 reader 跑 `test()`；
//!   大文件可关（性能语义本注释）。
//!
//! 出处：02 §9.5 图、§7.3 `ArchiveWriter`/`EntrySink`/`WriteCtx` 契约、
//! 02 §6.3 级别/线程、附录 B.1 `compress.*`、02 §3.5 边界规则。

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use hzip_error::HzipError;
use hzip_format::{
    ArchiveWriter, CreateOptions, EncryptionRequest, EntryMeta, ExtractCtx, FormatId, PasswordRef,
    PasswordSource, Sink, WriteReport,
};
use hzip_progress::{CancellationToken, Outcome, ProgressBus, ProgressEvent};
use rayon::prelude::*;
use time::OffsetDateTime;
use zeroize::Zeroizing;

use crate::jobs::{JobCtx, JobKind, JobSpec};
use crate::report;
use crate::services::walk::{self, InputFilters, PreStats, WalkedFile};

/// 小文件并行预读阈值（§4.4 裁定：4 MiB；超出直接流式，防 RSS 膨胀）。
const PREFETCH_SMALL_BYTES: u64 = 4 * 1024 * 1024;
/// 流拷贝块（02 §9.5 J 框；hzip-format 契约常量同值 1 MiB）。
const STREAM_CHUNK: usize = 1024 * 1024;

/// 压缩任务选项（JobSpec 载体；02 附录 B.1 `compress.*` + §6.3）。
///
/// 字段集「非冻结」——T33 起 services 消费。`Default` = 全默认
/// （级别 Normal、线程自动、verify_after=true、follow_symlinks=false、
/// best_effort=false、无密码/分卷/排除）。
#[derive(Debug, Clone)]
pub struct CompressSpec {
    /// 输入路径列表（目录递归 / 文件单条）
    pub inputs: Vec<PathBuf>,
    /// 输出格式 ID（"tar"/"zip"/"7z"/"gz"/"bz2"/"xz"/"zst"；分卷/加密能力随格式）
    pub format: FormatId,
    /// 输出路径（Sink::File）；`split` 有值时即卷前缀
    pub out: PathBuf,
    /// 分卷大小（None = 单文件）
    pub split: Option<u64>,
    /// 统一压缩级别（02 §6.3）
    pub level: hzip_codec::level::Level,
    /// 压缩方式（`None` = 沿用格式默认编码器）。
    ///
    /// 与 `level` 正交：方式选编码器、级别选压多狠。须 ∈ 该格式
    /// `caps().methods`，否则 [`writer_for`] 以 HZ-5001 拒绝（不在 fmt 层各写一份）。
    pub method: Option<hzip_codec::traits::CodecMethod>,
    /// 线程数（0 = 自动）
    pub threads: u32,
    /// LZMA2 字典大小覆写（`None` = 随级别派生；仅 7z 消费，09 §3.2）
    pub dict_size: Option<u64>,
    /// 加密请求（格式/feature 不支持 → writer_for HZ-5001 / 各 fmt 二次防线）
    pub encryption: Option<EncryptionRequest>,
    /// 加密密码（与 `encryption` **成对**：一有一无 → HZ-5003，core 入口校验）。
    pub password: Option<Zeroizing<String>>,
    /// 7z「加密文件名」= 头加密（`None` = 格式默认 true；其余格式忽略）。
    pub encrypt_names: Option<bool>,
    /// 固实（7z；feature 门控）
    pub solid: bool,
    /// 更新方式：追加到既有档案（`false` = 新建档案）。
    ///
    /// 仅 `caps.append` 格式有意义（zip / 裸 tar 已实装；7z 与单流格式在
    /// writer 内正确拒绝 HZ-5001）。**绝不静默新建**——用户选了「追加」而格式
    /// 不支持时应当报错，而不是把既有档案覆盖掉。
    pub append: bool,
    /// 排除 glob 列表
    pub exclude_globs: Vec<String>,
    /// 排除隐藏文件（默认 false）
    pub exclude_hidden: bool,
    /// 跟随符号链接（默认 false：链接条目记 target）
    pub follow_symlinks: bool,
    /// finish 后重开自检（默认 true；大文件可关）
    pub verify_after: bool,
    /// 写向 best_effort（源读取/写入侧失败记 Partial 继续；默认 false=fail-fast）
    pub best_effort: bool,
    /// 保留文件属性（mtime/mode/attrs；默认 true）
    pub keep_metadata: bool,
    /// 档案注释（caps.comment 格式才有效）
    pub comment: Option<String>,
}

impl Default for CompressSpec {
    fn default() -> Self {
        Self {
            inputs: Vec::new(),
            format: FormatId::from("tar"),
            out: PathBuf::new(),
            split: None,
            level: hzip_codec::level::Level::Normal,
            method: None,
            threads: 0,
            dict_size: None,
            encryption: None,
            password: None,
            encrypt_names: None,
            solid: false,
            append: false,
            exclude_globs: Vec::new(),
            exclude_hidden: false,
            follow_symlinks: false,
            verify_after: true,
            best_effort: false,
            keep_metadata: true,
            comment: None,
        }
    }
}

/// JobKind::Compress 实现体（02 §9.5 全管道）。
pub(crate) fn run_compress(ctx: &JobCtx, spec: &JobSpec) -> Outcome {
    match compress_impl(ctx, spec) {
        Ok(outcome) => outcome,
        Err(HzipError::Cancelled) => Outcome::Cancelled,
        Err(e) => Outcome::Failed(e),
    }
}

impl CompressSpec {
    /// 从 JobSpec 提取（`compress: None` → 默认）。
    pub(crate) fn from_job(spec: &JobSpec) -> Self {
        spec.compress.clone().unwrap_or_default()
    }
}

/// 便捷构造（测试/服务直调；inputs 目录列表）。
#[allow(dead_code)] // 集成测试直调面（roundtrip 已消费；CLI 层构建 JobSpec 亦用）
pub(crate) fn compress_job(spec: CompressSpec) -> JobSpec {
    let source = spec.inputs.first().cloned().unwrap_or_default();
    let mut s = JobSpec::new(JobKind::Compress, source);
    s.compress = Some(spec);
    s
}

/// 组合格式 → 内层编码器（`tar.gz` → `gz`）。
///
/// 组合写走**两遍法**（见 [`compress_composite`]）：先写 tar，再用该编码器压一层。
/// 读端本就按魔数递归解层（`sniffer` + `registry::build_decoder_chain`），故产物
/// 与自己压出来的 `.tar.gz` 完全同构。
fn composite_codec(format: &str) -> Option<&'static str> {
    match format.to_ascii_lowercase().as_str() {
        "tar.gz" | "tgz" => Some("gz"),
        "tar.bz2" | "tbz2" | "tbz" => Some("bz2"),
        "tar.xz" | "txz" => Some("xz"),
        "tar.zst" | "tzst" => Some("zst"),
        _ => None,
    }
}

/// 组合写的中间 tar 路径（与目标同目录同名：`packed.tar.gz` → `packed.tar`）。
///
/// 用「目标主名 + .tar」而不是随机临时名：外层编码器会把内层文件名写进容器
/// （gzip 的 FNAME），`packed.tar` 才是解压端期望看到的名字。已存在同名文件时
/// **不覆盖**（返回 Err）——那是用户的东西。
fn composite_temp_path(out: &std::path::Path) -> Result<PathBuf, HzipError> {
    let stem = out
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.split('.').next())
        .unwrap_or("archive");
    Ok(out.with_file_name(format!("{stem}.tar")))
}

/// 组合写：`tar.<codec>`（两遍法）。
///
/// pass 1 = 源 → 中间 tar（**复用** tar handler + [`write_all`]：取消 / 进度 /
/// CRC / best_effort 全是同一份代码）；pass 2 = 中间 tar → 目标（复用单流
/// handler，恰一条目）。报告取两遍的并集：条目数/输入字节来自 pass 1（用户看到的
/// 是「我压了几个文件」），输出字节/卷路径来自 pass 2（真正的产物）。
///
/// 自检（`verify_after`）不在这里做：调用方拿合并后的报告按**原始 spec**自检，
/// 读端会自动解组合层，等于顺带验了两遍的产物。
fn compress_composite(
    ctx: &JobCtx,
    c: &CompressSpec,
    codec: &str,
    inputs: &[walk::WalkedFile],
    stats0: &walk::PreStats,
) -> Result<(WriteReport, Vec<hzip_progress::EntryFailure>), HzipError> {
    let tmp = composite_temp_path(&c.out)?;
    if tmp.exists() {
        return Err(HzipError::Unsupported {
            what: format!(
                "组合压缩的中间文件已存在：{}（请先移走或改名）",
                tmp.display()
            )
            .into(),
        });
    }
    let mut failures: Vec<hzip_progress::EntryFailure> = Vec::new();
    // —— pass 1：源 → 中间 tar ——
    let mut tar_spec = c.clone();
    tar_spec.format = FormatId::from("tar");
    tar_spec.out = tmp.clone();
    tar_spec.split = None; // tar 不支持分卷；分卷语义留给外层单流（也不支持）
    tar_spec.append = false;
    tar_spec.verify_after = false; // 中间产物不自检，终局统一验
    let tmp_report = {
        let opts = create_options(&tar_spec);
        let mut w = writer_for(ctx, &tar_spec, Sink::File(tmp.clone()), &opts)?;
        write_all(
            &mut *w,
            inputs,
            stats0,
            ctx,
            &tar_spec,
            PasswordRef::none(),
            &mut failures,
        )?;
        w.finish()?
    };
    // —— pass 2：中间 tar → 目标（单条目） ——
    let mut codec_spec = c.clone();
    codec_spec.format = FormatId::from(codec);
    codec_spec.inputs = vec![tmp.clone()];
    codec_spec.out = c.out.clone();
    codec_spec.split = None;
    codec_spec.append = false;
    codec_spec.exclude_globs = Vec::new();
    codec_spec.exclude_hidden = false;
    codec_spec.follow_symlinks = false;
    codec_spec.best_effort = false;
    codec_spec.verify_after = false; // 同上：终局由调用方自检
    let codec_result = (|| -> Result<WriteReport, HzipError> {
        let walked = walk::collect(&codec_spec.inputs, &InputFilters::default())?;
        let st = walk::pre_stats(&walked);
        let opts = create_options(&codec_spec);
        let mut w = writer_for(ctx, &codec_spec, Sink::File(codec_spec.out.clone()), &opts)?;
        let mut f2 = Vec::new();
        write_all(
            &mut *w,
            &walked,
            &st,
            ctx,
            &codec_spec,
            PasswordRef::none(),
            &mut f2,
        )?;
        let rep = w.finish()?;
        // 外层单条目的失败不该被吞掉
        failures.extend(f2);
        Ok(rep)
    })();
    // 中间产物无条件清理（成功/失败都不留）
    let _ = std::fs::remove_file(&tmp);
    let codec_report = codec_result?;
    Ok((
        WriteReport {
            entries_written: tmp_report.entries_written,
            bytes_in: tmp_report.bytes_in,
            bytes_out: codec_report.bytes_out,
            volume_paths: codec_report.volume_paths,
        },
        failures,
    ))
}

/// 写入选项（表单一处，`compress_impl` 与组合写共用）。
fn create_options(c: &CompressSpec) -> CreateOptions {
    CreateOptions {
        level: c.level,
        method: c.method,
        threads: c.threads,
        dict_size: c.dict_size,
        encryption: c.encryption,
        encrypt_names: c.encrypt_names,
        solid: c.solid,
        comment: c.comment.clone(),
        append: c.append,
    }
}

fn compress_impl(ctx: &JobCtx, spec: &JobSpec) -> Result<Outcome, HzipError> {
    let c = CompressSpec::from_job(spec);
    validate_encryption_pair(&c.encryption, &c.password)?;
    // 档案级密码（写入主循环逐条目透传；fmt 按 caps 消费，目录/链接不加密）
    let pw_ref = match c.password.as_ref() {
        Some(p) => PasswordRef::new(p),
        None => PasswordRef::none(),
    };
    // 任务实际开始时刻（终局 stats 回填用——report 聚合时新建的 stats
    // started 落于聚合时刻，不经回填耗时恒 0 ms）
    let started = Instant::now();
    // 压缩入口（源/目标概要；密码/加密请求内容不落日志）
    tracing::info!(
        job = ctx.job_id,
        src = %c.inputs.first().map(|p| p.display().to_string()).unwrap_or_default(),
        out = %c.out.display(),
        format = %c.format,
        inputs = c.inputs.len(),
        "压缩任务开始"
    );
    // B：展开输入 + 过滤链（§4.3）
    let filters = InputFilters {
        exclude_globs: c.exclude_globs.clone(),
        exclude_hidden: c.exclude_hidden,
        follow_symlinks: c.follow_symlinks,
    };
    let inputs = match walk::collect(&c.inputs, &filters) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(job = ctx.job_id, err = %e, "压缩输入展开失败");
            return Err(e);
        }
    };
    // 空输入 → 带语义错误（CLI 层转 warning；TC-CLI-011 语义在 core 边界）
    if inputs.is_empty() {
        tracing::error!(job = ctx.job_id, "压缩输入过滤后为空（hz-5003）");
        return Err(HzipError::Unsupported {
            what: "hz-5003 empty-input（过滤后 0 条目）".into(),
        });
    }
    // C：预统计（JobStarted total 源头）
    let stats0 = walk::pre_stats(&inputs);
    ctx.progress().send(ProgressEvent::JobStarted {
        job_id: ctx.job_id,
        total_bytes: stats0.total_bytes,
        total_entries: stats0.entries,
    });
    let result = (|| -> Result<(WriteReport, Vec<hzip_progress::EntryFailure>), HzipError> {
        // 组合格式（tar.gz / tar.bz2 / tar.xz / tar.zst）：两遍法（见上）
        if let Some(codec) = composite_codec(c.format.as_str()) {
            return compress_composite(ctx, &c, codec, &inputs, &stats0);
        }
        // D/G：sink → create_writer
        let sink = match c.split {
            Some(vpb) => Sink::Split {
                prefix: c.out.clone(),
                bytes_per_volume: vpb,
            },
            None => Sink::File(c.out.clone()),
        };
        let create_opts = create_options(&c);
        // 分卷需 caps.split（tar/single 拒绝经各自 Unsupported）
        let mut writer = writer_for(ctx, &c, sink, &create_opts)?;
        // H~K：调度 + 写入主循环
        let mut failures: Vec<hzip_progress::EntryFailure> = Vec::new();
        write_all(
            &mut *writer,
            &inputs,
            &stats0,
            ctx,
            &c,
            pw_ref,
            &mut failures,
        )?;
        // L：finish（消费式收尾）
        let wreport = writer.finish()?;
        Ok((wreport, failures))
    })();
    // M：fail-fast 回滚 / best_effort 收敛 / 自检
    match result {
        Ok((wreport, failures)) => {
            if c.verify_after
                && let Err(e) = verify_reopen(&c, &wreport)
            {
                tracing::error!(job = ctx.job_id, out = %c.out.display(), err = %e, "压缩自检失败（verify_after）");
                cleanup_failed_output(&c);
                return Err(e);
            }
            tracing::info!(
                job = ctx.job_id,
                entries = wreport.entries_written,
                bytes_in = wreport.bytes_in,
                bytes_out = wreport.bytes_out,
                failed = failures.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "压缩任务完成"
            );
            Ok(finish_outcome(&c, wreport, failures, started))
        }
        Err(e) => {
            tracing::error!(
                job = ctx.job_id,
                out = %c.out.display(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                err = %e,
                "压缩任务失败"
            );
            cleanup_failed_output(&c);
            Err(e)
        }
    }
}

/// writer 创建：直接经 registry handler（组合写/分卷能力由各 fmt 承担）。
///
/// **压缩方式校验的唯一点**：`opts.method` 越出 `caps().methods` 即 HZ-5001。
/// 放在此处而非各 fmt 的 `create_writer` 内，是为了避免五个 handler 各写一份
/// 同样的判断（漏一处 = 某个格式能绕过白名单）。
fn writer_for(
    ctx: &JobCtx,
    c: &CompressSpec,
    sink: Sink,
    opts: &CreateOptions,
) -> Result<Box<dyn ArchiveWriter>, HzipError> {
    let handler =
        ctx.registry
            .handler(c.format.as_str())
            .ok_or_else(|| HzipError::Unsupported {
                what: format!("格式 {} 未注册（feature 门控）", c.format).into(),
            })?;
    if let Some(m) = opts.method {
        let allowed = handler.caps().methods;
        if !allowed.contains(&m) {
            let allow = if allowed.is_empty() {
                "该格式无可选压缩方式".to_string()
            } else {
                allowed
                    .iter()
                    .map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join("/")
            };
            return Err(HzipError::Unsupported {
                what: format!(
                    "hz-5001 压缩方式 {} 不被格式 {} 支持（可选：{allow}）",
                    m.as_str(),
                    c.format
                )
                .into(),
            });
        }
    }
    // 加密方案校验的唯一点（同方法白名单：一处漏 = 某格式可绕过；各 fmt 保留二次防线）
    if let Some(req) = opts.encryption {
        let caps = handler.caps();
        if !caps.write_encryption.contains(&req) {
            let allow = if caps.write_encryption.is_empty() {
                "该格式写入不可加密".to_string()
            } else {
                caps.write_encryption
                    .iter()
                    .map(|r| format!("{r:?}"))
                    .collect::<Vec<_>>()
                    .join("/")
            };
            return Err(HzipError::Unsupported {
                what: format!(
                    "hz-5001 加密方案 {req:?} 不被格式 {} 支持（可选：{allow}）",
                    c.format
                )
                .into(),
            });
        }
    }
    handler.create_writer(sink, opts)
}

/// 写入主循环（§9.5 H~K 逐框）。返回条目失败明细（best_effort 模式填充）。
fn write_all(
    writer: &mut dyn ArchiveWriter,
    inputs: &[WalkedFile],
    stats0: &PreStats,
    ctx: &JobCtx,
    c: &CompressSpec,
    pw: PasswordRef<'_>,
    failures: &mut Vec<hzip_progress::EntryFailure>,
) -> Result<(), HzipError> {
    // H：预读窗口（W = 2×threads；小文件并行读入内存池，写入序恒定）
    //
    // 内存上界 = 窗口内的活跃小文件（≤ W × PREFETCH_SMALL_BYTES）——每条目
    // **写完即释放**（下方 `cache[idx].take()`）。**不得改为长期持有**：滞留会让
    // RSS 随「小文件总字节数」线性增长（实测 2026-09-29：400 × 1 MiB 输入峰值
    // 419 MiB，改为即取即释放后与 100 MiB 输入同档；大文件走流式不受影响）。
    let n = inputs.len();
    let window = prefetch_window(c.threads);
    let mut cache: Vec<Option<std::sync::Arc<Vec<u8>>>> = vec![None; n];
    let mut next_prefetch = 0usize;
    let mut idx = 0usize;
    let mut done_bytes: u64 = 0;
    while idx < n {
        // 预读 [idx .. idx+window) 内的小文件（并行）
        let hi = (idx + window).min(n);
        if next_prefetch < hi {
            // 窗口内条目数已知（`hi - next_prefetch` ≤ W）⇒ 两块 Vec 直给容量：
            // 旧写法每轮从零 `collect`，重填时各自重新增长分配一次（W = 2×threads，
            // 轮次 = n / W；大输入下轮次很多）。元素序与旧写法完全一致。
            let cap = hi - next_prefetch;
            let mut small: Vec<(usize, &Path)> = Vec::with_capacity(cap);
            small.extend(
                inputs[next_prefetch..hi]
                    .iter()
                    .enumerate()
                    .filter_map(|(k, f)| {
                        let i = next_prefetch + k;
                        let small = stats0.total_bytes > 0
                            && std::fs::metadata(&f.abs)
                                .map(|m| m.len() < PREFETCH_SMALL_BYTES)
                                .unwrap_or(false);
                        small.then_some((i, f.abs.as_path()))
                    }),
            );
            // `collect_into_vec`：按索引序写入已备容量的 Vec（等价于旧 `collect`，
            // 但不额外分配中转缓冲）。
            let mut results: Vec<(usize, Option<Vec<u8>>)> = Vec::with_capacity(small.len());
            small
                .par_iter()
                .map(|(i, p)| (*i, std::fs::read(p).ok()))
                .collect_into_vec(&mut results);
            for (i, data) in results {
                cache[i] = data.map(std::sync::Arc::new);
            }
            next_prefetch = hi;
        }
        let f = &inputs[idx];
        // I：EntryMeta（目录零长 / 链接记 target / 元数据映射）
        let meta = entry_meta(f, c);
        // 目录条目零数据
        if is_dir_meta(&meta) {
            let sink = writer
                .next_entry(&meta, pw)
                .map_err(|e| annotate_io(&e, &f.abs))?;
            sink.finish().map_err(|e| annotate_io(&e, &f.abs))?;
            emit_entry_done(ctx, &meta, 0, true);
            idx += 1;
            continue;
        }
        // 符号链接且不跟随：链接条目（数据 = 0；target 已入 meta）
        if meta.is_symlink {
            let sink = writer
                .next_entry(&meta, pw)
                .map_err(|e| annotate_io(&e, &f.abs))?;
            sink.finish().map_err(|e| annotate_io(&e, &f.abs))?;
            emit_entry_done(ctx, &meta, 0, true);
            idx += 1;
            continue;
        }
        // 普通文件（follow=true 时链接走此分支——读目标内容）
        let entry_smol: hzip_progress::EntryName = meta.name.as_str().into();
        let declared = meta.size_raw;
        ctx.progress().send(ProgressEvent::EntryStarted {
            entry: entry_smol.clone(),
            bytes: declared.unwrap_or(0),
        });
        let copy_result = if let Some(buf) = cache[idx].take() {
            // 预读命中：内存直写（零 IO）。`take` 取出即**释放**——本行写完
            // 这个 `Arc` 就落地，缓存里不再留（见函数头注释：滞留会让 RSS
            // 随「小文件总字节数」线性增长）。
            write_from_memory(writer, &meta, &buf, ctx, pw)
        } else {
            // 流式：File → 1 MiB 桥
            write_from_file(writer, &meta, &f.abs, ctx, pw)
        };
        match copy_result {
            Ok(bytes) => {
                done_bytes += bytes;
                ctx.progress().send(ProgressEvent::EntryDone {
                    entry: entry_smol.clone(),
                    bytes,
                    crc_ok: true,
                });
                idx += 1;
            }
            Err(e) if c.best_effort => {
                // 写向 best_effort：跳过失败源继续（记明细）
                let name_smol: hzip_progress::EntryName = meta.name.as_str().into();
                ctx.progress().send(ProgressEvent::EntryDone {
                    entry: entry_smol,
                    bytes: 0,
                    crc_ok: false,
                });
                failures.push(hzip_progress::EntryFailure {
                    entry: name_smol,
                    error: Box::new(e),
                });
                idx += 1;
            }
            Err(e) => return Err(e), // fail-fast
        }
    }
    let _ = done_bytes;
    Ok(())
}

/// 预读窗口宽度：`2 × clamp_threads(threads)`（threads=0 自动 → 核数；02 §6.3）。
fn prefetch_window(threads: u32) -> usize {
    let t = hzip_codec::level::clamp_threads(threads).max(1);
    (2 * t) as usize
}

/// I 节点：EntryMeta 构造规则（02 §9.5：目录=零长、symlink=记 target、
/// mtime/mode/attrs 经 platform::metadata（keep_metadata=false → 置默认））。
fn entry_meta(f: &WalkedFile, c: &CompressSpec) -> EntryMeta {
    // 链接自身的元数据（lstat 语义）
    let lmd = std::fs::symlink_metadata(&f.abs).ok();
    let is_link = lmd
        .as_ref()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    // follow=true：链接按**目标内容**收录，故大小/权限必须取目标元数据——与
    // `open(abs)`（跟随链接）读到的字节数同源。用 lstat 长度会让 `EntrySink`
    // 的「写入字节数 == size_raw」校验失败（链接路径串长度 ≠ 目标内容长度，
    // 如 10 B 链接 → 22 B 内容 → HZ-2004 bad header）。
    let md = if is_link && c.follow_symlinks {
        std::fs::metadata(&f.abs).ok().or_else(|| lmd.clone())
    } else {
        lmd.clone()
    };
    let is_dir = md.as_ref().map(|m| m.is_dir()).unwrap_or(false);
    // 链接条目仅在**不跟随**时成立；follow=true → 目标内容按普通文件（K/J2）
    let is_symlink = is_link && !c.follow_symlinks;
    let size_raw = if is_dir || is_symlink {
        Some(0) // 目录零长；链接数据=0（target 在字段）
    } else {
        md.as_ref().map(|m| m.len())
    };
    let symlink_target = if is_symlink {
        Some(std::fs::read_link(&f.abs).unwrap_or_default())
    } else {
        None
    };
    let (mtime, mode, attrs) = if c.keep_metadata {
        meta_from_walked(f, md.as_ref())
    } else {
        (None, None, hzip_format::EntryAttrs::default())
    };
    EntryMeta {
        name: f.rel.as_str().into(),
        is_dir,
        is_symlink,
        symlink_target,
        size_raw,
        mtime,
        mode,
        attrs,
        // 加密档案仅文件条目置位（目录零长、链接内容公开，不加密）
        encrypted: c.encryption.is_some() && !is_dir && !is_symlink,
    }
}

/// WalkedFile.meta（platform 统一模型，walk 已读）→ EntryMeta 字段。
fn meta_from_walked(
    f: &WalkedFile,
    md: Option<&std::fs::Metadata>,
) -> (Option<OffsetDateTime>, Option<u32>, hzip_format::EntryAttrs) {
    let mtime = f.meta.mtime;
    #[cfg(unix)]
    {
        // unix mode 位：f.meta 无原始 mode——从 fs metadata 补一次（仅权限位；
        // 目录/链接不适用）
        use std::os::unix::fs::PermissionsExt;
        let mode = md
            .filter(|m| m.is_file())
            .map(|m| m.permissions().mode() & 0o7777);
        let read_only = f.meta.read_only;
        let executable = f.meta.executable;
        (
            mtime,
            mode,
            hzip_format::EntryAttrs {
                read_only,
                executable: Some(executable),
                ..Default::default()
            },
        )
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        (
            mtime,
            None,
            hzip_format::EntryAttrs {
                read_only: f.meta.read_only,
                hidden: f.meta.hidden,
                system: f.meta.system,
                executable: Some(f.meta.executable),
            },
        )
    }
}

fn is_dir_meta(meta: &EntryMeta) -> bool {
    meta.is_dir
}

/// J：内存直写（预读命中）。
fn write_from_memory(
    writer: &mut dyn ArchiveWriter,
    meta: &EntryMeta,
    buf: &[u8],
    ctx: &JobCtx,
    pw: PasswordRef<'_>,
) -> Result<u64, HzipError> {
    let mut sink = writer.next_entry(meta, pw)?;
    sink.write_all(buf)?;
    sink.finish()?;
    let _ = ctx;
    Ok(buf.len() as u64)
}

/// J：文件流式拷贝（1 MiB 块 + 每块取消检查点）。
fn write_from_file(
    writer: &mut dyn ArchiveWriter,
    meta: &EntryMeta,
    abs: &Path,
    ctx: &JobCtx,
    pw: PasswordRef<'_>,
) -> Result<u64, HzipError> {
    let mut f = File::open(abs).map_err(|e| HzipError::Io {
        source: e,
        path: Some(abs.to_path_buf()),
    })?;
    let mut sink = writer.next_entry(meta, pw)?;
    let mut buf = vec![0u8; STREAM_CHUNK];
    let mut total = 0u64;
    loop {
        ctx.cancel.throw_if_cancelled()?;
        let n = f.read(&mut buf).map_err(|e| HzipError::Io {
            source: e,
            path: Some(abs.to_path_buf()),
        })?;
        if n == 0 {
            break;
        }
        sink.write_all(&buf[..n])?;
        // 每块输入已写入 → 字节级进度（10 Hz 节流器窗口化；大文件单条目
        // 期间进度条持续刷新——否则整文件压缩期 UI 冻结）
        ctx.progress()
            .send(ProgressEvent::Bytes { delta: n as u64 });
        total += n as u64;
    }
    sink.finish()?;
    Ok(total)
}

/// 进度 EntryDone（目录/链接零数据条目收尾用）。
fn emit_entry_done(ctx: &JobCtx, meta: &EntryMeta, bytes: u64, crc_ok: bool) {
    ctx.progress().send(ProgressEvent::EntryDone {
        entry: meta.name.as_str().into(),
        bytes,
        crc_ok,
    });
}

/// 把底层错误补充源路径上下文（写入侧 IO 错）。
fn annotate_io(e: &HzipError, path: &Path) -> HzipError {
    match e {
        HzipError::Io { source, path: None } => HzipError::Io {
            source: clone_io(source),
            path: Some(path.to_path_buf()),
        },
        _ => e.clone(),
    }
}

fn clone_io(e: &std::io::Error) -> std::io::Error {
    if let Some(c) = e.raw_os_error() {
        std::io::Error::from_raw_os_error(c)
    } else {
        std::io::Error::new(e.kind(), e.to_string())
    }
}

/// 终局收敛：failures 空 → Done；非空 → Partial。stats 回填实际开始时刻
/// （report 聚合新建 → 耗时恒 0 ms，见 [`report::stamp_stats`]）。
fn finish_outcome(
    _c: &CompressSpec,
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

/// 产物扫描登记：Split 卷（fmt 卷路径只在 finish 报告；运行中按 `{prefix}.*`
/// 登记潜在残留）。
#[allow(dead_code)] // fail-fast 清理路径使用（Split 中途失败时 fmt 可能残留 .zNN）
fn tracked_volumes(c: &CompressSpec) -> Vec<PathBuf> {
    if c.split.is_none() {
        return vec![c.out.clone()];
    }
    let mut v = vec![c.out.clone()];
    if let Some(dir) = c.out.parent() {
        let name = c
            .out
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                let fname = p
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if fname.starts_with(&name) && fname != name {
                    v.push(p);
                }
            }
        }
    }
    v
}

/// 失败回滚：删除半写产物（out 文件 / 卷集全部卷）。删除失败仅 warning。
fn cleanup_failed_output(c: &CompressSpec) {
    for p in tracked_volumes(c) {
        if p.exists()
            && let Err(e) = std::fs::remove_file(&p)
        {
            tracing::warn!(path = %p.display(), err = ?e, "失败产物清理 best-effort");
        }
    }
}

/// M：重开自检（compress.verify_after=true → 重开 reader 跑 test()）。
///
/// 分卷产物（zip Sink::Split 两遍法）：fmt 的 volume_paths 报告为空
/// （Splitter 卷清单在 finish 内即弃）——自检按命名约定扫 `{stem}.zNN` 卷集
/// （数值序）+ 末卷（`{prefix}`），装配 `Source::Volumes` 读回。无 `.zNN`
/// （单卷产物）→ 直开末卷。
fn verify_reopen(c: &CompressSpec, wreport: &WriteReport) -> Result<(), HzipError> {
    let src = if c.split.is_some() && !wreport.volume_paths.is_empty() {
        hzip_format::Source::Volumes {
            first: wreport.volume_paths[0].clone(),
            rest: wreport.volume_paths[1..].to_vec(),
        }
    } else if c.split.is_some() {
        split_source_of(&c.out)
    } else {
        hzip_format::Source::File(c.out.clone())
    };
    let mut registry = hzip_format::FormatRegistry::new();
    let _ = crate::engine::register_builtin_handlers(&mut registry);
    // 头加密产物（7z 加密文件名）重开即需密码；条目密码经固定源注入
    let open_opts = hzip_format::OpenOptions {
        password: c.password.clone(),
        ..Default::default()
    };
    let reader = registry.open_reader(src, &open_opts)?;
    let no_pw = NoPwSource;
    let fixed_pw = VerifyPw(c.password.clone());
    let passwords: &dyn PasswordSource = if c.encryption.is_some() {
        &fixed_pw
    } else {
        &no_pw
    };
    let cancel = CancellationToken::new();
    let bus = ProgressBus::new();
    let ectx = ExtractCtx {
        cancel: &cancel,
        progress: &bus,
        passwords,
        conflicts: hzip_format::ConflictPolicy::Overwrite,
        sanitize: &hzip_format::SanitizeConfig::default(),
        quota: &hzip_format::QuotaLimits::default(),
        best_effort: false,
        codepage: None,
    };
    let _ = reader.test(&ectx)?;
    let _ = ectx;
    Ok(())
}

/// 分卷产物卷集装配：目录内 `{stem}.zNN`（数值升序）+ 末卷 `{prefix}`。
///
/// 命名约定（02 §8.1 / zip Splitter）：`{dir}/{stem}.z01 … .zNN` + `{prefix}`
/// （末卷含中央目录/EOCD）。`stem` = 末卷文件名（`vol.zip → vol`）。
fn split_source_of(out: &Path) -> hzip_format::Source {
    let dir = out.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    // 卷前缀 stem = 末卷文件 stem（vol.zip → vol）：`.zNN` 卷与末卷同 stem
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut vols: Vec<(u64, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            // {stem}.zNN：NN 数值
            let Some(rest) = name.strip_prefix(&stem) else {
                continue;
            };
            let Some(num) = rest.strip_prefix(".z") else {
                continue;
            };
            if let Ok(n) = num.parse::<u64>() {
                vols.push((n, p));
            }
        }
    }
    vols.sort_by_key(|(n, _)| *n);
    let mut paths: Vec<PathBuf> = vols.into_iter().map(|(_, p)| p).collect();
    paths.push(out.to_path_buf());
    match paths.split_first() {
        Some((first, rest)) => hzip_format::Source::Volumes {
            first: first.clone(),
            rest: rest.to_vec(),
        },
        None => hzip_format::Source::File(out.to_path_buf()),
    }
}

/// 加密请求与密码**成对**校验（HZ-5003；设计稿 11 §3.6）。`compress_impl`
/// 入口调用——单边携带（有方式无密码 / 有密码无方式）在任何 IO 之前拒绝。
fn validate_encryption_pair(
    encryption: &Option<hzip_format::EncryptionRequest>,
    password: &Option<Zeroizing<String>>,
) -> Result<(), HzipError> {
    match (encryption, password) {
        (Some(_), None) => Err(HzipError::Unsupported {
            what: "hz-5003 设置了加密方式但没有密码（--password-env / HZIP_PASSWORD）".into(),
        }),
        (None, Some(_)) => Err(HzipError::Unsupported {
            what: "hz-5003 提供了密码但加密方式为「不加密」".into(),
        }),
        _ => Ok(()),
    }
}

/// 自检路径的固定密码源（自产加密产物：重开自检复用压缩密码）。
struct VerifyPw(Option<Zeroizing<String>>);

impl PasswordSource for VerifyPw {
    fn password(&self, _entry: Option<&hzip_format::EntryName>) -> Option<Zeroizing<String>> {
        self.0.clone()
    }
}

/// 恒无密码源（自检路径：自产未加密产物）。
struct NoPwSource;

impl PasswordSource for NoPwSource {
    fn password(&self, _entry: Option<&hzip_format::EntryName>) -> Option<Zeroizing<String>> {
        None
    }
}

#[cfg(test)]
#[path = "tests/compress.rs"]
mod tests;
