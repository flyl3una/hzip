//! 六子命令执行（02 §10.1 分发流程 F~K 节点）。
//!
//! - `c/x/t/cv`：构造 `JobSpec` 经 `Engine::submit`（02 §9.3）；主线程
//!   轮询事件流渲染 + 请求应答（密码三通道 / 冲突决策），终局 `wait`
//!   → 报告 → 退出码；
//! - `l/p`：同步轻量路径（02 §9.10，不占线程池）——`Engine::browse`；
//! - stdin 源：`x/t` 的 `archive=-` 先物化临时文件（core 解压/测试入口要求
//!   File 源，02 §7.4「stdin 需缓冲代理」由 fmt 层对 l/p 直接处理）；
//!   `l/p` 的 `archive=-` 直接 `Source::Stdin`。
//!
//! # 请求-应答轮询（02 §10.1 H 节点）
//!
//! 事件流（`handle.events()`）与请求通道（`handle.requests()`，容量 1）双
//! 路非阻塞轮询 + 10 ms 休眠（worker 侧 need() 以 20 ms 轮询令牌，应答及时
//! 性足够）。终局以 `JobFinished` 事件为哨，之后 `wait()` 收尾。
//!
//! 密码请求：WrongPassword 重试 reason 达 3 次 → `handle.cancel()`（02 §4.4
//! 重试语义双保险；core 侧 PASSWORD_MAX_RETRIES 同样收敛）。
//! 冲突请求：无 TTY（非交互）→ 默认 Skip + warning 直答（TC-CLI-012 不挂起）。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use hzip_core::engine::Engine;
use hzip_core::jobs::{ConflictDecision, JobKind, JobRequest, JobResponse, JobSpec};
use hzip_core::services::compress::CompressSpec;
use hzip_core::services::extract::ExtractOptions;
use hzip_error::HzipError;
use hzip_format::{Encoding, FormatId, OpenOptions, Selection, Source};
use hzip_progress::{Outcome, ProgressEvent};
use zeroize::Zeroizing;

use crate::cli::Cmd;
use crate::password::{PASSWORD_MAX_RETRIES, PasswordResolver};
use crate::render::{self, ProgressPrinter};

/// 子命令执行结果。
pub struct RunOutcome {
    /// 进程退出码（0/1/2/130）
    pub exit_code: i32,
    /// `--json` 模式 stdout 单文档（None = 非 json 或 stdout 被内容占用）
    pub json_doc: Option<String>,
}

