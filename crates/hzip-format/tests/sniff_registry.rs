//! 嗅探表与注册表集成测试（T20 §6.1；02 §7.4/§7.5）。
//!
//! 覆盖：每格式魔数正样本命中、负样本 2001、组合层判定（.tar.gz / .gz.gz /
//! 4 层嵌套 Quota）。
//!
//! 注：mock handler 属 crate 内 `#[cfg(test)]`（集成测试编译期不可见），
//! 注册表/mock 相关用例在 `registry.rs` 单元测试内覆盖。
#![allow(clippy::unwrap_used, clippy::expect_used)] // 测试代码：断言即意图

use std::io::Write;

use hzip_format::FormatRegistry;
use hzip_format::handler::Source;

/// 写临时文件并返回路径。
fn tmp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("hzip-t20-test");
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write");
    p
}

/// 以文件源嗅探并断言结果为 Single(expected)。
fn assert_single(p: &std::path::Path, expected: &str) -> hzip_format::Sniffed {
    let reg = FormatRegistry::new();
    let s = reg
        .sniff(&Source::File(p.to_path_buf()))
        .unwrap_or_else(|e| panic!("嗅探 {} 失败: {e}", p.display()));
    match &s {
        hzip_format::Sniffed::Single(id, _) => {
            assert_eq!(id.as_str(), expected, "{} 应命中 {expected}", p.display())
        }
        other => panic!("{} 应命中 Single({expected})，实际 {other:?}", p.display()),
    }
    s
}

#[test]
fn sniff_magic_positive() {
    // zip 本地头
    assert_single(&tmp_file("p.zip", b"PK\x03\x04rest-of-data"), "zip");
    // 7z
    assert_single(&tmp_file("p.7z", b"7z\xbc\xaf\x27\x1cdata"), "7z");
    // rar4 / rar5
    assert_single(&tmp_file("p4.rar", b"Rar!\x1a\x07\x00x"), "rar");
    assert_single(&tmp_file("p5.rar", b"Rar!\x1a\x07\x01\x00x"), "rar");
    // gz / bz2 / xz / zstd：压缩流命中后需解层判定内层——
    // 以真实可解样本（内容为无魔数文本 → Single(压缩流格式)）。
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(b"plain text content, no magic inside")
        .expect("w");
    assert_single(&tmp_file("p.gz", &enc.finish().expect("f")), "gz");

    let bz = {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(9));
        e.write_all(b"plain text content, no magic inside")
            .expect("w");
        e.finish().expect("f")
    };
    assert_single(&tmp_file("p.bz2", &bz), "bz2");

    let xz = {
        let mut e = xz2::write::XzEncoder::new(Vec::new(), 6);
        e.write_all(b"plain text content, no magic inside")
            .expect("w");
        e.finish().expect("f")
    };
    assert_single(&tmp_file("p.xz", &xz), "xz");

    let zst = {
        let mut e = zstd::stream::write::Encoder::new(Vec::new(), 3).expect("zst enc");
        e.write_all(b"plain text content, no magic inside")
            .expect("w");
        e.finish().expect("f")
    };
    assert_single(&tmp_file("p.zst", &zst), "zst");
}

#[test]
fn sniff_magic_tar_at_257() {
    // ustar 位于偏移 257
    let mut buf = vec![0u8; 257];
    buf.extend_from_slice(b"ustar");
    buf.extend_from_slice(b"00");
    assert_single(&tmp_file("p.tar", &buf), "tar");
}

#[test]
fn sniff_negative_bad_signature() {
    let reg = FormatRegistry::new();
    let p = tmp_file("junk.bin", b"this is not an archive at all, sorry");
    let err = reg.sniff(&Source::File(p)).expect_err("未知魔数应报错");
    assert_eq!(err.code(), 2001);
}

#[test]
fn sniff_extension_fallback_low() {
    // 无魔数但 .zipx 扩展名 → Single(zip, Low)
    let reg = FormatRegistry::new();
    let p = tmp_file("a.zipx", b"randombytes-no-magic-here");
    match reg.sniff(&Source::File(p)).expect("zipx 扩展名回退") {
        hzip_format::Sniffed::Single(id, conf) => {
            assert_eq!(id.as_str(), "zip");
            assert_eq!(conf, hzip_format::Confidence::Low);
        }
        other => panic!("zipx 应回退 Single(zip, Low)，实际 {other:?}"),
    }
}

