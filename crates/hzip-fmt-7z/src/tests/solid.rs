use super::*;

fn meta(name: &str, len: usize) -> EntryMeta {
    EntryMeta {
        name: name.into(),
        size_raw: Some(len as u64),
        ..Default::default()
    }
}

/// 块满恰在条目边界：小值块上限驱动（compress.solid_block_mib 覆盖语义同型）。
#[test]
fn solid_block_boundary() {
    let sb = SolidBuffer::new(0); // 默认 64 MiB
    assert_eq!(sb.cap, (SOLID_BLOCK_DEFAULT_MIB as usize) * 1024 * 1024);
    // 小值覆盖生效：1 MiB 块
    let mut small = SolidBuffer::new(1);
    let payload = vec![0x41u8; 700 * 1024];
    small.push(&meta("a.bin", payload.len()), &payload);
    assert!(!small.is_full(), "700KiB < 1MiB 未满");
    small.push(&meta("b.bin", payload.len()), &payload);
    assert!(small.is_full(), "1.4MiB > 1MiB 已满");
    let block = small.take_block().unwrap();
    assert_eq!(block.entries.len(), 2);
    assert_eq!(block.data.len(), payload.len() * 2);
    assert_eq!(block.entries[1].start, payload.len() as u64);
}

/// 单文件跨块、块满产出后缓冲清空可续写。
#[test]
fn solid_buffer_take_resets() {
    let mut sb = SolidBuffer::new(1);
    let a = vec![0x22u8; 600 * 1024];
    let b = vec![0x33u8; 600 * 1024];
    sb.push(&meta("a", a.len()), &a);
    sb.push(&meta("b", b.len()), &b);
    assert!(sb.is_full());
    let block = sb.take_block().unwrap();
    assert_eq!(block.entries.len(), 2);
    assert!(sb.entry_count() == 0 && sb.buffered() == 0, "取走后清空");
    sb.push(&meta("c", 10), b"0123456789");
    assert_eq!(sb.entry_count(), 1);
    let b2 = sb.take_block().unwrap();
    assert_eq!(b2.entries[0].len, 10);
}

/// 统一压缩 roundtrip：整块 → LZMA2 pack；逐条目 size/CRC 记账正确。
#[test]
fn solid_roundtrip_and_ratio() {
    use std::io::Read;
    let mut sb = SolidBuffer::new(0);
    // 相似文本（固实压缩比收益）
    let base: Vec<u8> = b"hzip solid block entry payload with repeated words archive matrix deflate lzma zstd bzip2 entry header checksum roundtrip golden asset seed deterministic compress extract volume"
        .iter()
        .copied()
        .cycle()
        .take(40_000)
        .collect();
    for i in 0..10 {
        let off = (i * 40) % 200;
        let data: Vec<u8> = base
            .iter()
            .copied()
            .skip(off)
            .cycle()
            .take(20_000)
            .collect();
        sb.push(&meta(&format!("f{i:02}.txt"), data.len()), &data);
    }
    let raw_len = sb.buffered();
    let block = sb.take_block().unwrap();
    let pf = compress_block(block, Level::Normal, 1, false).unwrap();
    assert_eq!(pf.unpacked_sizes.len(), 10);
    assert!(pf.pack.len() < raw_len, "文本固实压缩应有效");
    // 解压还原整块并逐条目切出校验 CRC
    let mut dec =
        sevenz_rust::lzma::LZMA2Reader::new(std::io::Cursor::new(&pf.pack), 1 << 24, None);
    let mut out = Vec::new();
    dec.read_to_end(&mut out).unwrap();
    assert_eq!(out.len(), raw_len);
    let mut off = 0usize;
    for (i, sz) in pf.unpacked_sizes.iter().enumerate() {
        let seg = &out[off..off + *sz as usize];
        let mut c = hzip_codec::crc::Crc32::new();
        c.update(seg);
        assert_eq!(c.finalize(), pf.crcs[i], "f{i:02} CRC");
        off += *sz as usize;
    }
}

/// BCJ2+ solid：编码串行约束登记 + 未交付路径明确报错（不静默错编）。
#[test]
fn solid_bcj2_serial_constraint() {
    let mut sb = SolidBuffer::new(0);
    let data = vec![0x90u8; 100];
    sb.push(&meta("x.bin", data.len()), &data);
    let block = sb.take_block().unwrap();
    let res = compress_block(block, Level::Normal, 1, true);
    assert!(
        matches!(res, Err(HzipError::Unsupported { .. })),
        "solid+BCJ2 自组装未交付 → 明确 Unsupported（串行约束登记于模块 rustdoc）"
    );
}
