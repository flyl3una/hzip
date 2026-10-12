use super::*;

/// 按 POSIX 构造 pax 记录行：`"{len} {key}={value}\n"`（len 含换行）。
fn record(key: &str, value: &str) -> String {
    let len = key.len() + value.len() + 4; // " " + "=" + value + "\n" + len 自身位数…
    // 迭代：len 字段位数影响总长
    let mut l = len;
    loop {
        let digits = format!("{l}").len();
        let total = digits + 1 + key.len() + 1 + value.len() + 1;
        if total == l {
            return format!("{l} {key}={value}\n");
        }
        l = total;
    }
}

#[test]
fn parse_record_basic() {
    let line = record("path", "dir/a-very-long-file-name.txt");
    let (k, v) = parse_record(line.as_bytes()).expect("record");
    assert_eq!(k, b"path");
    assert_eq!(v, b"dir/a-very-long-file-name.txt");
}

#[test]
fn parse_record_malformed_ignored() {
    assert!(parse_record(b"not-a-len path=x\n").is_none());
    assert!(parse_record(b"99999999999999 path=x\n").is_none());
    assert!(parse_record(b"5 path").is_none());
}

#[test]
fn pax_apply_path_linkpath_ignores_unknown() {
    let mut p = PaxOverride::default();
    p.apply(b"path", b"real/name.txt");
    p.apply(b"linkpath", b"../target");
    p.apply(b"mtime", b"1700000000.123456789");
    // 未知 key：忽略不报错（前向兼容，T22 §3.2 末行）
    p.apply(b"SCHILY.xattr.user.foo", b"bar");
    p.apply(b"comment", b"whatever");
    assert_eq!(p.path.as_deref(), Some("real/name.txt"));
    assert_eq!(p.linkpath.as_deref(), Some("../target"));
    let m = p.mtime.expect("mtime");
    assert!((m - 1_700_000_000.123_456_8).abs() < 1e-6, "小数秒 mtime");
    // 半套 GNU.sparse：pending offset 未配对 → finish_sparse Err（坏头 2004）
    p.apply(b"GNU.sparse.offset", b"1024");
    assert!(p.finish_sparse().is_err(), "有 offset 无 size → 坏头");
}

#[test]
fn pax_sparse_1_0_pairs() {
    let mut p = PaxOverride::default();
    p.apply(b"GNU.sparse.offset", b"0");
    p.apply(b"GNU.sparse.numbytes", b"4096");
    p.apply(b"GNU.sparse.offset", b"65536");
    p.apply(b"GNU.sparse.numbytes", b"128");
    p.apply(b"GNU.sparse.size", b"131072");
    p.apply(b"GNU.sparse.name", b"some-real-name");
    p.apply(b"GNU.sparse.major", b"1");
    p.apply(b"GNU.sparse.minor", b"0");
    assert!(p.is_sparse());
    p.finish_sparse().expect("配对完整");
    assert_eq!(
        p.sparse_map,
        vec![(0, 4096), (65536, 128)],
        "offset/size 成对累积"
    );
    assert_eq!(p.sparse_size, Some(131072));
}

#[test]
fn global_header_snapshot_and_merge() {
    let mut g = PaxGlobal::default();
    g.merge(record("path", "global/dir").as_bytes());
    g.merge(record("mtime", "1600000000.000000000").as_bytes());
    let snap = g.snapshot();
    assert_eq!(snap.path.as_deref(), Some("global/dir"));
    assert_eq!(snap.mtime, Some(1_600_000_000.0));
    // 局部 x 覆盖同 key
    let mut local = snap;
    local.apply(b"path", b"override/here");
    assert_eq!(local.path.as_deref(), Some("override/here"));
    // 另一条目不继承局部
    let snap2 = g.snapshot();
    assert_eq!(snap2.path.as_deref(), Some("global/dir"));
}

#[test]
fn map_err_truncated_header_signature() {
    let fmt = FormatId::from("tar");
    let eof = io::Error::new(io::ErrorKind::UnexpectedEof, "boom");
    assert!(matches!(
        map_err(eof, fmt.clone()),
        HzipError::Format {
            kind: FormatErrorKind::Truncated,
            ..
        }
    ));
    let hdr = io::Error::other("archive header checksum mismatch");
    assert!(matches!(
        map_err(hdr, fmt.clone()),
        HzipError::Format {
            kind: FormatErrorKind::BadHeader,
            ..
        }
    ));
    let sig = io::Error::new(io::ErrorKind::InvalidInput, "bad magic");
    assert!(matches!(
        map_err(sig, fmt),
        HzipError::Format {
            kind: FormatErrorKind::BadSignature,
            ..
        }
    ));
}
