//! 重新编码助手（T-IMG0：被包裹的磁盘镜像写回的三件机械件）。
//!
//! 三个自包含助手，由 core 编辑管线（`services/composite_edit.rs` 的写回链）
//! 逐层调用——形状 1（外层是编码层）与形状 2（外层是容器条目）两条路都只需要
//! 「解一层 / 压一层 / 换一个条目」这三种动作：
//!
//! ```text
//! 读方向（打开被包裹的镜像）：decode_file_with_codec(ctx, "gz", src, tmp)
//!   编码档 → 普通临时文件（内层容器/镜像从它打开）
//! 写方向-形状 1（外层编码层，如 a.qcow2.gz）：
//!   内层写回产物（普通临时文件）→ encode_file_with_codec(ctx, "gz", tmp, dst)
//! 写方向-形状 2（外层是**无编辑器**的容器 = 普通 tar，如 a.img.tar）：
//!   内层写回产物 → rebuild_tar_replacing_entry(ctx, outer, entry, tmp, dst)
//!   （zip/7z 有 `ArchiveEditor`，不走本助手；tar/cpio/rar 无编辑器 ⇒ 链只读）
//! ```
//!
//! # 语义与边界（rustdoc）
//!
//! - **明文前提**：调用方保证外层未加密（或已在更外层解掉密码）。`entries()`
//!   里出现 `encrypted` 条目 → `Err Unsupported`（本层不猜密码、不静默产出乱码）。
//!   解析不出「可解码条目」的单流源同样拒绝。
//! - **取消/进度**：每个 1 MiB 拷贝循环先查 `ctx.cancel`（`HzipError::Cancelled`
//!   语义与 `convert::bridge` 一致），每块发 `ProgressEvent::Bytes{delta}`（用法同
//!   `hzip-fmt-single::reader` / `compress::write_from_file`）。
//! - **失败回滚**：任一失败/取消 → 删掉写了一半的 `out`（`convert.rs::cleanup_failed`
//!   同款「产物不可用即整体作废」；删除失败仅 warning，不掩盖原错）。
//! - **元数据保真**：`EntryMeta` 逐字段映射同 `convert.rs`（name/is_dir/is_symlink/
//!   symlink_target/mtime/mode/attrs）。目录与符号链接按读侧信息直通（零数据条目），
//!   被替换条目沿用原名与元数据，**只有** `size_raw` 随新内容更新——预声明大小必须
//!   等于实写字节，否则 `EntrySink::finish` 在 fmt 层报 BadHeader。
//! - **不做的事**：不碰加密外层、不装配组合链、不管 `attr`/同名冲突——那是调用方
//!   （`composite_edit`）与 fmt handler 的职责。
//!
//! 出处：`docs/tasks/T-IMG/T-IMG0-组合与嵌套写回.md` §1（两种形状）/§5.2（逐层
//! 冒泡）；模板 = `services/convert.rs:139-200`（逐条目读→写循环）+
//! `services/compress.rs`（单文件编码路径 `writer_for` 一段拿 handler/CreateOptions）。

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use hzip_error::HzipError;
use hzip_format::{
    CreateOptions, EntryInfo, EntryMeta, FormatRegistry, OpenOptions, PasswordRef, Sink, Source,
    WriteReport,
};
use hzip_progress::{CancellationToken, ProgressBus, ProgressEvent};

use crate::jobs::JobCtx;

/// 拷贝块（1 MiB；与 `hzip-format` 契约常量 `COPY_CHUNK_BYTES` 同值）。
const STREAM_CHUNK: usize = 1024 * 1024;

/// 普通 tar 的格式 ID（形状 2 的外层落点：无 `ArchiveEditor` 的容器）。
const TAR_ID: &str = "tar";

