use super::*;

/// 标识往返：`from_id(as_str(m)) == m`（UI 白名单 value / 配置落盘回读靠它，
/// 错位会让「选了 BZip2 变成 LZMA2」这类静默换编码器）。
#[test]
fn codec_method_str_roundtrip() {
    for m in [
        CodecMethod::Deflate,
        CodecMethod::BZip2,
        CodecMethod::Lzma2,
        CodecMethod::Zstd,
    ] {
        assert_eq!(CodecMethod::from_id(m.as_str()), Some(m), "{m} 往返");
        // 桥到开放标识：codec_id 的字符串形态与 as_str 同形
        assert_eq!(&*m.codec_id(), m.as_str());
    }
}

/// 别名不宽容：`lzma` / `zst` 不是 `lzma2` / `zstd`（未知值回落默认，
/// 不得静默选到另一个编码器）。
#[test]
fn codec_method_from_id_is_strict() {
    assert_eq!(CodecMethod::from_id("lzma"), None, "lzma ≠ lzma2");
    assert_eq!(CodecMethod::from_id("zst"), None, "zst ≠ zstd");
    assert_eq!(
        CodecMethod::from_id("Store"),
        None,
        "存储归级别轴，不属方式轴"
    );
    assert_eq!(CodecMethod::from_id(""), None);
    assert_eq!(CodecMethod::from_id("deflate64"), None);
}
