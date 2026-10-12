use super::*;
use crate::RarHandler;
use hzip_format::FormatHandler;

#[test]
fn caps_decl_read_only() {
    let c = RarHandler::new().caps();
    let want = FormatCaps {
        read: true,
        write: false,
        append: false,
        edit: false,
        encryption: EncryptionCaps::Aes(AesKey::Bits256),
        split: true,
        solid: true,
        comment: false,
        methods: &[],
        write_encryption: &[],
    };
    assert_eq!(c, want);
}

#[test]
fn signature_rar_magic() {
    let s = RarHandler::new().signature();
    assert_eq!(s.offset, 0);
    // rar5 长魔数优先（handler 报 8 B 魔数）
    assert_eq!(s.magic, &MAGIC_RAR5);
    assert!(s.display == "rar");
    // 双版本魔数共享前 6 B `Rar!\x1A\x07`，第 7 B 区分版本
    assert_eq!(&MAGIC_RAR5[..6], b"Rar!\x1a\x07");
    assert_eq!(MAGIC_RAR4[6], 0x00, "rar4 第 7 字节 0x00（7 B 总长）");
    assert_eq!(MAGIC_RAR5[6], 0x01, "rar5 第 7 字节 0x01");
    assert_eq!(MAGIC_RAR5[7], 0x00, "rar5 末字节 0x00（8 B 总长）");
    assert_eq!(MAGIC_RAR4.len(), 7);
    assert_eq!(MAGIC_RAR5.len(), 8);
}

#[test]
fn write_and_edit_unsupported_5001() {
    let h = RarHandler::new();
    let code_of = |r: Result<Box<dyn hzip_format::ArchiveWriter>, hzip_error::HzipError>| {
        r.err().map(|e| e.code())
    };
    // create_writer（非 append）→ Unsupported 5001
    let code = code_of(h.create_writer(
        hzip_format::Sink::File(std::path::PathBuf::from("x.rar")),
        &Default::default(),
    ));
    assert_eq!(code, Some(5001));
    // create_writer(append=true) → 同上（append=false 负断言，TC-CONF-005）
    let code2 = code_of(h.create_writer(
        hzip_format::Sink::File(std::path::PathBuf::from("x.rar")),
        &hzip_format::CreateOptions {
            append: true,
            ..Default::default()
        },
    ));
    assert_eq!(code2, Some(5001));
    // 文案指明 rar 不支持压缩（HZ-5001 语义）
    let msg = h
        .create_writer(
            hzip_format::Sink::File(std::path::PathBuf::from("x.rar")),
            &Default::default(),
        )
        .map(|_| String::new())
        .unwrap_err()
        .to_string();
    assert!(msg.contains("rar 不支持压缩"), "文案应指明: {msg}");
    // open_editor → 同上（caps.edit=false）
    let code3 = h
        .open_editor(
            hzip_format::Source::File(std::path::PathBuf::from("x.rar")),
            &Default::default(),
        )
        .err()
        .map(|e| e.code());
    assert_eq!(code3, Some(5001));
}
