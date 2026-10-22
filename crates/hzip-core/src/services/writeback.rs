//! 组合/嵌套会话的写回（T-IMG0）：把「内层映像里的一次编辑」逐层冒泡到最外层实体档案。
//!
//! # 三种链形状（`layers` 外→内）
//!
//! | 链 | 例 | 写回怎么做 |
//! |---|---|---|
//! | `[Codec{gz}]` | `disk.qcow2.gz` | 内层编辑后按编码链**全量重压**（`.gz` 无随机写，格式决定的固有代价） |
//! | `[EntryReplace{entry}]` | `v.zip` 里的 `disk.vmdk` | 交给 zip/7z 编辑器 `rewrite(Replace{entry, src=新内层})`——未命中条目无损保留 ⇒ 其它条目**不被重压** |
//! | `[Codec{gz}, TarRebuild{name}]` | `disk.qcow2.tgz` | 先按编码链解出普通 tar ⇒ 整档重建（逐条目原序拷贝 + 替换一条）⇒ 再按编码链压回 |
//!
//! # 原子性（TC-CONF-009）
//!
//! **中间层全部落临时档**，只有最外层做一次
//! [`hzip_platform::fs_util::atomic_replace`]；任一步失败/取消 ⇒ 删临时档、原档字节不变。
//!
//! # 边界
//!
//! 容器层最多一层（容器套容器 = 深度 > 1，T-IMG0 未铺开）⇒ 如实拒绝
//! （`vol.reject.nested-depth`），不静默降级成「只改了临时层」。
//!
//! `layers` 由调用方（GUI 会话）按**嗅探**结果装配：前导 [`WriteBackLayer::Codec`]
//! 来自 `Sniffed::Composite::layers`，容器层来自其 `tail`
//! （`Container(tar)` ⇒ [`WriteBackLayer::TarRebuild`]，`Container(zip/7z)`
//! ⇒ [`WriteBackLayer::EntryReplace`]，`Raw` ⇒ 无容器层）。

use std::path::{Path, PathBuf};

use hzip_error::HzipError;
use hzip_progress::Outcome;

use crate::jobs::{JobCtx, JobSpec};

use super::{edit, reencode};

/// 写回链的一层（外→内）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteBackLayer {
    /// 单文件编码层（`gz`/`bz2`/`xz`/`zst`）：把内层字节重新编码。
    Codec {
        /// 编码格式 id（`"gz"`…）。
        codec: String,
    },
    /// 可编辑容器（zip/7z）：用其编辑器把内层字节替换进 `entry`。
    EntryReplace {
        /// 容器里承载内层内容/映像的条目 id（与浏览行 id 同口径）。
        entry: hzip_format::EntryId,
        /// 条目名（诊断/报告用）。
        entry_name: String,
    },
    /// 无条目编辑器的容器（tar）：整档重建，把 `entry_name` 换成内层字节。
    TarRebuild {
        /// 条目名（整档重建按名匹配）。
        entry_name: String,
    },
}

/// 写回链（外→内）。`layers[0]` = 最外层。
///
/// `spec.source`（本链的「内层」）= 被编辑的那份物化映像；`outer` = 唯一原子替换点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteBackChain {
    /// 最外层实体档案。
    pub outer: PathBuf,
    /// 逐层（外→内）。
    pub layers: Vec<WriteBackLayer>,
}

/// 写回任务（`JobKind::EditBatch` + `JobSpec.write_back` 携带链）。
pub(crate) fn run_write_back(ctx: &JobCtx, spec: &JobSpec) -> Outcome {
    match write_back_impl(ctx, spec) {
        Ok(outcome) => outcome,
        Err(HzipError::Cancelled) => Outcome::Cancelled,
        Err(e) => Outcome::Failed(e),
    }
}

