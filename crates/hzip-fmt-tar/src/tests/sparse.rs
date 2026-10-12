use super::*;
use hzip_format::FormatId;
use std::io;

fn fmt() -> FormatId {
    FormatId::from("tar")
}

#[test]
fn validate_extents_ok() {
    let ext = [(0u64, 4096u64), (8192, 1024), (65536, 4096)];
    validate(&ext, 70000, &fmt()).expect("升序区间合法");
    // 末区间末偏移 == 逻辑大小 允许（无尾洞）
    validate(&[(0, 4096)], 4096, &fmt()).expect("恰好占满合法");
}

#[test]
fn validate_extents_bad() {
    // 重叠
    assert!(validate(&[(0, 4096), (2048, 4096)], 8192, &fmt()).is_err());
    // 乱序
    assert!(validate(&[(8192, 1), (0, 1)], 9000, &fmt()).is_err());
    // 越出逻辑大小
    assert!(validate(&[(0, 4096)], 2048, &fmt()).is_err());
    // 空区间（len=0）跳过不报错
    validate(&[(0, 0), (1024, 512)], 4096, &fmt()).expect("零长区间容忍");
}

#[test]
fn write_sparse_pax_rebuilds_logical() {
    let dir = std::env::temp_dir().join(format!(
        "hzip-sparse-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("sparse.bin");
    let mut f = std::fs::File::create(&p).unwrap();
    // 区间：offset0..4KB 写 'A'；offset 1MiB 处 4B 'B'；逻辑大小 1MiB+4
    let extents = [(0u64, 4096u64), (1 << 20, 4)];
    let logical = (1 << 20) + 4;
    let mut data = Vec::new();
    data.extend_from_slice(&[b'A'; 4096]);
    data.extend_from_slice(b"BBBB");
    write_sparse_pax(
        &mut f,
        &extents,
        logical as u64,
        &mut io::Cursor::new(data),
        &fmt(),
    )
    .expect("稀疏写");
    drop(f);

    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(bytes.len(), logical, "逻辑大小");
    assert_eq!(&bytes[..4096], &[b'A'; 4096][..]);
    assert!(bytes[4096..(1 << 20)].iter().all(|&b| b == 0), "洞区读零");
    assert_eq!(&bytes[1 << 20..], b"BBBB");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn write_sparse_pax_overflow_rejected() {
    let dir = std::env::temp_dir().join(format!(
        "hzip-sparse-ovf-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bad.bin");
    let mut f = std::fs::File::create(&p).unwrap();
    // 区间超出逻辑大小 → 2004
    let r = write_sparse_pax(&mut f, &[(0, 4096)], 100, &mut io::empty(), &fmt());
    assert!(matches!(r, Err(e) if e.code() == 2004));
    drop(f);
    std::fs::remove_dir_all(&dir).unwrap();
}