/// 助手内部上下文 = [`JobCtx`] 的三件套解构（registry / 取消令牌 / 进度总线）。
///
/// 存在的理由：这三个助手是**纯机械**的——它们只用到 `JobCtx` 的 `registry`、
/// `cancel`、`progress()` 三个公开面，而 `JobCtx` 的通道字段是 `jobs::manager`
/// 私有、仓库里没有（也不该有）测试构造器。单测直造本结构即可覆盖全部助手逻辑
/// （见 `tests/reencode.rs`），不必为一个机械件起整个引擎。
///
/// 公开入口（[`encode_file_with_codec`] 等）签名逐字照契约，只做 `of(ctx)` 转发。
struct HelperCtx<'a> {
    /// 格式注册表（handler/open_reader/create_writer 入口）
    registry: &'a FormatRegistry,
    /// 取消令牌（每 1 MiB 一查）
    cancel: &'a CancellationToken,
    /// 进度总线（`Bytes` 事件）
    progress: &'a ProgressBus,
}

impl<'a> HelperCtx<'a> {
    /// 从任务上下文解出三件套。
    fn of(ctx: &'a JobCtx) -> Self {
        Self {
            registry: ctx.registry.as_ref(),
            cancel: &ctx.cancel,
            progress: ctx.progress(),
        }
    }
}

/// 用单文件编码格式（gz/bz2/xz/zst）把 `src` 压成 `out`（新文件）。
///
/// `out` 由本函数创建（存在则截断）；失败/取消时删除。条目名取 `src` 的文件名
/// （gz FNAME 语义：解压端看到的即是内层档名，与 `compress::composite_temp_path`
/// 的中间 tar 命名口径一致），`size_raw` 取 `src` 的当前长度（预声明大小）。
pub(crate) fn encode_file_with_codec(
    ctx: &crate::jobs::JobCtx,
    codec: &str,
    src: &std::path::Path,
    out: &std::path::Path,
) -> Result<hzip_format::WriteReport, hzip_error::HzipError> {
    encode_file_impl(&HelperCtx::of(ctx), codec, src, out)
}

/// 用单文件编码格式把 `src`（编码文件）解成普通文件 `out`（新文件）。
///
/// `out` 由本函数创建（存在则截断）；失败/取消时删除。
pub(crate) fn decode_file_with_codec(
    ctx: &crate::jobs::JobCtx,
    codec: &str,
    src: &std::path::Path,
    out: &std::path::Path,
) -> Result<(), hzip_error::HzipError> {
    decode_file_impl(&HelperCtx::of(ctx), codec, src, out)
}

/// 重建一个**普通 tar 文件**：逐条目原序拷贝，把名为 `replace_name` 的条目内容
/// 换成 `child` 的字节；其余条目字节/元数据保持；条目不存在 → Err。
///
/// `child` 是**已写好的普通文件**（内层写回产物）；`out` 是新 tar 的落点
/// （失败/取消时删除）。目录/符号链接条目按读侧元数据直通（零数据）；
/// `replace_name` 命中目录或链接 → Err（那是「换内容」而不是「换条目」）。
pub(crate) fn rebuild_tar_replacing_entry(
    ctx: &crate::jobs::JobCtx,
    tar_plain: &std::path::Path,
    replace_name: &str,
    child: &std::path::Path,
    out: &std::path::Path,
) -> Result<hzip_format::WriteReport, hzip_error::HzipError> {
    rebuild_tar_impl(&HelperCtx::of(ctx), tar_plain, replace_name, child, out)
}

// ===================== 实现体（ctx 三件套入参；公开入口只做转发） =====================

/// 编码：`src`（普通文件）→ `out`（单流编码档）。
fn encode_file_impl(
    ctx: &HelperCtx<'_>,
    codec: &str,
    src: &Path,
    out: &Path,
) -> Result<WriteReport, HzipError> {
    let handler = ctx
        .registry
        .handler(codec)
        .ok_or_else(|| HzipError::Unsupported {
            what: format!("编码格式 {codec} 未注册（feature 门控）").into(),
        })?;
    if !handler.caps().write {
        return Err(HzipError::Unsupported {
            what: format!("格式 {codec} 不支持写出（caps.write=false，HZ-5001）").into(),
        });
    }
    let size = std::fs::metadata(src)
        .map_err(|e| HzipError::Io {
            source: e,
            path: Some(src.to_path_buf()),
        })?
        .len();
    let meta = EntryMeta {
        name: entry_name_of(src),
        size_raw: Some(size), // 预声明大小（fmt 层据此写容器头）
        ..Default::default()
    };
    let mut writer =
        handler.create_writer(Sink::File(out.to_path_buf()), &CreateOptions::default())?;
    let copied = (|| -> Result<(), HzipError> {
        let mut f = File::open(src).map_err(|e| HzipError::Io {
            source: e,
            path: Some(src.to_path_buf()),
        })?;
        let mut sink = writer.next_entry(&meta, PasswordRef::none())?;
        copy_loop(&mut f, &mut sink, ctx, out)?;
        sink.finish()
    })();
    match copied {
        Ok(()) => writer.finish().inspect_err(|_| cleanup_failed(out)),
        Err(e) => {
            cleanup_failed(out);
            Err(e)
        }
    }
}