fn write_back_impl(ctx: &JobCtx, spec: &JobSpec) -> Result<Outcome, HzipError> {
    let chain = spec
        .write_back
        .as_ref()
        .ok_or_else(|| HzipError::Unsupported {
            what: "写回链缺失（JobSpec.write_back）".into(),
        })?;
    if chain.layers.is_empty() {
        return Err(HzipError::Unsupported {
            what: "写回链为空（非嵌套会话不该走写回）".into(),
        });
    }
    let changes = spec.changes.clone().unwrap_or_default();
    let inner = spec.source.clone();
    // 临时档守卫：任何路径退出（含 `?` 早退）都删干净；最外层临时档被
    // atomic_replace 改名后自然消失，再删一次是无害 no-op。
    let mut temps = TempGuard::default();
    let target_tmp = temps.adopt(edit::tmp_archive_path(&chain.outer));
    // 进度可观测（用户实测 2026-10-19：点「保存」后进度窗一直停在「准备中…（读取
    // 档案索引）」——那是**还没有任何进度事件**的状态，分不清卡在哪一步）。
    // 每个阶段发 `EntryStarted`（GUI 显示为当前步骤 + 该步字节量）。
    let phase = |entry: &str, bytes: u64| {
        ctx.progress.send(hzip_format::ProgressEvent::EntryStarted {
            entry: entry.into(),
            bytes,
        });
    };
    phase("内层写回（编辑应用到映像）", 0);
    // ① 内层：把变更集应用到物化出来的映像（卷容器 ⇒ 就地写；其余 ⇒ 重写 + 原子替换）。
    //    调用方（GUI）只在链可写时才提交本任务。
    let inner_outcome = edit::rewrite_and_replace(
        ctx,
        &inner,
        &inner,
        spec.password.as_ref(),
        &|| changes.clone(),
        &|infos| infos.first().cloned().ok_or_else(edit::bad_header),
    )?;
    let (codecs, container) = split_layers(&chain.layers)?;
    // ② 容器层（至多一层，且必须在最内）：把内层字节替换进容器 ⇒ 新容器文件。
    //    编码层在外 ⇒ 容器得先「解出来」才是普通文件（zip/7z 编辑器要可乱序读）。
    let mut child = inner;
    if let Some(layer) = container {
        let plain = if codecs.is_empty() {
            chain.outer.clone()
        } else {
            let t = temps.pending(&target_tmp, "container");
            decode_chain(ctx, &chain.outer, &codecs, &t)?;
            temps.adopt(t)
        };
        let child_len = std::fs::metadata(&child).map(|m| m.len()).unwrap_or(0);
        phase(
            &format!("重建外层容器（{}）", chain.outer.display()),
            child_len,
        );
        let rebuilt = if codecs.is_empty() {
            // 无编码层：容器的重建产物就是最终档（同目录同卷 ⇒ 原子替换直用）
            target_tmp.clone()
        } else {
            temps.adopt(temps.pending(&target_tmp, "rebuild"))
        };
        rebuild_container(ctx, spec, &plain, layer, &child, &rebuilt)?;
        child = rebuilt;
    }
    // ③ 编码层：自内向外逐层编码（`codecs` 是外→内，故反序遍历）。
    if !codecs.is_empty() {
        let child_len = std::fs::metadata(&child).map(|m| m.len()).unwrap_or(0);
        phase(&format!("重新编码外层（{} 层）", codecs.len()), child_len);
    }
    let mut report: Option<hzip_format::WriteReport> = None;
    for (ix, codec) in codecs.iter().rev().enumerate() {
        let last = ix + 1 == codecs.len();
        let out = if last {
            target_tmp.clone()
        } else {
            temps.adopt(temps.pending(&target_tmp, "codec"))
        };
        report = Some(reencode::encode_file_with_codec(ctx, codec, &child, &out)?);
        child = out;
    }
    // ④ 收尾：最外层原子替换（中间层从未碰过原档）
    hzip_platform::fs_util::atomic_replace(&target_tmp, &chain.outer).map_err(|e| {
        HzipError::Io {
            source: e,
            path: Some(chain.outer.clone()),
        }
    })?;
    let mut stats = match report.as_ref() {
        Some(r) => crate::report::stats_from_write(r),
        None => match &inner_outcome {
            Outcome::Done(s) => s.clone(),
            _ => return Ok(inner_outcome),
        },
    };
    stats.finished = Some(std::time::Instant::now());
    tracing::info!(
        outer = %chain.outer.display(),
        layers = chain.layers.len(),
        "嵌套写回完成（逐层冒泡 + 一次原子替换）"
    );
    Ok(Outcome::Done(stats))
}

/// 拆链：前导编码层（外→内）+ 至多一个容器层（必须在最后）。
///
/// 容器层之后再出现任何层 = 容器套容器（深度 > 1）⇒ `vol.reject.nested-depth`。
fn split_layers(
    layers: &[WriteBackLayer],
) -> Result<(Vec<String>, Option<&WriteBackLayer>), HzipError> {
    let mut codecs: Vec<String> = Vec::new();
    let mut container: Option<&WriteBackLayer> = None;
    for layer in layers {
        match layer {
            WriteBackLayer::Codec { codec } => {
                if container.is_some() {
                    return Err(nested_depth());
                }
                codecs.push(codec.clone());
            }
            other => {
                if container.is_some() {
                    return Err(nested_depth());
                }
                container = Some(other);
            }
        }
    }
    Ok((codecs, container))
}

