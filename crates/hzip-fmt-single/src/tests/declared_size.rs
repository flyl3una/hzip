use super::*;

/// gz：写一个已知载荷 → 尾部 ISIZE 必须等于载荷长度。
#[cfg(feature = "gz")]
#[test]
fn gz_size_from_isize() {
    let payload = vec![b'x'; 12_345];
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut enc, &payload).unwrap();
    let bytes = enc.finish().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.gz");
    std::fs::write(&p, &bytes).unwrap();
    assert_eq!(
        declared_raw_size(SingleFmt::Gz, &p, bytes.len() as u64),
        Some(12_345)
    );
    // 太短的文件 → None（不 panic）
    let q = dir.path().join("short.gz");
    std::fs::write(&q, b"\x1f\x8b").unwrap();
    assert_eq!(declared_raw_size(SingleFmt::Gz, &q, 2), None);
}

/// xz：写一个已知载荷 → 尾部索引求和必须等于载荷长度。
#[cfg(feature = "xz")]
#[test]
fn xz_size_from_index() {
    let payload = vec![b'y'; 200_000];
    let mut enc = xz2::write::XzEncoder::new(Vec::new(), 1);
    std::io::Write::write_all(&mut enc, &payload).unwrap();
    let bytes = enc.finish().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.xz");
    std::fs::write(&p, &bytes).unwrap();
    assert_eq!(
        declared_raw_size(SingleFmt::Xz, &p, bytes.len() as u64),
        Some(200_000)
    );
    // 截断（只剩头 12 字节）→ None
    let q = dir.path().join("cut.xz");
    std::fs::write(&q, &bytes[..12]).unwrap();
    assert_eq!(declared_raw_size(SingleFmt::Xz, &q, 12), None);
    // 尾部魔数被改坏 → None
    let mut bad = bytes.clone();
    let n = bad.len();
    bad[n - 1] = b'X';
    let r = dir.path().join("bad.xz");
    std::fs::write(&r, &bad).unwrap();
    assert_eq!(declared_raw_size(SingleFmt::Xz, &r, bad.len() as u64), None);
}

#[test]
fn vli_bounds_and_encoding() {
    // 0x7f = 127（单字节）
    assert_eq!(read_vli(&[0x7f], &mut 0), Some(127));
    // 128 = 0x80 0x01（续位）
    assert_eq!(read_vli(&[0x80, 0x01], &mut 0), Some(128));
    // 越界 → None；超过 9 字节 → None
    assert_eq!(read_vli(&[], &mut 0), None);
    assert_eq!(read_vli(&[0x80; 10], &mut 0), None);
}

/// 无该字段的格式恒 `None`（bz2；zst 的大小在帧头，不在此模块）。
#[test]
fn other_formats_are_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.bin");
    std::fs::write(&p, b"whatever").unwrap();
    #[cfg(feature = "bz2")]
    assert_eq!(
        declared_raw_size(SingleFmt::Bz2, &p, 8),
        None,
        "bz2 无原始大小字段"
    );
}
