use super::*;
use hzip_format::Signature;

#[test]
fn caps_decl() {
    // 能力如实声明：encryption=AesHeader（`aes` feature 门控）、write=True；
    // feature 裁剪时回落。
    //
    // **solid / split 都不在其中**（超声明修正：09 §15.7 与 02 §7.2）：7z 固实块打包
    // 与写侧分卷都尚未接通（`solid.rs`/`split.rs` 已编译但 writer 未接），能力必须
    // 如实声明为 false —— 配套断言 `solid_request_is_rejected` / `split_request_is_rejected`
    // 钉住「请求被显式拒绝，而非接受后静默忽略」。
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
    assert_eq!(crate::handler::caps(), want);
}

/// 分卷请求**必须被显式拒绝**，不得接受后静默忽略（与 `solid_request_is_rejected` 同款）。
///
/// 守的是一个同类的「超声明」缺陷：`caps.split` 曾为 true（`split` feature 全开时）
/// 而 writer 对 `Sink::Split` 直接 `Err Unsupported` ⇒ CLI/GUI 以为能分卷，一路走到
/// 落盘才失败（CLI 实测 `hzip c -f 7z --split 100m` → HZ-5001）。现 caps 恒为 false，
/// 这里再钉住 writer 侧的拒绝行为。
#[test]
fn split_request_is_rejected() {
    let h = SevenZipFormatHandler::new();
    let area = tempfile::TempDir::new().unwrap();
    let err = h
        .create_writer(
            Sink::Split {
                prefix: area.path().join("vol.7z"),
                bytes_per_volume: 1024,
            },
            &CreateOptions::default(),
        )
        .err()
        .expect("分卷请求应被拒绝（写侧分卷未接通，T46）");
    // 断言文案而非错误码：`Unsupported`（HZ-5001）的 Display 只带 what 文本，不带码。
    let msg = err.to_string();
    assert!(
        msg.contains("写入目标不支持") && msg.contains("Split"),
        "拒绝原因应指向「分卷写入目标未接通」: {msg}"
    );
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
