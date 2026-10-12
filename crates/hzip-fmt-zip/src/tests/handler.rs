use super::*;
use hzip_format::FormatCaps;

#[test]
fn caps_decl() {
    let c = caps();
    let want = FormatCaps {
        read: true,
        write: true,
        append: true,
        edit: true,
        encryption: EncryptionCaps::ZipCrypto,
        split: true,
        solid: false,
        comment: false,
        // 写侧压缩方式 = 单候选 Deflate（UI 的「压缩方式」下拉数据源）
        methods: &[hzip_codec::traits::CodecMethod::Deflate],
        write_encryption: &[
            EncryptionRequest::ZipCrypto,
            EncryptionRequest::Aes(AesKey::Bits128),
            EncryptionRequest::Aes(AesKey::Bits192),
            EncryptionRequest::Aes(AesKey::Bits256),
        ],
    };
    assert_eq!(c, want);
}

#[test]
fn signature_magic() {
    assert_eq!(ZipFormatHandler::new().signature().magic, b"PK\x03\x04");
}
