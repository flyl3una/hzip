use super::*;

#[cfg(feature = "gz")]
#[test]
fn gzip_mtime_zero_is_none() {
    assert!(mtime_from_gzip_field(0).is_none());
    let t = mtime_from_gzip_field(1_700_000_000).expect("转换");
    assert_eq!(t.unix_timestamp(), 1_700_000_000);
}

#[cfg(feature = "gz")]
#[test]
fn gzip_field_roundtrip() {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let t = OffsetDateTime::from_unix_timestamp(now).expect("now");
    let field = gzip_mtime_field(Some(t));
    assert_eq!(field as i64, now);
    assert_eq!(gzip_mtime_field(None), 0);
}

#[test]
fn hzip_payload_roundtrip() {
    let millis = 1_700_000_000_000u64;
    let p = build_hzip_mtime_payload(millis);
    let t = parse_hzip_mtime_payload(&p).expect("解析私有帧");
    assert_eq!(t.unix_timestamp(), 1_700_000_000);
    // 非 hzip 载荷 → None
    assert!(parse_hzip_mtime_payload(b"not-hzip-time-xxx").is_none());
    // 长度不足 → None
    assert!(parse_hzip_mtime_payload(b"short").is_none());
}
