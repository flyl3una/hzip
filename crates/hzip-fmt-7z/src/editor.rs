//! 7z 编辑：`ArchiveEditor::rewrite`（流式重写，02 §7.3/§9.9；TC-CONF-009）。
//!
//! `open_editor` 时以只读打开源并解析头（不触碰数据区、不改源文件）；
//! `rewrite`：遍历旧条目 → 以 `ChangeSet` 应用删除/替换/改名 → 追加变更的新
//! 条目（含 `MakeDir` 空目录）→ 逐条目拷贝/重写进**新档案** `dst`。`dst` 为临时
//! 目标路径（core 随后 `atomic_replace`，02 §9.9）；本实现**不触碰源档案**
//! （原子性前提：TC-CONF-009 注入写错误时源字节不变由本层保证——源只读打开且
//! 从不写回）。
//!
//! 属性保留：旧条目经 `entries()` 得 `EntryInfo`，拷贝时携带既有 mode/mtime/attrs；
//! symlink 条目内容（链接目标字符串）经 `read_entry` 读回并重放（7z 属性表仅
//! 标记 `S_IFLNK`，目标在数据区）；目录条目为元数据+空流，不重压缩数据。
//!
//! # 改名与目录（用户口径 2026-09-28）
//!
//! `Change::Rename{from,to}` 按**名**匹配（不是 id）：先对全部旧条目建 `name →
//! new_name` 映射，再在拷贝循环替换 `meta.name`。`from` 命中同名条目（含目录条目）
//! 时改该条目；命中以 `from + '/'` 为前缀的条目（隐式目录子树）时整段前缀替换为
//! `to + '/'`。两者皆未命中 → `Format{BadHeader}`（与其他越界变更一致）。
//! `Change::MakeDir{name}` 在拷贝循环后追加空目录条目（`is_dir`，mtime = 执行时刻）。
//!
//! # 加密保留
//!
//! `open_editor` 记住 `OpenOptions::password`：读旧条目以该密码解数据区；重写时
//! 若源档案有加密条目（或 `WriteCtx.opts.encryption` 请求加密），**每个条目**的
//! `next_entry` 均携带该密码（7z 加密为档案级，密码缺失 → `Password{Archive}`）。
//! 未加密源 + 未请求加密 → 全程 `PasswordRef::none()`（零密码弹窗）。

use std::io::{Read, Write};
use std::path::PathBuf;

use hzip_error::{FormatErrorKind, HzipError, PasswordTarget};
use hzip_format::{
    ArchiveEditor, ArchiveReader, Change, ChangeSet, EntryMeta, EntryName, FormatHandler,
    OpenOptions, PasswordRef, Sink, Source, WriteCtx, WriteReport,
};
use zeroize::Zeroizing;

use crate::reader::SevenZipReader;

/// 7z 编辑器（持只读打开的旧 reader）。
pub(crate) struct SevenZipEditor {
    old: Box<dyn ArchiveReader>,
    /// 打开期携带的档密码（`OpenOptions::password`）：读旧条目与加密重写复用。
    password: Option<Zeroizing<String>>,
}

impl SevenZipEditor {
    pub(crate) fn open(
        src: Source,
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        let old = SevenZipReader::open(src, opts)?;
        Ok(Box::new(SevenZipEditor {
            old,
            password: opts.password.clone(),
        }))
    }

    /// 读/写用的密码引用（无密码 → `none`）。
    fn pw(&self) -> PasswordRef<'_> {
        match self.password.as_ref() {
            Some(p) => PasswordRef::new(p),
            None => PasswordRef::none(),
        }
    }
}

impl ArchiveEditor for SevenZipEditor {
    fn rewrite(
        &self,
        changes: ChangeSet,
        dst: Sink,
        ctx: &WriteCtx<'_>,
    ) -> Result<WriteReport, HzipError> {
        let old = self.old.as_ref();
        let infos = old.entries()?;
        let format = hzip_format::FormatId::from(crate::FORMAT_ID);
        let bad = |_why: &str| HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadHeader,
        };

        // 变更分派（id 越界 → 明确 Format BadHeader；名字类变更在下方解析）
        let mut to_delete = vec![false; infos.len()];
        let mut to_replace: Vec<Option<PathBuf>> = vec![None; infos.len()];
        let mut to_append: Vec<(PathBuf, EntryName)> = Vec::new();
        let mut to_rename: Vec<(EntryName, EntryName)> = Vec::new();
        let mut to_mkdir: Vec<EntryName> = Vec::new();
        for c in &changes.changes {
            match c {
                Change::Delete(id) => {
                    let Some(flag) = to_delete.get_mut(*id) else {
                        return Err(bad("变更引用条目越界"));
                    };
                    *flag = true;
                }
                Change::Replace { entry, src } => {
                    let Some(slot) = to_replace.get_mut(*entry) else {
                        return Err(bad("变更引用条目越界"));
                    };
                    *slot = Some(src.clone());
                }
                Change::Append { src, name } => to_append.push((src.clone(), name.clone())),
                Change::Rename { from, to } => to_rename.push((from.clone(), to.clone())),
                Change::MakeDir { name } => to_mkdir.push(name.clone()),
                _ => {
                    return Err(HzipError::Unsupported {
                        what: "未知 Change 变体（契约扩展；本格式暂不支持）".into(),
                    });
                }
            }
        }

