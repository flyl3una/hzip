use super::*;

struct MemBackend {
    buf: Vec<u8>,
    finish_calls: std::cell::Cell<usize>,
    declared: Option<u64>,
}

impl MemBackend {
    fn new(declared: Option<u64>) -> Self {
        Self {
            buf: Vec::new(),
            finish_calls: std::cell::Cell::new(0),
            declared,
        }
    }
}

impl Write for MemBackend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl EntrySinkBackend for MemBackend {
    fn finish_entry(&mut self) -> Result<(), HzipError> {
        self.finish_calls.set(self.finish_calls.get() + 1);
        if let Some(d) = self.declared
            && self.buf.len() as u64 != d
        {
            return Err(HzipError::Format {
                format: crate::FormatId::from("test"),
                kind: hzip_error::FormatErrorKind::BadHeader,
            });
        }
        Ok(())
    }
}

fn meta(size: Option<u64>) -> EntryMeta {
    EntryMeta {
        name: "a.bin".into(),
        size_raw: size,
        ..Default::default()
    }
}

#[test]
fn entry_sink_crc_and_bytes() {
    let cancel = CancellationToken::new();
    let bus = ProgressBus::new();
    let m = meta(Some(4));
    let mut sink = EntrySink::from_backend(Box::new(MemBackend::new(Some(4))), &m, &cancel, &bus);
    sink.write_all(b"test").unwrap();
    assert_eq!(sink.bytes_written(), 4);
    // crc32("test") = 0xD87F7E0C
    assert_eq!(sink.crc32(), 0xD87F_7E0C);
    sink.finish().unwrap();
}

#[test]
fn entry_sink_precancel() {
    let cancel = CancellationToken::new();
    let bus = ProgressBus::new();
    cancel.cancel();
    let m = meta(None);
    let mut sink = EntrySink::from_backend(Box::new(MemBackend::new(None)), &m, &cancel, &bus);
    assert!(sink.write_all(b"x").is_err(), "预取消下首次 write 应失败");
}

#[test]
fn entry_sink_size_mismatch() {
    let cancel = CancellationToken::new();
    let bus = ProgressBus::new();
    let m = meta(Some(10));
    let mut sink = EntrySink::from_backend(Box::new(MemBackend::new(Some(10))), &m, &cancel, &bus);
    sink.write_all(b"short").unwrap();
    // 预声明 10 实际 5 → finish 报 Format BadHeader
    assert!(matches!(
        sink.finish(),
        Err(HzipError::Format {
            kind: hzip_error::FormatErrorKind::BadHeader,
            ..
        })
    ));
}

#[test]
fn entry_sink_drop_best_effort() {
    let cancel = CancellationToken::new();
    let bus = ProgressBus::new();
    {
        let m = meta(None);
        let mut sink = EntrySink::from_backend(Box::new(MemBackend::new(None)), &m, &cancel, &bus);
        sink.write_all(b"x").unwrap();
        // 不调 finish，直接 drop → 保底收尾不 panic
    }
}