/// 解码：`src`（单流编码档）→ `out`（普通文件）。
fn decode_file_impl(
    ctx: &HelperCtx<'_>,
    codec: &str,
    src: &Path,
    out: &Path,
) -> Result<(), HzipError> {
    let handler = ctx
        .registry
        .handler(codec)
        .ok_or_else(|| HzipError::Unsupported {
            what: format!("解码格式 {codec} 未注册（feature 门控）").into(),
        })?;
    let reader = handler.open_reader(Source::File(src.to_path_buf()), &OpenOptions::default())?;
    let entries = reader.entries()?;
    ensure_plain(&entries)?;
    // 单流源恰一条目（gz/bz2/xz/zst 容器无目录/链接概念）：取第一条可解码文件条目
    let info = entries
        .iter()
        .find(|e| !e.is_dir && !e.is_symlink)
        .ok_or_else(|| HzipError::Unsupported {
            what: format!("{codec} 源里没有可解码的文件条目").into(),
        })?;
    let copied = (|| -> Result<(), HzipError> {
        let mut dec = reader.read_entry(info.id, PasswordRef::none())?;
        let mut f = File::create(out).map_err(|e| HzipError::Io {
            source: e,
            path: Some(out.to_path_buf()),
        })?;
        copy_loop(&mut dec, &mut f, ctx, out).map(|_| ())
    })();
    match copied {
        Ok(_) => Ok(()),
        Err(e) => {
            cleanup_failed(out);
            Err(e)
        }
    }
}

/// tar 条目替换重建（形状 2）。条目序 = 读侧原序。
fn rebuild_tar_impl(
    ctx: &HelperCtx<'_>,
    tar_plain: &Path,
    replace_name: &str,
    child: &Path,
    out: &Path,
) -> Result<WriteReport, HzipError> {
    let handler = ctx
        .registry
        .handler(TAR_ID)
        .ok_or_else(|| HzipError::Unsupported {
            what: format!("格式 {TAR_ID} 未注册（feature 门控）").into(),
        })?;
    let reader = handler.open_reader(
        Source::File(tar_plain.to_path_buf()),
        &OpenOptions::default(),
    )?;
    let entries = reader.entries()?;
    ensure_plain(&entries)?;
    let target = entries
        .iter()
        .find(|e| e.name.as_str() == replace_name)
        .ok_or_else(|| HzipError::Unsupported {
            what: format!(
                "条目 {replace_name} 不在 {} 中（无法替换）",
                tar_plain.display()
            )
            .into(),
        })?;
    if target.is_dir || target.is_symlink {
        return Err(HzipError::Unsupported {
            what: format!("条目 {replace_name} 是目录或符号链接，不能替换其内容").into(),
        });
    }
    let target_id = target.id;
    // 新内容长度：被替换条目的 size_raw 必须随之更新（预声明 = 实写，否则 fmt 报 BadHeader）
    let child_len = std::fs::metadata(child)
        .map_err(|e| HzipError::Io {
            source: e,
            path: Some(child.to_path_buf()),
        })?
        .len();
    let mut writer =
        handler.create_writer(Sink::File(out.to_path_buf()), &CreateOptions::default())?;
    let copied = (|| -> Result<(), HzipError> {
        for info in &entries {
            let is_target = info.id == target_id;
            let mut meta = meta_of(info);
            if is_target {
                meta.size_raw = Some(child_len);
            }
            let mut sink = writer.next_entry(&meta, PasswordRef::none())?;
            if info.is_dir || info.is_symlink {
                // 零数据条目（目录/链接）：元数据直通，内容在头里
                sink.finish()?;
                continue;
            }
            if is_target {
                let mut f = File::open(child).map_err(|e| HzipError::Io {
                    source: e,
                    path: Some(child.to_path_buf()),
                })?;
                copy_loop(&mut f, &mut sink, ctx, out)?;
            } else {
                let mut r = reader.read_entry(info.id, PasswordRef::none())?;
                copy_loop(&mut r, &mut sink, ctx, out)?;
            }
            sink.finish()?;
        }
        Ok(())
    })();
    match copied {
        Ok(()) => writer.finish().inspect_err(|_| cleanup_failed(out)),
        Err(e) => {
            cleanup_failed(out);
            Err(e)
        }
    }
}

