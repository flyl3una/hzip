//! StubHandler：tar 临时桩（05 T21 交付物④；套件自验证用）。
//!
//! 同波次无 hzip-fmt-tar 可依赖，以内嵌桩验证**套件自身**可运行（T22 合入后由
//! 真 tar handler 替换为正式接入对象）。多条目、write/append 可、edit=false、
//! encryption=None——足以驱动 001~006/008/010 组；007/009 组经 CapsFilter 自动 Skip。
//!
//! 写路径为简化语义：`create_writer` 在 `finish()` 时把桩当前条目集序列化为
//! `HZIPSTUB` 自描述文件（无流式增量；套件自验证不需要真实格式写语义）。

use std::path::Path;
use std::sync::Arc;

use hzip_error::HzipError;
use hzip_format::{
    ArchiveEditor, ArchiveReader, ArchiveWriter, CreateOptions, EntryInfo, EntryMeta, ExtractCtx,
    ExtractReport, FormatCaps, FormatHandler, OpenOptions, PasswordRef, Selection, Sink, Source,
    WriteReport,
};
use hzip_format::{EntryName, FormatId};

use crate::mock::MockEntry;

/// 桩格式魔数（自描述布局头）。
const STUB_MAGIC: &[u8; 8] = b"HZIPSTUB";

/// tar 桩 handler（套件自验证；feature `stub` 门控）。
#[derive(Debug, Clone, Default)]
pub struct StubHandler {
    /// 档案条目（写路径输入集）
    entries: Arc<Vec<MockEntry>>,
}

impl StubHandler {
    /// 新建（默认空条目）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 带条目的桩。
    pub fn with_entries(entries: Vec<MockEntry>) -> Self {
        Self {
            entries: Arc::new(entries),
        }
    }

    fn caps_impl() -> FormatCaps {
        FormatCaps {
            read: true,
            write: true,
            append: true,
            edit: false,
            ..Default::default()
        }
    }
}

impl FormatHandler for StubHandler {
    fn id(&self) -> FormatId {
        FormatId::from("stub")
    }
    fn signature(&self) -> &'static hzip_format::Signature {
        &hzip_format::Signature {
            magic: STUB_MAGIC,
            offset: 0,
            display: "stub",
            extensions: &["stub"],
        }
    }
    fn caps(&self) -> FormatCaps {
        Self::caps_impl()
    }
    fn open_reader(
        &self,
        src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        let entries = match src {
            Source::File(p) => {
                StubArchive::parse(&std::fs::read(&p).map_err(HzipError::from)?)?.entries
            }
            _ => self.entries.clone(),
        };
        Ok(Box::new(StubReader { entries }))
    }
    fn create_writer(
        &self,
        dst: Sink,
        _opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        Ok(Box::new(StubWriter {
            dst,
            entries: self.entries.clone(),
        }))
    }
    fn open_editor(
        &self,
        _src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        Err(HzipError::Unsupported {
            what: "stub 无编辑能力".into(),
        })
    }
}

/// 桩档案（内存自描述布局）。
#[derive(Debug)]
pub struct StubArchive {
    entries: Arc<Vec<MockEntry>>,
}

impl StubArchive {
    /// 从字节解析桩档案。
    pub fn parse(bytes: &[u8]) -> Result<StubArchive, HzipError> {
        if !bytes.starts_with(STUB_MAGIC) {
            return Err(HzipError::Format {
                format: FormatId::from("stub"),
                kind: hzip_error::FormatErrorKind::BadSignature,
            });
        }
        // 布局：magic(8) + count(u32 LE) + 每项 [name_len u32][name][data_len u64][data]
        let mut pos = STUB_MAGIC.len();
        let truncated = || HzipError::Format {
            format: FormatId::from("stub"),
            kind: hzip_error::FormatErrorKind::Truncated,
        };
        let read_u32 = |p: &mut usize| -> Result<u32, HzipError> {
            if *p + 4 > bytes.len() {
                return Err(truncated());
            }
            // 长度已校验（>= 4），用索引拼装避免 try_into 的 expect
            let v = u32::from_le_bytes([bytes[*p], bytes[*p + 1], bytes[*p + 2], bytes[*p + 3]]);
            *p += 4;
            Ok(v)
        };
        let read_u64 = |p: &mut usize| -> Result<u64, HzipError> {
            if *p + 8 > bytes.len() {
                return Err(truncated());
            }
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&bytes[*p..*p + 8]);
            let v = u64::from_le_bytes(arr);
            *p += 8;
            Ok(v)
        };
        let count = read_u32(&mut pos)? as usize;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let name_len = read_u32(&mut pos)? as usize;
            if pos + name_len > bytes.len() {
                return Err(truncated());
            }
            let name_raw = &bytes[pos..pos + name_len];
            pos += name_len;
            let name = String::from_utf8_lossy(name_raw).into_owned();
            let data_len = read_u64(&mut pos)? as usize;
            if pos + data_len > bytes.len() {
                return Err(truncated());
            }
            let data = bytes[pos..pos + data_len].to_vec();
            pos += data_len;
            entries.push(MockEntry::file(EntryName::new(name), data));
        }
        Ok(StubArchive {
            entries: Arc::new(entries),
        })
    }

    /// 序列化。
    pub fn to_bytes(entries: &[MockEntry]) -> Vec<u8> {
        let mut out = STUB_MAGIC.to_vec();
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for e in entries {
            let name = e.name.as_str().as_bytes();
            out.extend_from_slice(&(name.len() as u32).to_le_bytes());
            out.extend_from_slice(name);
            out.extend_from_slice(&(e.data.len() as u64).to_le_bytes());
            out.extend_from_slice(&e.data);
        }
        out
    }
}

