use super::*;

#[test]
fn replay_reader_preserves_captured_bytes() {
    let inner: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(b"abcdef".to_vec()));
    let mut rr = ReplayReader::new(inner);
    // 预读阶段捕获前 3 字节
    rr.start_capture();
    let mut head = vec![0u8; 3];
    rr.read_exact(&mut head).expect("read head");
    assert_eq!(head, b"abc");
    rr.stop_capture();
    // 后续完整读：应得 6 字节（3 重放 + 3 新）
    let mut all = Vec::new();
    rr.read_to_end(&mut all).expect("read all");
    assert_eq!(all, b"abcdef");
}

#[test]
fn replay_reader_no_capture_passthrough() {
    let inner: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(b"xyz".to_vec()));
    let mut rr = ReplayReader::new(inner);
    let mut all = Vec::new();
    rr.read_to_end(&mut all).expect("read all");
    assert_eq!(all, b"xyz");
}