// ===================== 公共小件 =====================

/// 1 MiB 桥接循环（流式不落盘）：每块先查取消、再读、写完发字节进度。
///
/// `err_path` 只用于读取侧 IO 错附路径上下文（写入侧错误经 fmt 的错误语义原样上抛，
/// 见 [`HzipError`] 的 `From<io::Error>` 例外条）。
fn copy_loop(
    r: &mut dyn Read,
    w: &mut dyn Write,
    ctx: &HelperCtx<'_>,
    err_path: &Path,
) -> Result<u64, HzipError> {
    let mut buf = vec![0u8; STREAM_CHUNK];
    let mut total = 0u64;
    loop {
        ctx.cancel.throw_if_cancelled()?;
        let n = r.read(&mut buf).map_err(|e| HzipError::Io {
            source: e,
            path: Some(err_path.to_path_buf()),
        })?;
        if n == 0 {
            return Ok(total);
        }
        w.write_all(&buf[..n])?;
        ctx.progress.send(ProgressEvent::Bytes { delta: n as u64 });
        total += n as u64;
    }
}

/// 明文前提校验：本助手的三个函数都只处理未加密输入。
fn ensure_plain(entries: &[EntryInfo]) -> Result<(), HzipError> {
    match entries.iter().find(|e| e.encrypted) {
        Some(e) => Err(HzipError::Unsupported {
            what: format!(
                "条目 {} 加密：本助手只处理明文（调用方须先解码外层或提供明文外层）",
                e.name
            )
            .into(),
        }),
        None => Ok(()),
    }
}

/// `EntryInfo` → `EntryMeta`（字段逐个直通，口径同 `convert.rs` 的映射）。
///
/// `encrypted` 恒 `false`：明文前提由 [`ensure_plain`] 在入口守住（重编码产物
/// 一律明文——外层加密由调用方在更外层承担）。
fn meta_of(info: &EntryInfo) -> EntryMeta {
    EntryMeta {
        name: info.name.clone(),
        is_dir: info.is_dir,
        is_symlink: info.is_symlink,
        symlink_target: info.symlink_target.clone(),
        size_raw: Some(info.size_raw),
        mtime: info.mtime,
        mode: info.mode,
        attrs: info.attributes,
        encrypted: false,
    }
}

/// 单流编码档的条目名：取源文件名（gz FNAME）；无文件名时退化为 `data`。
fn entry_name_of(src: &Path) -> hzip_format::EntryName {
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("data");
    hzip_format::EntryName::new(name)
}

/// 失败/取消清理：删掉写了一半的产物（产物不可用即整体作废）。
///
/// 删除失败仅 warning（清理是 best-effort，不得掩盖原错）——同
/// `convert.rs::cleanup_failed` / `compress.rs::cleanup_failed_output`。
fn cleanup_failed(out: &Path) {
    if out.exists()
        && let Err(e) = std::fs::remove_file(out)
    {
        tracing::warn!(path = %out.display(), err = ?e, "重编码产物清理 best-effort 失败");
    }
}

#[cfg(test)]
#[path = "tests/reencode.rs"]
mod tests;