        // 改名映射：逐条按序应用（后一变更看到前一变更的结果）。`from` 命中同名
        // 条目（目录条目本身）或前缀 `from + '/'`（隐式目录子树）即算命中；全无
        // 命中 → BadHeader（同越界语义）。
        let mut names: Vec<EntryName> = infos.iter().map(|i| i.name.clone()).collect();
        for (from, to) in &to_rename {
            let from_s = from.as_str();
            let prefix = format!("{from_s}/");
            let mut hit = false;
            let mut next: Vec<EntryName> = Vec::with_capacity(names.len());
            for cur in &names {
                let cur_s = cur.as_str();
                if cur_s == from_s {
                    next.push(to.clone());
                    hit = true;
                } else if let Some(rest) = cur_s.strip_prefix(&prefix) {
                    next.push(EntryName::new(format!("{}/{rest}", to.as_str())));
                    hit = true;
                } else {
                    next.push(cur.clone());
                }
            }
            if !hit {
                return Err(bad("重命名引用的条目名不存在"));
            }
            names = next;
        }

        // 输出是否加密：源有加密条目（保真）或调用方请求加密。7z 加密为档案级，
        // 任一成立即需要密码——缺失则明确 Password{Archive}（而非静默产明文档）。
        let encrypt_out = ctx.opts.encryption.is_some() || infos.iter().any(|i| i.encrypted);
        if encrypt_out && self.password.is_none() {
            return Err(HzipError::Password {
                target: PasswordTarget::Archive,
            });
        }
        let write_pw = if encrypt_out {
            self.pw()
        } else {
            PasswordRef::none()
        };

        let mut writer =
            crate::handler::SevenZipFormatHandler::new().create_writer(dst, ctx.opts)?;
        let mut bytes_in = 0u64;
        let mut written = 0usize;

        for (id, info) in infos.iter().enumerate() {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            if to_delete[id] {
                continue;
            }
            let data = if let Some(src) = to_replace[id].as_deref() {
                bytes_in += std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
                std::fs::read(src).map_err(|e| HzipError::Io {
                    source: e,
                    path: Some(src.to_path_buf()),
                })?
            } else if info.is_dir {
                Vec::new()
            } else {
                // 非目录：读旧条目内容（symlink 目标即内容；加密档案用打开期密码）
                let mut r = old.read_entry(id, self.pw())?;
                let mut buf = Vec::new();
                r.read_to_end(&mut buf).map_err(HzipError::from)?;
                bytes_in += buf.len() as u64;
                buf
            };
            // symlink 条目内容即 target（写侧把 target 作为数据重放）；对替换情形
            // 用户提供的新内容替换整体条目。名字取改名映射结果。
            let meta = EntryMeta {
                name: names[id].clone(),
                is_dir: info.is_dir,
                is_symlink: info.is_symlink && to_replace[id].is_none(),
                symlink_target: if info.is_symlink && to_replace[id].is_none() {
                    Some(PathBuf::from(String::from_utf8_lossy(&data).into_owned()))
                } else {
                    None
                },
                size_raw: if info.is_dir {
                    Some(0)
                } else {
                    Some(data.len() as u64)
                },
                mtime: info.mtime,
                mode: info.mode,
                attrs: info.attributes,
                encrypted: encrypt_out,
            };
            let mut sink = writer.next_entry(&meta, write_pw)?;
            // symlink 的 target 内容经 EntryMeta.symlink_target 重放（writer 读之，
            // 不写数据流）；普通/替换文件写数据。
            if !meta.is_dir && !meta.is_symlink {
                sink.write_all(&data).map_err(HzipError::from)?;
            }
            sink.finish()?;
            written += 1;
        }

        // 新建空目录条目（写侧对目录只写元数据、无数据流；时间戳取执行时刻）
        for name in to_mkdir {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            let meta = EntryMeta {
                name,
                is_dir: true,
                is_symlink: false,
                symlink_target: None,
                size_raw: Some(0),
                mtime: Some(time::OffsetDateTime::now_utc()),
                mode: None,
                attrs: Default::default(),
                encrypted: encrypt_out,
            };
            let sink = writer.next_entry(&meta, write_pw)?;
            sink.finish()?;
            written += 1;
        }

        // 追加新条目
        for (src, name) in to_append {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            let data = std::fs::read(&src).map_err(|e| HzipError::Io {
                source: e,
                path: Some(src.clone()),
            })?;
            bytes_in += data.len() as u64;
            let meta = EntryMeta {
                name,
                is_dir: false,
                is_symlink: false,
                symlink_target: None,
                size_raw: Some(data.len() as u64),
                mtime: None,
                mode: None,
                attrs: Default::default(),
                encrypted: encrypt_out,
            };
            let mut sink = writer.next_entry(&meta, write_pw)?;
            sink.write_all(&data).map_err(HzipError::from)?;
            sink.finish()?;
            written += 1;
        }

        let mut report = writer.finish()?;
        report.entries_written = written;
        report.bytes_in = bytes_in;
        Ok(report)
    }
}
