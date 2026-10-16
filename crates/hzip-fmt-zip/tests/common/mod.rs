//! 测试共享助手（写档案 → 读回；断言等价）。
//!
//! 每个 tests/*.rs 独立 crate 都 `mod common;`——各测试只用到本模块子集，
//! dead_code 报警以整模块 allow 豁免（非漏用）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]
#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use hzip_format::{EntryMeta, FormatHandler, PasswordRef, Sink, Source};
use sha2::{Digest, Sha256};

/// 无密码密码源（PasswordSource 测试替身）。
#[derive(Default)]
pub struct NoPwSource;
impl hzip_format::PasswordSource for NoPwSource {
    fn password(
        &self,
        _entry: Option<&hzip_format::EntryName>,
    ) -> Option<zeroize::Zeroizing<String>> {
        None
    }
}

/// 最小 ExtractCtx 持有组件。
pub struct TestEnv<'a> {
    cancel: hzip_progress::CancellationToken,
    bus: hzip_format::ProgressBus,
    pws: &'a dyn hzip_format::PasswordSource,
    sanitize: hzip_format::SanitizeConfig,
    quota: hzip_format::QuotaLimits,
}
impl<'a> TestEnv<'a> {
    pub fn new(pws: &'a dyn hzip_format::PasswordSource) -> Self {
        Self {
            cancel: Default::default(),
            bus: Default::default(),
            pws,
            sanitize: Default::default(),
            quota: Default::default(),
        }
    }
    pub fn ctx(&'a self) -> hzip_format::ExtractCtx<'a> {
        hzip_format::ExtractCtx {
            cancel: &self.cancel,
            progress: &self.bus,
            passwords: self.pws,
            conflicts: hzip_format::ConflictPolicy::Overwrite,
            sanitize: &self.sanitize,
            quota: &self.quota,
            best_effort: false,
            codepage: None,
        }
    }
    pub fn cancel(&self) -> &hzip_progress::CancellationToken {
        &self.cancel
    }
}

/// 便捷 handler。
pub fn handler() -> hzip_fmt_zip::ZipFormatHandler {
    hzip_fmt_zip::ZipFormatHandler::new()
}

/// sha256 hex。
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// 临时目录（进程级唯一）。
pub fn tmp_dir(tag: &str) -> PathBuf {
    // ⚠️ 不能只靠 `SystemTime::now()` 的纳秒：Windows 上它是**粗粒度**时钟
    // （约 15.6 ms 一跳），同一测试二进制里并行跑、**同 tag** 的用例会拿到
    // **同一个目录** ⇒ 一个用例收尾的 `remove_dir_all` 会把另一个用例正在读的
    // 档案删掉（实测 `tests/editor.rs::edit_replace_delete_append` 因此偶发
    // `Io { NotFound }`，且失败点落在「刚写完的文件上」）。故再加进程内自增
    // 序号：同一进程内**永不重名**，与时钟分辨率无关。
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "hzip-zip-{tag}-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 经 handler writer 创建 zip 档案（entries: (name, data)）。
pub fn write_archive(entries: &[(String, Vec<u8>)]) -> PathBuf {
    write_archive_at(&tmp_dir("w"), "out.zip", entries)
}

/// 在指定目录写档案。
pub fn write_archive_at(dir: &Path, name: &str, entries: &[(String, Vec<u8>)]) -> PathBuf {
    let p = dir.join(name);
    let mut w = handler()
        .create_writer(Sink::File(p.clone()), &Default::default())
        .unwrap();
    for (name, data) in entries {
        let meta = EntryMeta {
            name: name.as_str().into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
        sink.write_all(data).unwrap();
        sink.finish().unwrap();
    }
    w.finish().unwrap();
    p
}

/// 打开 reader 并列出条目。
pub fn entries(p: &Path) -> Vec<hzip_format::EntryInfo> {
    let r = handler()
        .open_reader(Source::File(p.to_path_buf()), &Default::default())
        .unwrap();
    r.entries().unwrap()
}

/// 读整条（entry id）。
pub fn read_all(r: &dyn hzip_format::ArchiveReader, id: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    r.read_entry(id, PasswordRef::none())
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    buf
}

/// 读档案文件单条目。
pub fn read_entry_file(p: &Path, id: usize) -> Vec<u8> {
    let r = handler()
        .open_reader(Source::File(p.to_path_buf()), &Default::default())
        .unwrap();
    read_all(r.as_ref(), id)
}

/// 打开文件源 reader。
pub fn reader_of(p: &Path) -> Box<dyn hzip_format::ArchiveReader> {
    handler()
        .open_reader(Source::File(p.to_path_buf()), &Default::default())
        .unwrap()
}