fn nested_depth() -> HzipError {
    HzipError::Unsupported {
        what: "vol.reject.nested-depth".into(),
    }
}

/// 逐层解码：`outer` 去掉 `codecs`（外→内）后落成普通文件。中间产物进守卫清理。
fn decode_chain(
    ctx: &JobCtx,
    outer: &Path,
    codecs: &[String],
    out: &Path,
) -> Result<(), HzipError> {
    let mut temps = TempGuard::default();
    let mut cur = outer.to_path_buf();
    for (ix, codec) in codecs.iter().enumerate() {
        let last = ix + 1 == codecs.len();
        let dst = if last {
            out.to_path_buf()
        } else {
            temps.adopt(temps.pending(out, "decode"))
        };
        reencode::decode_file_with_codec(ctx, codec, &cur, &dst)?;
        cur = dst;
    }
    Ok(())
}

/// 容器层重建：把 `child`（已编辑的内层字节）替换进 `plain`（普通容器文件）⇒ `out`。
fn rebuild_container(
    ctx: &JobCtx,
    spec: &JobSpec,
    plain: &Path,
    layer: &WriteBackLayer,
    child: &Path,
    out: &Path,
) -> Result<(), HzipError> {
    match layer {
        WriteBackLayer::EntryReplace { entry, .. } => {
            // 写侧选项按容器自身条目推导（加密档案保持加密算法/文件名加密）
            // 外层容器自己的密码（不是内层 `source` 的）：加密容器的条目表与
            // 未命中条目都要用它读/写回，见 `JobSpec::outer_password`。
            let open_opts = hzip_format::OpenOptions {
                password: spec.outer_password.clone(),
                ..Default::default()
            };
            let reader = ctx
                .registry
                .open_reader(hzip_format::Source::File(plain.to_path_buf()), &open_opts)?;
            let infos = reader.entries()?;
            drop(reader);
            let mut opts = edit::write_opts_for(&infos);
            // 让外层重写的字节事件进任务进度窗（否则「重建外层」整段没有任何
            // 进度事件，界面看起来永远停在「准备中」）
            opts.progress = Some(ctx.progress().clone());
            let changes = hzip_format::ChangeSet {
                changes: vec![hzip_format::Change::Replace {
                    entry: *entry,
                    src: child.to_path_buf(),
                }],
            };
            let wctx = hzip_format::WriteCtx {
                cancel: &ctx.cancel,
                progress: ctx.progress(),
                opts: &opts,
            };
            // 编辑器用完即 drop（Windows 上目标仍被打开时不可替换；此处目标只是临时档）
            let editor = ctx
                .registry
                .open_editor(
                    hzip_format::Source::File(plain.to_path_buf()),
                    &hzip_format::OpenOptions {
                        password: spec.outer_password.clone(),
                        ..Default::default()
                    },
                )
                .map_err(|e| match e {
                    HzipError::Unsupported { .. } => HzipError::Unsupported {
                        what: "vol.readonly.outer-not-editable".into(),
                    },
                    other => other,
                })?;
            editor.rewrite(changes, hzip_format::Sink::File(out.to_path_buf()), &wctx)?;
            Ok(())
        }
        WriteBackLayer::TarRebuild { entry_name } => {
            reencode::rebuild_tar_replacing_entry(ctx, plain, entry_name, child, out)?;
            Ok(())
        }
        WriteBackLayer::Codec { codec } => {
            // split_layers 已把编码层挑走，这里到不了
            reencode::encode_file_with_codec(ctx, codec, child, out)?;
            Ok(())
        }
    }
}

/// 临时档守卫（T-IMG0 §7）：任一路径退出都尽力删；成功路径上最终档已被
/// `atomic_replace` 改名，再删一次是无害 no-op。
#[derive(Default)]
struct TempGuard {
    paths: Vec<PathBuf>,
}

impl TempGuard {
    /// 登记一个临时档（返回同一路径，便于链式使用）。
    fn adopt(&mut self, path: PathBuf) -> PathBuf {
        self.paths.push(path.clone());
        path
    }

    /// 「与最终档同目录同卷」的派生名（中间产物必须能原子替换目标）。
    fn pending(&self, target_tmp: &Path, tag: &str) -> PathBuf {
        let base = target_tmp
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "hzip-tmp".into());
        let dir = target_tmp
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        dir.join(format!("{base}.{tag}-{}", uuid::Uuid::new_v4()))
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_file(p);
        }
    }
}
