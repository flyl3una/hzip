use super::*;
use crate::filter::bcj2::Bcj2Decoder;
use std::io::{Cursor, Read, Write};
use std::sync::Arc;

/// 共享输出端（Write + Send + 'static；Arc<Mutex<Vec<u8>>> 承载）。
#[derive(Clone)]
struct SharedSink(Arc<std::sync::Mutex<Vec<u8>>>);

impl SharedSink {
    fn new() -> Self {
        Self(Arc::new(std::sync::Mutex::new(Vec::new())))
    }
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 编码器（输出到调用方持有的 SharedSink）。
fn make_encoder(m: &SharedSink, j: &SharedSink, c: &SharedSink, b: &SharedSink) -> Bcj2Encoder {
    Bcj2Encoder::new(
        Box::new(m.clone()),
        Box::new(j.clone()),
        Box::new(c.clone()),
        Box::new(b.clone()),
    )
}

fn to_reader(v: Vec<u8>) -> Box<dyn Read + Send> {
    Box::new(Cursor::new(v))
}

/// encode → 四流 → T12 解码器 → 字节还原。
fn roundtrip(label: &str, sample: &[u8]) {
    let (m, j, c, b) = (
        SharedSink::new(),
        SharedSink::new(),
        SharedSink::new(),
        SharedSink::new(),
    );
    let mut enc = make_encoder(&m, &j, &c, &b);
    enc.write_all(sample).unwrap();
    let stats = {
        let e: Box<Bcj2Encoder> = Box::new(enc);
        e.finish().unwrap()
    };
    assert_eq!(stats.bytes_in, sample.len() as u64, "bytes_in");
    assert_eq!(stats.call % 4, 0);
    assert_eq!(stats.jump % 4, 0);
    assert_eq!(
        stats.call / 4 + stats.jump / 4,
        stats.converted,
        "converted"
    );

    let (mb, cb, jb, bb) = (m.bytes(), c.bytes(), j.bytes(), b.bytes());
    if matches!(label, "e8-full" | "xorshift" | "branch") && std::env::var("BCJ2_DUMP").is_ok() {
        panic!("@{label} dump main={mb:02x?} call={cb:02x?} jump={jb:02x?} byte={bb:02x?}");
    }
    let dec = Bcj2Decoder::new(to_reader(mb), to_reader(cb), to_reader(jb), to_reader(bb));
    let mut dec = dec;
    let mut out = Vec::new();
    if let Err(e) = dec.read_to_end(&mut out) {
        panic!("BCJ2 解码失败 @ {label}: {e}");
    }
    assert_eq!(out, sample, "BCJ2 还原失败 @ {label}");
    let _ = dec.finish();
}

/// xorshift 伪随机（确定性）。
fn xorshift(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

/// E8/E9 密集 x86 代码体（call/jmp rel32 + 偶发 NOP）。
fn branch_dense(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let op = if x.is_multiple_of(2) { 0xE8u8 } else { 0xE9 };
        out.push(op);
        let disp = ((x % 65536) as i32) - 32768;
        out.extend_from_slice(&disp.to_le_bytes());
        let pad = (x >> 16) % 4;
        out.extend(std::iter::repeat_n(0x90, pad as usize));
    }
    out.truncate(len);
    out
}

/// 真实小 PE 形态头 + 分支密集体。
fn pe_like(seed: u32, len: usize) -> Vec<u8> {
    let mut v = b"MZ".to_vec();
    v.extend_from_slice(&[0u8; 58]);
    v.extend_from_slice(b"This program cannot be run in DOS mode.");
    v.resize(66, 0);
    v.extend_from_slice(&branch_dense(seed, len.saturating_sub(v.len())));
    v
}

/// 0F 8x (jcc rel32) 密集序列。
fn jcc_dense(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        out.push(0x0F);
        out.push(0x80 | ((x >> 8) & 0x0F) as u8);
        let disp = ((x % 65536) as i32) - 32768;
        out.extend_from_slice(&disp.to_le_bytes());
    }
    out.truncate(len);
    out
}

#[test]
fn decoder_alone_empty() {
    let dec = Bcj2Decoder::new(
        to_reader(Vec::new()),
        to_reader(Vec::new()),
        to_reader(Vec::new()),
        to_reader(vec![0u8; 5]),
    );
    let mut dec = dec;
    let mut out = Vec::new();
    let r = dec.read_to_end(&mut out);
    assert!(r.is_ok(), "empty main + 5zero rc 应干净 EOF: {r:?}");
    assert!(out.is_empty());
    dec.finish().unwrap();
}