struct StubReader {
    entries: Arc<Vec<MockEntry>>,
}

impl ArchiveReader for StubReader {
    fn caps(&self) -> FormatCaps {
        StubHandler::caps_impl()
    }
    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
        Ok(self
            .entries
            .iter()
            .enumerate()
            .map(|(id, e)| EntryInfo {
                id,
                name: e.name.clone(),
                is_dir: e.meta.is_dir,
                is_symlink: e.meta.is_symlink,
                symlink_target: e.meta.symlink_target.clone(),
                size_raw: e.data.len() as u64,
                size_packed: e.data.len() as u64,
                mtime: e.meta.mtime,
                mode: e.meta.mode,
                attributes: e.meta.attrs,
                encrypted: false,
                header_encrypted: false,
                crc32: None,
                codec: None,
                name_encoding: hzip_format::NameEncoding::Unknown,
                encryption: None,
                compression_level: None,
            })
            .collect())
    }
    fn read_entry(
        &self,
        id: usize,
        _pw: PasswordRef<'_>,
    ) -> Result<Box<dyn std::io::Read + Send>, HzipError> {
        let d = self
            .entries
            .get(id)
            .ok_or_else(|| HzipError::Format {
                format: FormatId::from("stub"),
                kind: hzip_error::FormatErrorKind::BadHeader,
            })?
            .data
            .clone();
        Ok(Box::new(std::io::Cursor::new((*d).clone())))
    }
    fn extract(
        &self,
        sel: &Selection,
        _ctx: &ExtractCtx<'_>,
        dest: &Path,
    ) -> Result<ExtractReport, HzipError> {
        let ids: Vec<usize> = match sel {
            Selection::All => (0..self.entries.len()).collect(),
            Selection::Ids(v) => v.clone(),
            Selection::Names(n) => self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| n.contains(&e.name))
                .map(|(i, _)| i)
                .collect(),
            _ => {
                return Err(HzipError::Unsupported {
                    what: "Selection 新变体未支持（stub）".into(),
                });
            }
        };
        let mut report = ExtractReport::default();
        for id in ids {
            let e = self.entries.get(id).ok_or_else(|| HzipError::Format {
                format: FormatId::from("stub"),
                kind: hzip_error::FormatErrorKind::BadHeader,
            })?;
            let p = dest.join(e.name.as_str());
            if e.meta.is_dir {
                std::fs::create_dir_all(&p).map_err(HzipError::from)?;
            } else {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(HzipError::from)?;
                }
                std::fs::write(&p, &*e.data).map_err(HzipError::from)?;
            }
            report.entries_done += 1;
            report.bytes_written += e.data.len() as u64;
        }
        Ok(report)
    }
    fn test(&self, _ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
        Ok(ExtractReport {
            entries_done: self.entries.len(),
            bytes_written: self.entries.iter().map(|e| e.data.len() as u64).sum(),
            failures: vec![],
        })
    }
}

/// 桩写路径：finish 时把条目集序列化到目标（简化；无流式增量）。
struct StubWriter {
    dst: Sink,
    entries: Arc<Vec<MockEntry>>,
}

impl ArchiveWriter for StubWriter {
    fn next_entry(
        &mut self,
        _meta: &EntryMeta,
        _pw: PasswordRef<'_>,
    ) -> Result<hzip_format::EntrySink<'_>, HzipError> {
        Err(HzipError::Unsupported {
            what: "stub writer 不实现流式 next_entry——002 组经 create_writer+finish 全量写".into(),
        })
    }
    fn finish(self: Box<Self>) -> Result<WriteReport, HzipError> {
        let bytes = StubArchive::to_bytes(&self.entries);
        match self.dst {
            Sink::File(p) => {
                std::fs::write(&p, &bytes).map_err(HzipError::from)?;
                Ok(WriteReport {
                    entries_written: self.entries.len(),
                    bytes_in: self.entries.iter().map(|e| e.data.len() as u64).sum(),
                    bytes_out: bytes.len() as u64,
                    volume_paths: vec![],
                })
            }
            other => Err(HzipError::Unsupported {
                what: match other {
                    Sink::Stdout => "stub writer 目标仅支持 File（收到 Stdout）".into(),
                    Sink::Split { .. } => "stub writer 目标仅支持 File（收到 Split）".into(),
                    Sink::Stream(_) => "stub writer 目标仅支持 File（收到 Stream）".into(),
                    Sink::File(_) => unreachable!("File 已在上分支处理"),
                },
            }),
        }
    }
}

#[cfg(test)]
#[path = "tests/stub.rs"]
mod tests;
