use super::*;
use hzip_format::Signature;

#[test]
fn caps_decl() {
    // T46 caps 升级（feature 全开时）：encryption=AesHeader、split=true（02 §7.2）；
    // feature 裁剪时回落。断言按编译 feature 展开。
    //
    // **solid 不在其中**（09 §15.7 超声明修正）：7z 固实块打包尚未接通，能力
    // 如实声明为 false —— 与之配套的 `solid_request_is_rejected` 断言请求会被
    // 显式拒绝（而非接受后静默忽略）。
    let mut want = FormatCaps {
        read: true,
        write: true,
        append: false,
        edit: true,
        encryption: EncryptionCaps::None,
        split: false,
        solid: false,
        comment: false,
        // 压缩方式不受 feature 门控：sevenz-rust 恒在（LZMA2 单候选）
        methods: &[hzip_codec::traits::CodecMethod::Lzma2],
        write_encryption: &[],
    };
    #[cfg(feature = "aes")]
    {
        want.encryption = EncryptionCaps::AesHeader;
        want.write_encryption = &[EncryptionRequest::Aes(AesKey::Bits256)];
    }
    #[cfg(feature = "split")]
    {
        want.split = true;
    }
    assert_eq!(crate::handler::caps(), want);
}

/// 固实请求**必须被显式拒绝**，不得接受后静默忽略。
///
/// 守的是一个已经发生过的缺陷：`caps.solid` 曾为 true 而 writer 从不消费
/// `opts.solid`，于是界面勾了「固实」、条目却仍逐条压成各自 folder——用户
/// 以为压缩率会变好，实际一点没变，且无从察觉。
#[test]
fn solid_request_is_rejected() {
    let h = SevenZipFormatHandler::new();
    let area = tempfile::TempDir::new().unwrap();
    let out = area.path().join("x.7z");
    let opts = CreateOptions {
        solid: true,
        ..Default::default()
    };
    let err = h
        .create_writer(Sink::File(out), &opts)
        .err()
        .expect("固实请求应被拒绝（尚未接通）");
    assert!(
        err.to_string().contains("5001") || err.to_string().contains("固实"),
        "拒绝原因应指向「固实未接通」: {err}"
    );
}

#[test]
fn signature_fields() {
    let s: &'static Signature = SevenZipFormatHandler::new().signature();
    assert_eq!(s.magic, b"7z\xBC\xAF\x27\x1C");
    assert_eq!(s.offset, 0);
    assert!(s.extensions.contains(&"7z"));
}