#[test]
fn bcj2_encoder_symmetry_samples() {
    let samples: Vec<(&str, Vec<u8>)> = vec![
        ("empty", b"".to_vec()),
        ("plain", b"plain text, no branches here".to_vec()),
        ("e8-tail1", b"\xE8".to_vec()),
        ("e8-tail2", b"\xE8\x01".to_vec()),
        ("e8-tail3", b"\xE8\x01\x02\x03".to_vec()),
        ("e8-full", b"\xE8\x01\x02\x03\x04".to_vec()),
        ("jcc-full", b"\x0F\x80\x01\x02\x03\x04".to_vec()),
        ("jcc-tail", b"\x0F\x81\x01\x02\x03".to_vec()),
        ("branch", branch_dense(0x1234_5678, 8192)),
        ("pe", pe_like(0x9E37_79B9, 4096)),
        ("jcc", jcc_dense(0xDEAD_BEEF, 4096)),
        ("rand", xorshift(0xCAFE_F00D, 8192)),
        ("e8-run", b"\xE8\xE8\xE8\xE8\xE8".to_vec()),
    ];
    for (name, data) in &samples {
        roundtrip(name, data);
    }
    roundtrip("lit", b"plain text, no branches here");
    roundtrip("lit", b"\xE8"); // 流尾不完整候选（非转换 → 解码器 main 直通）
    roundtrip("lit", b"\xE8\x01");
    roundtrip("lit", b"\xE8\x01\x02\x03");
    roundtrip("lit", b"\xE8\x01\x02\x03\x04"); // 恰完整一组（转换）
    roundtrip("lit", b"\x0F\x80\x01\x02\x03\x04"); // jcc 完整（转换）
    roundtrip("lit", b"\x0F\x81\x01\x02\x03"); // jcc 不完整（非转换）
    roundtrip("lit", &branch_dense(0x1234_5678, 8192));
    roundtrip("lit", &pe_like(0x9E37_79B9, 4096));
    roundtrip("lit", &jcc_dense(0xDEAD_BEEF, 4096));
    roundtrip("lit", &xorshift(0xCAFE_F00D, 8192));
    roundtrip("lit", b"\xE8\xE8\xE8\xE8\xE8");
    roundtrip("lit", b"\xE8\xE9\xE8\xE9\xE8\xE9\xE8\xE9\xE8\xE9");
}

#[test]
fn bcj2_encoder_chunked_write() {
    let sample = branch_dense(0xABCD_EF01, 100_000);
    let (m1, j1, c1, b1) = (
        SharedSink::new(),
        SharedSink::new(),
        SharedSink::new(),
        SharedSink::new(),
    );
    let mut enc = make_encoder(&m1, &j1, &c1, &b1);
    enc.write_all(&sample).unwrap();
    let stats_all = {
        let e: Box<Bcj2Encoder> = Box::new(enc);
        e.finish().unwrap()
    };
    let (m2, j2, c2, b2) = (
        SharedSink::new(),
        SharedSink::new(),
        SharedSink::new(),
        SharedSink::new(),
    );
    let mut enc2 = make_encoder(&m2, &j2, &c2, &b2);
    for chunk in sample.chunks(7) {
        enc2.write_all(chunk).unwrap();
    }
    let stats2 = {
        let e: Box<Bcj2Encoder> = Box::new(enc2);
        e.finish().unwrap()
    };
    assert_eq!(stats_all, stats2, "分块写入应与整块写入统计一致");
}

#[test]
fn bcj2_stats_consistency() {
    let sample = jcc_dense(0x1122_3344, 2000);
    let (m, j, c, b) = (
        SharedSink::new(),
        SharedSink::new(),
        SharedSink::new(),
        SharedSink::new(),
    );
    let mut enc = make_encoder(&m, &j, &c, &b);
    enc.write_all(&sample).unwrap();
    let s = {
        let e: Box<Bcj2Encoder> = Box::new(enc);
        e.finish().unwrap()
    };
    assert_eq!(s.bytes_in, sample.len() as u64);
    assert_eq!(s.main, m.bytes().len() as u64);
    assert_eq!(s.jump, j.bytes().len() as u64);
    assert_eq!(s.call, c.bytes().len() as u64);
    assert_eq!(s.byte, b.bytes().len() as u64);
    assert_eq!(s.call % 4, 0);
    assert_eq!(s.jump % 4, 0);
    assert_eq!(s.converted, s.call / 4 + s.jump / 4);
    // main + 每转换位移 4 字节 = 输入（jcc 无未转换候选时）
    assert_eq!(s.bytes_in, s.main + s.converted * 4, "main 不含已转换位移");
}

#[test]
fn bcj2_encoder_not_panic_arbitrary() {
    for seed in [1u32, 7, 42, 999] {
        let mut data = xorshift(seed, 300);
        data.extend_from_slice(&[0xE8, 0xE9, 0x0F, 0x8F, 0x00]);
        roundtrip("xorshift", &data);
    }
    let all: Vec<u8> = (0..=255u8).cycle().take(3000).collect();
    roundtrip("allbytes", &all);
}
