use super::*;
use hzip_format::FormatHandler;

#[test]
fn caps_matrix() {
    use hzip_format::FormatCaps;
    let got = SINGLE_CAPS;
    let want = FormatCaps {
        read: true,
        write: true,
        append: false,
        edit: false,
        encryption: hzip_format::EncryptionCaps::None,
        split: false,
        solid: false,
        comment: false,
        methods: &[],
        write_encryption: &[],
    };
    assert_eq!(got, want);
}

/// 每变体的写侧压缩方式 = 容器规格对应的**唯一**编码器。
///
/// 守的是 UI 侧「压缩方式」下拉：单候选格式必须能被界面识别为「锁定值」，
/// 而锁定的是**哪一个**由此断言钉死（gz 锁 Deflate、bz2 锁 BZip2、
/// xz 锁 LZMA2、zst 锁 Zstd——错配会让界面显示与实际写入不一致）。
#[test]
fn caps_methods_are_per_variant_single() {
    use hzip_codec::traits::CodecMethod;
    use hzip_format::FormatHandler;
    let cases: &[(SingleHandler, CodecMethod)] = &[
        #[cfg(feature = "gz")]
        (gz(), CodecMethod::Deflate),
        #[cfg(feature = "bz2")]
        (bz2(), CodecMethod::BZip2),
        #[cfg(feature = "xz")]
        (xz(), CodecMethod::Lzma2),
        #[cfg(feature = "zst")]
        (zst(), CodecMethod::Zstd),
    ];
    for (h, want) in cases {
        let m = h.caps().methods;
        assert_eq!(m, &[*want], "{} 的压缩方式应为单候选 {want}", h.id());
    }
    // 基础常量保持空（变体差异只经 caps() 覆写，不污染共享常量）
    assert!(SINGLE_CAPS.methods.is_empty());
}

#[test]
fn editor_unsupported() {
    #[cfg(feature = "gz")]
    let h = SingleHandler::gz();
    #[cfg(not(feature = "gz"))]
    #[cfg(feature = "bz2")]
    let h = SingleHandler::bz2();
    #[cfg(not(any(feature = "gz", feature = "bz2")))]
    #[cfg(feature = "xz")]
    let h = SingleHandler::xz();
    #[cfg(not(any(feature = "gz", feature = "bz2", feature = "xz")))]
    #[cfg(feature = "zst")]
    let h = SingleHandler::zst();
    let e = h.open_editor(
        Source::File(std::path::PathBuf::from("x.gz")),
        &Default::default(),
    );
    assert_eq!(e.err().map(|e| e.code()), Some(5001));
}
