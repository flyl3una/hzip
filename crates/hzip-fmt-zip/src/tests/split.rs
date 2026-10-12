use super::*;

#[test]
fn splitter_byte_exact_and_finalize() {
    let dir = tempfile::tempdir().unwrap();
    let final_path = dir.path().join("out.zip");
    let mut sp = Splitter::new(final_path.clone(), 100).unwrap();
    let payload: Vec<u8> = (0..350u32).map(|i| (i % 251) as u8).collect();
    sp.write_all(&payload).unwrap();
    let vols = sp.finalize().unwrap();
    // 3 整卷 + 50B 末卷
    assert_eq!(vols.len(), 4);
    assert!(vols.last().unwrap().ends_with("out.zip"));
    // 拼接回读一致
    let mut cat = Vec::new();
    for v in &vols {
        cat.extend(std::fs::read(v).unwrap());
    }
    assert_eq!(cat, payload);
    // 各卷大小约束：非末卷 ≤ 100
    for v in &vols[..vols.len() - 1] {
        assert!(std::fs::metadata(v).unwrap().len() <= 100);
    }
}

#[test]
fn volume_set_reader_concat() {
    let dir = tempfile::tempdir().unwrap();
    let final_path = dir.path().join("x.zip");
    let mut sp = Splitter::new(final_path.clone(), 64).unwrap();
    let payload: Vec<u8> = (0..500u32).map(|i| (i % 250) as u8).collect();
    sp.write_all(&payload).unwrap();
    let vols = sp.finalize().unwrap();
    let mut r = VolumeSetReader::open(vols).unwrap();
    assert_eq!(r.total_len(), 500);
    let mut got = Vec::new();
    r.read_to_end(&mut got).unwrap();
    assert_eq!(got, payload);
    // 跨卷 seek
    r.seek(SeekFrom::Start(200)).unwrap();
    let mut tail = Vec::new();
    r.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, payload[200..]);
}