#[test]
fn sniff_composite_tar_gz() {
    // 现场构造 gz(tar)：flate2 GzEncoder 包一个含 ustar 头的载荷
    let mut payload = vec![0u8; 257];
    payload.extend_from_slice(b"ustar");
    payload.extend_from_slice(b"00");
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&payload).expect("gz write");
    let gz = enc.finish().expect("gz finish");
    let p = tmp_file("a.tar.gz", &gz);

    let reg = FormatRegistry::new();
    match reg.sniff(&Source::File(p)).expect("tar.gz 嗅探") {
        hzip_format::Sniffed::Composite { layers, tail, .. } => {
            assert_eq!(layers.len(), 1);
            assert_eq!(layers[0].as_str(), "gz");
            match tail {
                hzip_format::CompositeTail::Container(id) => assert_eq!(id.as_str(), "tar"),
                other => panic!("尾应为 Container(tar)，实际 {other:?}"),
            }
        }
        other => panic!("tar.gz 应为 Composite，实际 {other:?}"),
    }
}

#[test]
fn sniff_composite_raw_double_gz() {
    // gz(gz) → 尾 Raw
    let mut enc1 = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc1.write_all(b"inner plain text").expect("w");
    let inner_gz = enc1.finish().expect("f1");
    let mut enc2 = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc2.write_all(&inner_gz).expect("w2");
    let outer_gz = enc2.finish().expect("f2");
    let p = tmp_file("a.txt.gz.gz", &outer_gz);

    let reg = FormatRegistry::new();
    match reg.sniff(&Source::File(p)).expect("gz.gz 嗅探") {
        hzip_format::Sniffed::Composite { layers, tail, .. } => {
            assert_eq!(layers.len(), 2);
            assert!(matches!(tail, hzip_format::CompositeTail::Raw));
        }
        other => panic!("gz.gz 应为 Composite Raw，实际 {other:?}"),
    }
}

#[test]
fn sniff_composite_depth_limit() {
    // 4 层 gz 嵌套 → Quota HZ-4504
    let mut data: Vec<u8> = b"payload".to_vec();
    for _ in 0..4 {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&data).expect("w");
        data = enc.finish().expect("f");
    }
    let p = tmp_file("deep.gz.gz.gz.gz", &data);
    let reg = FormatRegistry::new();
    let err = reg.sniff(&Source::File(p)).expect_err("4 层嵌套应报 Quota");
    assert_eq!(err.code(), 4504);
}

/// 回归：tar.zst 组合嗅探（T32 授权修复 read_head_bytes 空 buf 缺陷）。
///
/// 缺陷背景：zstd::Decoder 对空 buf 的 read 返回 Err（output buffer full），
/// 原 read_head_bytes 循环读空切片至 EOF → .tar.zst 嗅探必败。读满 512B 即停后
/// 本用例必须通过（zstd 帧 + 内层 ustar 头）。
#[test]
fn sniff_composite_tar_zst() {
    // 现场构造 zst(tar)：zstd 编码一个含 ustar 头的载荷
    let mut payload = vec![0u8; 257];
    payload.extend_from_slice(b"ustar");
    payload.extend_from_slice(b"00");
    // 载荷尾部用**不可压随机**数据（可压重复字节会让 zstd 帧 < 512B 而测不到
    // 「读满即停」路径——缺陷复现条件是解码输出 > 512B）
    let mut state = 0x1234_5678_9abc_def0u64;
    for _ in 0..2048 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        payload.push((state & 0xff) as u8);
    }
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).expect("zst enc");
    enc.write_all(&payload).expect("zst write");
    let zst = enc.finish().expect("zst finish");
    assert!(zst.len() > 512, "zstd 帧应 > 512B 覆盖读满路径");
    let p = tmp_file("a.tar.zst", &zst);

    let reg = FormatRegistry::new();
    match reg.sniff(&Source::File(p)).expect("tar.zst 嗅探") {
        hzip_format::Sniffed::Composite { layers, tail, .. } => {
            assert_eq!(layers.len(), 1);
            assert_eq!(layers[0].as_str(), "zst");
            match tail {
                hzip_format::CompositeTail::Container(id) => assert_eq!(id.as_str(), "tar"),
                other => panic!("尾应为 Container(tar)，实际 {other:?}"),
            }
        }
        other => panic!("tar.zst 应为 Composite，实际 {other:?}"),
    }
}