/// 分发（02 §10.1 E 节点：子命令 → JobSpec / 同步路径）。
pub fn dispatch(cli: &crate::cli::Cli) -> Result<RunOutcome, HzipError> {
    // D：Engine（cancel_on_signal=true——platform signal 桥接占位期由 core
    // cfg 门控跳过，T11 激活后自动生效；02 §5.3.9）
    let engine = Engine::new(hzip_core::engine::EngineConfig {
        cancel_on_signal: true,
        ..Default::default()
    })?;
    let json = cli.json;
    match &cli.cmd {
        Cmd::Compress {
            format,
            level,
            out,
            password_env,
            encryption,
            no_encrypt_names,
            exclude,
            follow_symlinks,
            split,
            inputs,
        } => {
            if inputs.is_empty() {
                return Err(cli_err("压缩需要至少一个输入路径（HZ-5003）"));
            }
            let out_str = out.to_string_lossy().into_owned();
            if out_str == "-" {
                // stdout 管道压缩：core CompressSpec 恒 Sink::File/Split，无
                // stdout 表达；fmt 层 Sink::Stdout 仅由 core 转换管线间接使用。
                // 显式 Unsupported（02 §8.2 管道输出受限的 CLI 边界透明化）。
                return Err(HzipError::Unsupported {
                    what: "hz-5001 压缩写 stdout（-o -）暂不支持；请指定输出文件路径".into(),
                });
            }
            let resolver = PasswordResolver::new(password_env.clone());
            resolver.preflight()?; // 02 §10.2 C：--password-env 缺失即时退出
            // 加密开启条件：--encryption 或密码通道有值（--password-env /
            // HZIP_PASSWORD）；方式缺省 aes256。密码按三通道取（TTY 隐藏输入两次）。
            let wants_crypto = encryption.is_some()
                || password_env.is_some()
                || std::env::var_os("HZIP_PASSWORD").is_some();
            let mut c = CompressSpec {
                inputs: inputs.clone(),
                format: format.0.clone(),
                out: out.clone(),
                split: split.map(|h| h.0),
                level: level
                    .as_ref()
                    .map(|l| l.0)
                    .unwrap_or(hzip_core::Level::Normal),
                exclude_globs: exclude.iter().map(|g| g.0.clone()).collect(),
                follow_symlinks: *follow_symlinks,
                ..Default::default()
            };
            if wants_crypto {
                use hzip_format::{AesKey, EncryptionRequest};
                // 显式 --encryption 原样采纳（FromStr 已校验取值域；方案是否被
                // 格式支持由 core writer_for 按 caps 再验）；缺省 aes256
                c.encryption = Some(match encryption {
                    Some(a) => a.0,
                    None => EncryptionRequest::Aes(AesKey::Bits256),
                });
                c.password = Some(resolver.resolve_setup_password()?);
                c.encrypt_names = (*no_encrypt_names).then_some(false);
            }
            let mut spec = JobSpec::new(JobKind::Compress, inputs[0].clone());
            spec.compress = Some(c);
            tracing::info!(kind = "compress", out = %out.display(), inputs = inputs.len(), "命令入口");
            run_job(&engine, json, spec, "compress", resolver)
        }
        Cmd::Extract {
            archive,
            out,
            smart,
            codepage,
            select,
            on_conflict,
            symlink,
            password_env,
        } => {
            let names: Vec<_> = select.iter().map(|n| n.name().clone()).collect();
            let opts = ExtractOptions {
                dest_root: out.clone(),
                selection: if names.is_empty() {
                    Selection::All
                } else {
                    Selection::Names(names)
                },
                conflict: on_conflict.0,
                smart: *smart,
                best_effort: false,
                codepage: parse_codepage(codepage.as_deref())?,
                sanitize: hzip_format::SanitizeConfig { symlink: symlink.0 },
                ..Default::default()
            };
            let resolver = PasswordResolver::new(password_env.clone());
            resolver.preflight()?;
            let spec = JobSpec::extract_job(resolve_archive_path(archive)?, opts);
            tracing::info!(kind = "extract", src = %spec.source.display(), dest = %spec.dest.as_deref().map(|d| d.display().to_string()).unwrap_or_default(), "命令入口");
            run_job(&engine, json, spec, "extract", resolver)
        }
        Cmd::Test {
            archive,
            password_env,
        } => {
            let resolver = PasswordResolver::new(password_env.clone());
            resolver.preflight()?;
            let spec = JobSpec::new(JobKind::Test, resolve_archive_path(archive)?);
            tracing::info!(kind = "test", src = %spec.source.display(), "命令入口");
            run_job(&engine, json, spec, "test", resolver)
        }
        Cmd::Convert {
            archive,
            to,
            level,
            out,
        } => {
            let resolver = PasswordResolver::new(None);
            let out_path = out
                .clone()
                .unwrap_or_else(|| default_convert_out(archive, &to.0));
            let lvl = level
                .as_ref()
                .map(|l| l.0)
                .unwrap_or(hzip_core::Level::Normal);
            let c = CompressSpec {
                inputs: vec![archive.clone()],
                format: to.0.clone(),
                out: out_path,
                level: lvl,
                ..Default::default()
            };
            let mut spec = JobSpec::new(JobKind::Convert, archive.clone());
            spec.compress = Some(c);
            tracing::info!(kind = "convert", src = %spec.source.display(), out = %spec.out.as_deref().map(|p| p.display().to_string()).unwrap_or_default(), "命令入口");
            run_job(&engine, json, spec, "convert", resolver)
        }
        Cmd::List {
            archive,
            tree,
            select,
            password_env,
        } => {
            let names: Vec<_> = select.iter().map(|n| n.name().clone()).collect();
            // stdin 源物化临时文件（registry 嗅探会消费头 512 B 且不可重放——
            // 直接 Source::Stdin 对大档截断；CLI 侧统一物化，02 §7.4 语义）
            let path = resolve_archive_path(archive)?;
            tracing::info!(kind = "list", src = %path.display(), "命令入口");
            run_list(&engine, json, &path, *tree, names, password_env.clone())
        }
        Cmd::Preview {
            archive,
            entry,
            max_bytes,
            password_env,
        } => {
            let path = resolve_archive_path(archive)?;
            tracing::info!(kind = "preview", src = %path.display(), entry = %entry.name().as_str(), "命令入口");
            run_preview(
                &engine,
                &path,
                entry.name(),
                *max_bytes,
                password_env.clone(),
            )
        }
    }
}

