//! 注册表单元测试（T20 §6.1：注册/查找/重复 id/mock 组合装配）。
use self::mock::MockHandler;
use super::*;

#[test]
fn register_lookup_ids() {
    let mut reg = FormatRegistry::new();
    reg.register_handler(Box::new(MockHandler::new("zip")))
        .expect("注册 zip");
    reg.register_handler(Box::new(MockHandler::new("tar")))
        .expect("注册 tar");
    assert_eq!(reg.ids().len(), 2);
    assert!(reg.handler("zip").is_some());
    assert!(reg.handler("tar").is_some());
    assert!(reg.handler("rar").is_none());
}

#[test]
fn duplicate_id_rejected() {
    let mut reg = FormatRegistry::new();
    reg.register_handler(Box::new(MockHandler::new("zip")))
        .expect("首次注册 ok");
    let err = reg
        .register_handler(Box::new(MockHandler::new("zip")))
        .expect_err("重复 id 应拒绝");
    assert_eq!(err.code(), 5001);
}

#[test]
fn mock_open_reader_entries_passthrough() {
    // 经 handler() 直开 mock（绕过嗅探，mock 的 open_reader 返回固定条目表）
    let mock =
        MockHandler::new("mockfmt").with_entries(vec![("a.txt".to_string(), b"hi".to_vec())]);
    let mut reg = FormatRegistry::new();
    reg.register_handler(Box::new(mock)).expect("注册");

    let h = reg.handler("mockfmt").expect("查到 mock");
    let reader = h
        .open_reader(
            Source::File(std::path::PathBuf::from("unused.bin")),
            &OpenOptions::default(),
        )
        .expect("open_reader");
    let entries = reader.entries().expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name.as_str(), "a.txt");
    assert_eq!(entries[0].size_raw, 2);
}

pub(crate) mod mock {
    //! 临时私有 mock（T20 §4.8；正式 MockHandler 归 T21 公开交付）。
    use super::*;
    use crate::Signature;
    use std::io::Cursor;

    /// 最小 handler：id 可配置、固定条目表。
    pub(crate) struct MockHandler {
        pub(crate) id: &'static str,
        pub(crate) entries: Vec<(String, Vec<u8>)>,
        pub(crate) stream_opens: std::sync::atomic::AtomicUsize,
    }

    impl MockHandler {
        pub(crate) fn new(id: &'static str) -> Self {
            Self {
                id,
                entries: vec![],
                stream_opens: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        /// 设置固定条目表（mock 的 entries() 返回内容）。
        pub(crate) fn with_entries(mut self, entries: Vec<(String, Vec<u8>)>) -> Self {
            self.entries = entries;
            self
        }
    }

    /// mock 共用签名占位（嗅探路径由 registry 以文件魔数判定，mock 仅形式提供）。
    static MOCK_SIG: Signature = Signature {
        magic: b"",
        offset: 0,
        display: "mock",
        extensions: &[],
    };

    impl FormatHandler for MockHandler {
        fn id(&self) -> FormatId {
            FormatId::from(self.id)
        }
        fn signature(&self) -> &'static Signature {
            &MOCK_SIG
        }
        fn caps(&self) -> FormatCaps {
            FormatCaps {
                read: true,
                write: true,
                ..Default::default()
            }
        }
        fn open_reader(
            &self,
            src: Source,
            _opts: &OpenOptions,
        ) -> Result<Box<dyn ArchiveReader>, HzipError> {
            if let Source::Stream(_) = src {
                self.stream_opens
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            let entries = self
                .entries
                .iter()
                .enumerate()
                .map(|(i, (n, d))| EntryInfo {
                    id: i,
                    name: entry_name_of(n.clone()),
                    is_dir: false,
                    is_symlink: false,
                    symlink_target: None,
                    size_raw: d.len() as u64,
                    size_packed: d.len() as u64,
                    mtime: None,
                    mode: Some(0o644),
                    attributes: Default::default(),
                    encrypted: false,
                    header_encrypted: false,
                    crc32: None,
                    codec: None,
                    name_encoding: crate::types::NameEncoding::Unknown,
                    encryption: None,
                    compression_level: None,
                })
                .collect();
            Ok(Box::new(MockReader {
                entries,
                data: self.entries.clone(),
            }))
        }
        fn create_writer(
            &self,
            _dst: Sink,
            _opts: &CreateOptions,
        ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
            Err(HzipError::Unsupported {
                what: "mock 不实现写".into(),
            })
        }
        fn open_editor(
            &self,
            _src: Source,
            _opts: &OpenOptions,
        ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
            Err(HzipError::Unsupported {
                what: "mock 不实现编辑".into(),
            })
        }
    }

    fn entry_name_of(s: String) -> crate::types::EntryName {
        crate::types::EntryName::new(s)
    }

    pub(crate) struct MockReader {
        entries: Vec<EntryInfo>,
        data: Vec<(String, Vec<u8>)>,
    }

    impl ArchiveReader for MockReader {
        fn caps(&self) -> FormatCaps {
            FormatCaps {
                read: true,
                ..Default::default()
            }
        }
        fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
            Ok(self.entries.clone())
        }
        fn read_entry(
            &self,
            id: EntryId,
            _pw: PasswordRef<'_>,
        ) -> Result<Box<dyn Read + Send>, HzipError> {
            let data =
                self.data
                    .get(id)
                    .map(|(_, d)| d.clone())
                    .ok_or_else(|| HzipError::Format {
                        format: FormatId::from("mock"),
                        kind: hzip_error::FormatErrorKind::BadHeader,
                    })?;
            Ok(Box::new(Cursor::new(data)))
        }
        fn extract(
            &self,
            _sel: &Selection,
            _ctx: &ExtractCtx<'_>,
            _dest: &Path,
        ) -> Result<ExtractReport, HzipError> {
            Err(HzipError::Unsupported {
                what: "mock 不实现 extract".into(),
            })
        }
        fn test(&self, _ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
            Err(HzipError::Unsupported {
                what: "mock 不实现 test".into(),
            })
        }
    }
}
