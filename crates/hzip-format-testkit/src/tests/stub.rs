use super::*;

#[test]
fn stub_roundtrip_bytes() {
    let entries = vec![
        MockEntry::file("a.txt", b"hello".to_vec()),
        MockEntry::file("dir/b.bin", vec![1, 2, 3, 4]),
    ];
    let bytes = StubArchive::to_bytes(&entries);
    let parsed = StubArchive::parse(&bytes).expect("parse");
    assert_eq!(parsed.entries.len(), 2);
    assert_eq!(&*parsed.entries[0].data, b"hello");
}

#[test]
fn stub_bad_magic() {
    let err = StubArchive::parse(b"NOPE").expect_err("坏魔数");
    assert_eq!(err.code(), 2001);
}