/// `x/t` 的 stdin `-` → 物化临时文件（core extract/test 仅 File 源；
/// engine.submit 校验 source.exists()）。std:: 临时文件自管清理（不引入
/// tempfile 运行时依赖——CLI 依赖红线外 crate 不新增）。
fn resolve_archive_path(p: &Path) -> Result<PathBuf, HzipError> {
    if p.to_string_lossy() == "-" {
        let dir = std::env::temp_dir();
        let name = format!(
            "hzip-stdin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let path = dir.join(name);
        {
            let mut out = std::fs::File::create(&path).map_err(|e| HzipError::Io {
                source: e,
                path: Some(path.clone()),
            })?;
            std::io::copy(&mut std::io::stdin().lock(), &mut out).map_err(|e| {
                tracing::error!(path = %path.display(), err = %e, "stdin 物化失败");
                HzipError::Io {
                    source: e,
                    path: Some(path.clone()),
                }
            })?;
            out.flush().map_err(|e| HzipError::Io {
                source: e,
                path: Some(path.clone()),
            })?;
        }
        // 进程退出时清理（best-effort；取消路径由进程结束统一回收）
        register_cleanup(path.clone());
        Ok(path)
    } else {
        Ok(p.to_path_buf())
    }
}

/// stdin 物化 spool 注册表（进程退出统一清理）。
static SPOOLS: LazyLock<std::sync::Mutex<Vec<PathBuf>>> =
    LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// 进程退出清理注册（stdin 物化临时文件）。
fn register_cleanup(path: PathBuf) {
    if let Ok(mut v) = SPOOLS.lock() {
        v.push(path);
    }
}

/// main 退出前调用：删除 stdin 物化临时文件（best-effort）。
pub fn cleanup_spools() {
    if let Ok(mut v) = SPOOLS.lock() {
        for p in v.drain(..) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 任务主循环（02 §10.1 H~K）。
fn run_job(
    engine: &Engine,
    json: bool,
    spec: JobSpec,
    command: &'static str,
    mut resolver: PasswordResolver,
) -> Result<RunOutcome, HzipError> {
    let started = std::time::Instant::now();
    tracing::info!(
        kind = command,
        source = %spec.source.display(),
        dest = ?spec.dest.as_deref().or(spec.out.as_deref()),
        "任务提交"
    );
    let handle = match engine.submit(spec) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(kind = command, err = %e, "任务提交失败");
            return Err(e);
        }
    };
    let job_id = handle.job_id();
    let req_rx = handle.requests().clone();
    let events = handle.events();
    let mut printer = ProgressPrinter::new(json, command.to_string());
    let mut pw_retries: u8 = 0;
    let mut observed_entries: u64 = 0;
    let mut finished = false;
    while !finished {
        // 事件流（含 JobFinished 哨）
        while let Some(ev) = events.try_recv() {
            if matches!(ev, ProgressEvent::EntryDone { .. }) {
                observed_entries += 1;
            }
            printer.push(&ev);
            if matches!(ev, ProgressEvent::JobFinished { .. }) {
                finished = true;
            }
        }
        // 请求应答
        if let Ok(req) = req_rx.try_recv() {
            match &req {
                JobRequest::Password { reason, .. } => {
                    match resolver.resolve(&req) {
                        Ok(Some(resp)) => {
                            if matches!(reason, hzip_core::jobs::PasswordReason::WrongPassword) {
                                pw_retries += 1;
                            }
                            // 应答竞态（worker 已取消/终局 → 5003 stale）：容忍，
                            // 终局以 wait() 为准（02 §9.11 answer 校验语义）
                            let _ = handle.answer(0, resp);
                        }
                        Ok(None) => {
                            // EOF/通道不可答 → 取消任务（02 §4.4 重试超限语义）
                            handle.cancel();
                        }
                        Err(e) => {
                            // 密码通道读取失败（IO 类）→ 取消任务并记录（不 panic）
                            tracing::error!(err = %e, "密码读取失败（stdin/stdenv 通道），任务取消");
                            eprintln!("密码读取失败: {e}");
                            handle.cancel();
                        }
                    }
                }
                JobRequest::Conflict { .. } => {
                    // 非交互（无 TTY）按默认 Skip + warning 直答（TC-CLI-012）；
                    // 交互终端亦默认 Skip（本版无逐条 Ask UI——02 §9.8 超时默认
                    // Skip 同语义，透明告知）
                    if !hzip_platform::consts::stdin_is_tty() {
                        eprintln!("警告: 目标已存在且 stdin 非交互 → 跳过冲突条目（默认 Skip）");
                    }
                    let _ = handle.answer(0, JobResponse::Conflict(ConflictDecision::Skip));
                }
                _ => {} // 未知请求变体：忽略（worker 超时路径默认 Skip）
            }
            if pw_retries >= PASSWORD_MAX_RETRIES {
                handle.cancel();
            }
        }
        if !finished {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // 终局（events 已见 JobFinished；wait 阻塞直到落账）
    let result = handle.wait();
    let exit_code = crate::exit::exit_code_of_result(&result);
    match &result {
        Ok(outcome) => {
            let entries = observed_entries.max(match outcome {
                Outcome::Done(s) | Outcome::Partial(s, _) => s.entries_done,
                _ => 0,
            });
            tracing::info!(
                kind = command,
                job = job_id,
                ok = exit_code == 0,
                entries,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "任务完成"
            );
        }
        Err(e) => tracing::error!(
            kind = command,
            job = job_id,
            err = %e,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "任务失败"
        ),
    }
    let doc = if json {
        build_json_doc(command, &result, exit_code, observed_entries)
    } else {
        report_human(command, &result, observed_entries);
        None
    };
    Ok(RunOutcome {
        exit_code,
        json_doc: doc,
    })
}

/// 人读报告（02 §10.1 J；错误明细 stderr，统计 stdout）。
///
/// `observed_entries`：事件流 EntryDone 计数（进度权威来源）。zip fmt 写侧
/// WriteReport 对普通文件条目漏计 entries_written（T24 实现缺陷，core/fmt
/// 只读不可改）——报告条目数以事件流观测值为准（>0 时覆盖 stats 值）。
fn report_human(command: &str, result: &Result<Outcome, HzipError>, observed_entries: u64) {
    let mut out = std::io::stdout().lock();
    let entries_of = |s: &hzip_progress::JobStats| {
        if observed_entries > 0 {
            observed_entries
        } else {
            s.entries_done
        }
    };
    match result {
        Ok(Outcome::Done(s)) => {
            let _ = writeln!(
                out,
                "{} 完成：{}",
                command,
                render::stats_line(entries_of(s), s.bytes_total, s.bytes_done, duration_ms(s))
            );
        }
        Ok(Outcome::Partial(s, fs)) => {
            let _ = writeln!(
                out,
                "{} 部分完成（{} 成功 / {} 失败）：{}",
                command,
                entries_of(s),
                fs.len(),
                render::stats_line(entries_of(s), s.bytes_total, s.bytes_done, duration_ms(s))
            );
            for f in fs {
                eprintln!("  ✗ {}: {}", f.entry, f.error);
            }
        }
        Ok(Outcome::Cancelled) => {
            eprintln!("任务已取消（HZ-6001）");
        }
        Ok(Outcome::RolledBack(paths)) => {
            eprintln!("任务已取消并回滚（{} 个半写文件已清理）", paths.len());
        }
        Ok(Outcome::Failed(e)) => {
            eprintln!("错误 HZ-{:04}: {e}", e.code());
            eprintln!("（{}）", crate::i18n::err_text(e.code()));
        }
        Ok(_) => {
            // #[non_exhaustive] 未知 Outcome 变体 → 内部错误兜底（HZ-0000）
            eprintln!("错误 HZ-0000: 未知任务终局（请附带日志上报 issue）");
        }
        Err(e) => {
            eprintln!("错误 HZ-{:04}: {e}", e.code());
            eprintln!("（{}）", crate::i18n::err_text(e.code()));
        }
    }
}

/// `--json` 结果文档：c/x/cv/t 统一 JobResultJson（command 区分；
/// stats=None 于失败/取消；failures 承载 Partial 与 Failed 明细）。
fn build_json_doc(
    command: &str,
    result: &Result<Outcome, HzipError>,
    exit_code: i32,
    observed_entries: u64,
) -> Option<String> {
    let doc = match result {
        Ok(o) => fixup_stats_json(
            render::job_result_json(command, exit_code, o),
            observed_entries,
        ),
        Err(e) => crate::json_schema::JobResultJson {
            command: command.into(),
            exit_code,
            stats: None,
            failures: vec![crate::json_schema::FailureJson {
                entry: "".into(),
                code: e.code(),
                message: e.to_string(),
            }],
        },
    };
    serde_json::to_string(&doc).ok()
}

/// 以事件流观测条目数覆盖 stats（zip fmt WriteReport 漏计修正；见 report_human
/// rustdoc）。仅当观测值有效且与 stats 不同时覆盖。
fn fixup_stats_json(
    mut doc: crate::json_schema::JobResultJson,
    observed_entries: u64,
) -> crate::json_schema::JobResultJson {
    if observed_entries > 0
        && let Some(st) = doc.stats.as_mut()
        && st.entries < observed_entries
    {
        st.entries = observed_entries;
    }
    doc
}

fn duration_ms(s: &hzip_progress::JobStats) -> u64 {
    let started = s.started;
    s.finished
        .map(|f| f.duration_since(started).as_millis() as u64)
        .unwrap_or_else(|| started.elapsed().as_millis() as u64)
}

/// l/p 打开（同步路径自理密码链）：头加密（Password 错）经三通道取密码、
/// 带 `OpenOptions::password` 重开（≤ `PASSWORD_MAX_RETRIES`；与 x 的 core
/// `open_archive_retry` 同语义——重问标注 `WrongPassword`）。
fn browse_with_password(
    engine: &Engine,
    archive: &Path,
    resolver: &mut PasswordResolver,
) -> Result<
    (
        hzip_core::services::browse::Browser,
        Option<Zeroizing<String>>,
    ),
    HzipError,
> {
    let mut opts = OpenOptions::default();
    let mut reason = hzip_core::jobs::PasswordReason::Initial;
    let mut attempt = 0u8;
    loop {
        match engine.browse(list_source(archive)?, &opts) {
            Ok(b) => return Ok((b, opts.password)),
            Err(e @ HzipError::Password { .. }) if attempt < PASSWORD_MAX_RETRIES => {
                attempt += 1;
                let req = JobRequest::Password {
                    entry: None,
                    reason,
                };
                match resolver.resolve(&req)? {
                    Some(JobResponse::Password(pw)) => {
                        opts.password = Some(pw);
                        reason = hzip_core::jobs::PasswordReason::WrongPassword;
                    }
                    _ => return Err(e), // 通道不可答/重试超限 → 传播 3101/3102
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// l 子命令：browse 同步轻量（02 §9.10）。
fn run_list(
    engine: &Engine,
    json: bool,
    archive: &Path,
    tree: bool,
    select: Vec<hzip_format::EntryName>,
    password_env: Option<String>,
) -> Result<RunOutcome, HzipError> {
    let started = std::time::Instant::now();
    let mut resolver = PasswordResolver::new(password_env);
    resolver.preflight()?; // --password-env 缺失即时退出（02 §10.2 C）
    let (browser, _pw) = match browse_with_password(engine, archive, &mut resolver) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(src = %archive.display(), err = %e, "档案打开失败（list）");
            return Err(e);
        }
    };
    let entries = browser.entries();
    let filtered: Vec<&hzip_format::EntryInfo> = if select.is_empty() {
        entries
    } else {
        entries
            .into_iter()
            .filter(|e| select.iter().any(|n| n.as_str() == e.name.as_str()))
            .collect()
    };
    if json {
        let format = sniff_display(engine, archive);
        let list = crate::json_schema::ListResultJson {
            format,
            entry_count: filtered.len() as u64,
            entries: filtered.iter().map(|e| render::entry_json(e)).collect(),
            truncated: false,
        };
        return Ok(RunOutcome {
            exit_code: 0,
            json_doc: serde_json::to_string(&list).ok(),
        });
    }
    let mut text = String::new();
    if tree {
        let root = tree_of(&filtered);
        render::render_tree(&root, &mut text);
    } else {
        for e in &filtered {
            text.push_str(&render::entry_line(e));
            text.push('\n');
        }
    }
    {
        let mut so = std::io::stdout().lock();
        let _ = so.write_all(text.as_bytes());
        let _ = so.flush();
    }
    tracing::info!(
        kind = "list",
        ok = true,
        entries = filtered.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "命令完成"
    );
    Ok(RunOutcome {
        exit_code: 0,
        json_doc: None,
    })
}

/// p 子命令：条目内容直出 stdout（TC-CLI-004：字节相等 / --max-bytes 截断）。
fn run_preview(
    engine: &Engine,
    archive: &Path,
    entry_name: &hzip_format::EntryName,
    max_bytes: Option<u64>,
    password_env: Option<String>,
) -> Result<RunOutcome, HzipError> {
    let mut resolver = PasswordResolver::new(password_env);
    resolver.preflight()?; // --password-env 缺失即时退出（02 §10.2 C）
    let (browser, pw) = match browse_with_password(engine, archive, &mut resolver) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(src = %archive.display(), err = %e, "档案打开失败（preview）");
            return Err(e);
        }
    };
    let id = browser
        .entries()
        .into_iter()
        .find(|e| e.name.as_str() == entry_name.as_str())
        .map(|e| e.id)
        .ok_or_else(|| {
            tracing::error!(src = %archive.display(), entry = %entry_name.as_str(), "预览条目不存在");
            HzipError::Unsupported {
                what: format!("hz-5003 条目不存在: {entry_name}").into(),
            }
        })?;
    let data = browser.preview(
        id,
        hzip_core::services::preview::PreviewMode::Auto,
        max_bytes,
        match &pw {
            Some(p) => hzip_format::PasswordRef::new(p),
            None => hzip_format::PasswordRef::none(),
        },
    )?;
    let mut so = std::io::stdout().lock();
    so.write_all(&data.bytes).map_err(|e| HzipError::Io {
        source: e,
        path: None,
    })?;
    so.flush().map_err(|e| HzipError::Io {
        source: e,
        path: None,
    })?;
    tracing::info!(
        kind = "preview",
        ok = true,
        bytes = data.bytes.len(),
        "命令完成"
    );
    Ok(RunOutcome {
        exit_code: 0,
        json_doc: None,
    })
}

/// l/p 的档案源（stdin 已由 resolve_archive_path 物化为临时文件——
/// 恒 File 源；02 §7.4「stdin 需缓冲代理」由 CLI 承担）。
fn list_source(p: &Path) -> Result<Source, HzipError> {
    Ok(Source::File(p.to_path_buf()))
}

/// 列表 JSON 的 format 描述（嗅探）。
fn sniff_display(engine: &Engine, archive: &Path) -> String {
    let src = hzip_format::Source::File(archive.to_path_buf());
    engine
        .registry()
        .sniff(&src)
        .map(|s| s.describe())
        .unwrap_or_else(|_| "unknown".into())
}

/// 平铺条目 → TreeNode（select 过滤后；02 §9.10）。
fn tree_of(entries: &[&hzip_format::EntryInfo]) -> hzip_core::services::browse::TreeNode {
    hzip_core::services::browse::TreeNode::build(entries)
}

/// codepage → Encoding（02 §8.1；未知 → 5003）。
fn parse_codepage(s: Option<&str>) -> Result<Option<Encoding>, HzipError> {
    let Some(s) = s else { return Ok(None) };
    let enc = match s.to_ascii_lowercase().as_str() {
        "gbk" | "gb18030" | "cp936" => Encoding::Gbk,
        "big5" | "cp950" => Encoding::Big5,
        "shift_jis" | "sjis" | "cp932" => Encoding::ShiftJis,
        "euc-kr" | "cp949" => Encoding::EucKr,
        "cp1252" | "windows-1252" => Encoding::Cp1252,
        "utf-8" | "utf8" => Encoding::Utf8,
        _ => {
            return Err(HzipError::Unsupported {
                what: format!(
                    "hz-5003 未知编码 {s}（可选 gbk/big5/shift_jis/euc-kr/cp1252/utf-8）"
                )
                .into(),
            });
        }
    };
    Ok(Some(enc))
}

/// cv 默认输出：`{stem}.{to}`。
fn default_convert_out(archive: &Path, to: &FormatId) -> PathBuf {
    let stem = archive
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    archive
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{stem}.{}", to.as_str()))
}

/// 参数错误辅助（5003 语义；文案不 i18n——冻结退出码，文案内联中文）。
pub(crate) fn cli_err(msg: &str) -> HzipError {
    HzipError::Unsupported {
        what: msg.to_string().into(),
    }
}
