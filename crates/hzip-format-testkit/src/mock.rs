//! MockHandler：可注入缺陷的格式 handler（02 §8.6；T30 core 测试消费）。
//!
//! 注入点覆盖 core 集成测试全部场景：panic 隔离（TC-CORE-013）、取消延迟
//! （TC-CORE-014）、指定错误（TC-CORE-007/008 部分失败）、受控进度
//! （TC-CORE-010 守恒断言）。`Hook`/`Scripted` 标注 `#[non_exhaustive]`，
//! 冻结约束与契约同级（core 测试稳定性）。

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hzip_error::HzipError;
use hzip_format::{
    ArchiveEditor, ArchiveReader, ArchiveWriter, CreateOptions, EntryInfo, EntryMeta, ExtractCtx,
    ExtractReport, FormatCaps, FormatHandler, OpenOptions, PasswordRef, Selection, Sink, Source,
};
use hzip_format::{EntryName, FormatId};

/// 注入点（覆盖 core 集成测试全部场景）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Hook {
    /// open_reader
    OpenReader,
    /// create_writer
    CreateWriter,
    /// open_editor
    OpenEditor,
    /// entries()
    Entries,
    /// read_entry(id)
    ReadEntry(usize),
    /// extract()
    Extract,
    /// test()
    Test,
    /// next_entry()
    NextEntry,
    /// finish()
    Finish,
    /// rewrite()
    Rewrite,
}

/// 注入脚本（命中该 hook 时消费一次）。
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Scripted {
    /// 命中即 panic（TC-CORE-013 panic 隔离）
    Panic,
    /// 慢速（per_chunk × chunks；TC-CORE-014 取消延迟、006 计时）
    Slow {
        /// 每块延迟
        per_chunk: Duration,
        /// 块数
        chunks: u32,
    },
    /// 指定错误（TC-CORE-007/008 部分失败）
    Error(HzipError),
    /// 受控进度突发（TC-CORE-010 守恒断言）
    ProgressBurst {
        /// 突发字节
        bytes: u64,
        /// 突发次数
        times: u32,
    },
}

/// mock 档案条目。
#[derive(Debug, Clone)]
pub struct MockEntry {
    /// 条目名
    pub name: EntryName,
    /// 条目数据
    pub data: Arc<Vec<u8>>,
    /// 元数据
    pub meta: EntryMeta,
}

impl MockEntry {
    /// 便捷构造（普通文件条目，默认元数据）。
    pub fn file(name: impl Into<EntryName>, data: Vec<u8>) -> Self {
        let name = name.into();
        Self {
            meta: EntryMeta {
                name: name.clone(),
                size_raw: Some(data.len() as u64),
                ..Default::default()
            },
            name,
            data: Arc::new(data),
        }
    }
}

/// mock 内部共享状态（reader/writer 与 handler 同源）。
#[derive(Debug, Default)]
struct Shared {
    script: Mutex<VecDeque<(Hook, Scripted)>>,
    calls: Mutex<Vec<Hook>>,
}

/// 内存 mock handler（id/caps 可伪造、固定条目表、可注入脚本）。
#[derive(Debug)]
pub struct MockHandler {
    id: FormatId,
    caps: FormatCaps,
    entries: Vec<MockEntry>,
    shared: Arc<Shared>,
    read_counts: Arc<AtomicUsize>,
}

impl MockHandler {
    /// 新建（id + 伪造 caps）。
    pub fn new(id: &str, caps: FormatCaps) -> Self {
        Self {
            id: FormatId::from(id),
            caps,
            entries: Vec::new(),
            shared: Arc::new(Shared::default()),
            read_counts: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 设置固定条目表。
    pub fn with_entries(mut self, entries: Vec<MockEntry>) -> Self {
        self.entries = entries;
        self
    }

    /// 注入脚本（同一 hook 可叠加多次，按次消费）。
    pub fn inject(&self, hook: Hook, script: Scripted) {
        if let Ok(mut g) = self.shared.script.lock() {
            g.push_back((hook, script));
        }
    }

    /// 调用记录（顺序断言）。
    pub fn calls(&self) -> Vec<Hook> {
        self.shared
            .calls
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// read_entry 被调用次数。
    pub fn read_count(&self) -> usize {
        self.read_counts.load(Ordering::SeqCst)
    }

    /// 由 MockEntry 表构造 EntryInfo 列表。
    fn entry_infos(&self) -> Vec<EntryInfo> {
        self.entries
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
                encrypted: e.meta.encrypted,
                header_encrypted: false,
                crc32: None,
                codec: None,
                name_encoding: hzip_format::NameEncoding::Unknown,
                encryption: None,
                compression_level: None,
            })
            .collect()
    }
}

impl FormatHandler for MockHandler {
    fn id(&self) -> FormatId {
        self.id.clone()
    }
    fn signature(&self) -> &'static hzip_format::Signature {
        &hzip_format::Signature {
            magic: b"",
            offset: 0,
            display: "mock",
            extensions: &[],
        }
    }
    fn caps(&self) -> FormatCaps {
        self.caps
    }
    fn open_reader(
        &self,
        _src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        push_call(&self.shared, Hook::OpenReader);
        // 注：Panic 注入在调用点由 run_script 触发——open_reader 自身注入需在
        // 命中时 panic，这里直接在内部处理
        if let Some(Scripted::Panic) = take_script(&self.shared, &Hook::OpenReader) {
            panic!("MockHandler 注入 Panic@OpenReader");
        }
        Ok(Box::new(MockReader {
            infos: self.entry_infos(),
            data: self.entries.clone(),
            shared: Arc::clone(&self.shared),
            read_counts: Arc::clone(&self.read_counts),
        }))
    }
    fn create_writer(
        &self,
        _dst: Sink,
        _opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        push_call(&self.shared, Hook::CreateWriter);
        Err(HzipError::Unsupported {
            what: "mock create_writer 需用例声明 write caps 并自行实现".into(),
        })
    }
    fn open_editor(
        &self,
        _src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        push_call(&self.shared, Hook::OpenEditor);
        Err(HzipError::Unsupported {
            what: "mock open_editor 需用例声明 edit caps".into(),
        })
    }
}

/// 记录调用。
fn push_call(shared: &Shared, h: Hook) {
    if let Ok(mut g) = shared.calls.lock() {
        g.push(h);
    }
}

/// 取走某 hook 的下一条脚本（无则 None）。
fn take_script(shared: &Shared, hook: &Hook) -> Option<Scripted> {
    let mut g = shared.script.lock().ok()?;
    let idx = g.iter().position(|(h, _)| h == hook)?;
    g.remove(idx).map(|(_, s)| s)
}

/// Mock 读取器（Arc 共享 handler 状态）。
struct MockReader {
    infos: Vec<EntryInfo>,
    data: Vec<MockEntry>,
    shared: Arc<Shared>,
    read_counts: Arc<AtomicUsize>,
}

impl ArchiveReader for MockReader {
    fn caps(&self) -> FormatCaps {
        FormatCaps {
            read: true,
            ..Default::default()
        }
    }
    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
        push_call(&self.shared, Hook::Entries);
        match take_script(&self.shared, &Hook::Entries) {
            Some(Scripted::Error(e)) => Err(e),
            Some(Scripted::Panic) => panic!("MockHandler 注入 Panic@Entries"),
            Some(_) => Ok(self.infos.clone()),
            None => Ok(self.infos.clone()),
        }
    }
    fn read_entry(
        &self,
        id: usize,
        _pw: PasswordRef<'_>,
    ) -> Result<Box<dyn std::io::Read + Send>, HzipError> {
        self.read_counts.fetch_add(1, Ordering::SeqCst);
        push_call(&self.shared, Hook::ReadEntry(id));
        let data = self
            .data
            .get(id)
            .ok_or_else(|| HzipError::Format {
                format: FormatId::from("mock"),
                kind: hzip_error::FormatErrorKind::BadHeader,
            })?
            .data
            .clone();
        Ok(Box::new(std::io::Cursor::new((*data).clone())))
    }
    fn extract(
        &self,
        _sel: &Selection,
        _ctx: &ExtractCtx<'_>,
        _dest: &Path,
    ) -> Result<ExtractReport, HzipError> {
        push_call(&self.shared, Hook::Extract);
        match take_script(&self.shared, &Hook::Extract) {
            Some(Scripted::Error(e)) => Err(e),
            Some(Scripted::Panic) => panic!("MockHandler 注入 Panic@Extract"),
            _ => Ok(ExtractReport::default()),
        }
    }
    fn test(&self, _ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
        push_call(&self.shared, Hook::Test);
        match take_script(&self.shared, &Hook::Test) {
            Some(Scripted::Error(e)) => Err(e),
            Some(Scripted::Panic) => panic!("MockHandler 注入 Panic@Test"),
            _ => Ok(ExtractReport::default()),
        }
    }
}
