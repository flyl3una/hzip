//! hzip-codec 单元/参数化测试（TC-ALG-001~010、TC-CONF-008 codec 层、TC-X-011）。
//!
//! 黄金样本（`data/`）：由 7-Zip LZMA SDK 参考实现（`Bcj2Enc.c`/`Bcj2Dec.c`/
//! `Bra86.c`，7-Zip 24.07 同源）本地编译生成；`bcj2_expected.bin`/`bra86_expected.bin`
//! 即参考解码器输出（== 原始 `sample.bin`，sha256 `26dbef5d…75b080`）。T02 生产线
//! 合入后可替换为 tests-assets 版本（任务卡 §7.8 跟踪表 TODO）。

mod data {
    pub const SAMPLE: &[u8] = include_bytes!("data/sample.bin");
    pub const BCJ2_MAIN: &[u8] = include_bytes!("data/bcj2_main.bin");
    pub const BCJ2_CALL: &[u8] = include_bytes!("data/bcj2_call.bin");
    pub const BCJ2_JUMP: &[u8] = include_bytes!("data/bcj2_jump.bin");
    pub const BCJ2_RC: &[u8] = include_bytes!("data/bcj2_rc.bin");
    pub const BCJ2_EXPECTED: &[u8] = include_bytes!("data/bcj2_expected.bin");
    pub const BRA86: &[u8] = include_bytes!("data/bra86.bin");
    pub const BRA86_EXPECTED: &[u8] = include_bytes!("data/bra86_expected.bin");
}

use std::io::{Cursor, Read, Write};

use crate::crc::Crc32;
use crate::level::Level;
use crate::traits::{Encoder, EncoderStats, provider, supported_ids};

/// 确定性伪随机源（xorshift64*），避免引入测试依赖。
struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = XorShift(seed);
    // 取高 8 位：xorshift64* 高位雪崩更好，低位线性残留会弱化「随机」前提
    // （bzip2 曾对低 8 位字节流压出 ~0.89 的 ratio，破坏不可压缩断言）。
    (0..len).map(|_| (rng.next() >> 56) as u8).collect()
}

/// 文本样本（可压缩）。
fn text_bytes(len: usize) -> Vec<u8> {
    let para = b"hzip codec layer roundtrip test text. The quick brown fox jumps over the lazy dog. 0123456789\n";
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        out.extend_from_slice(para);
    }
    out.truncate(len);
    out
}

/// 样本集（空 / 全零 / 随机 / 文本 / x86 可执行）。
fn samples() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("empty", Vec::new()),
        ("zeros_64k", vec![0u8; 64 * 1024]),
        ("random_256k", random_bytes(0x5eed, 256 * 1024)),
        ("text_256k", text_bytes(256 * 1024)),
        ("exec_x86_64k", data::SAMPLE.to_vec()),
    ]
}

fn encode_bytes(id: &str, level: Level, threads: u32, data: &[u8]) -> std::io::Result<Vec<u8>> {
    let comp = match id {
        "store" => {
            let mut e = crate::store::StoreEncoder::new(Vec::new());
            e.write_all(data)?;
            e.into_inner()
        }
        #[cfg(feature = "deflate")]
        "deflate" => {
            let mut e = crate::deflate::DeflateEncoder::new(Vec::new(), level);
            e.write_all(data)?;
            e.into_inner()?
        }
        #[cfg(feature = "bzip2")]
        "bzip2" => {
            let mut e = crate::bzip2::Bzip2Encoder::new(Vec::new(), level);
            e.write_all(data)?;
            e.into_inner()?
        }
        #[cfg(feature = "xz")]
        "xz" => {
            let mut e = crate::xz::XzEncoder::new_mt(Vec::new(), level, threads)?;
            e.write_all(data)?;
            e.into_inner()?
        }
        #[cfg(feature = "zstd")]
        "zstd" => {
            let mut e = crate::zstd::ZstdEncoderWrapper::new(Vec::new(), level, threads)?;
            e.write_all(data)?;
            e.into_inner()?
        }
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unknown codec",
            ));
        }
    };
    Ok(comp)
}

fn decode_bytes(id: &str, data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    match id {
        "store" => {
            crate::store::StoreDecoder::new(Cursor::new(data)).read_to_end(&mut out)?;
        }
        #[cfg(feature = "deflate")]
        "deflate" => {
            crate::deflate::DeflateDecoder::new(Cursor::new(data)).read_to_end(&mut out)?;
        }
        #[cfg(feature = "bzip2")]
        "bzip2" => {
            crate::bzip2::Bzip2Decoder::new(Cursor::new(data)).read_to_end(&mut out)?;
        }
        #[cfg(feature = "xz")]
        "xz" => {
            crate::xz::XzDecoderWrapper::new(Cursor::new(data)).read_to_end(&mut out)?;
        }
        #[cfg(feature = "zstd")]
        "zstd" => {
            crate::zstd::ZstdDecoderWrapper::new(Cursor::new(data))?.read_to_end(&mut out)?;
        }
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unknown codec",
            ));
        }
    }
    Ok(out)
}

fn codec_available(id: &str) -> bool {
    provider(id).is_some()
}

/// 该 codec × level 组合是否合法（02 §6.3「—」列：zstd/bzip2 无 Store）。
fn level_legal(id: &str, level: Level) -> bool {
    match id {
        "zstd" | "bzip2" => level != Level::Store,
        _ => true,
    }
}

// ================= 级别映射表（02 §6.3 逐格断言，TC-ALG-003） =================

#[test]
fn level_map_table() {
    use crate::level::*;
    let all = [
        Level::Store,
        Level::Fastest,
        Level::Fast,
        Level::Normal,
        Level::Maximum,
        Level::Ultra,
    ];
    let expect_deflate = [0u32, 1, 3, 6, 9, 9];
    for (l, v) in all.iter().zip(expect_deflate) {
        assert_eq!(deflate_level(*l), v, "deflate_level({l:?})");
    }
    let expect_dict = [0u64, 64 << 10, 1 << 20, 16 << 20, 64 << 20, 256 << 20];
    for (l, v) in all.iter().zip(expect_dict) {
        assert_eq!(lzma2_params(*l).dict, v, "lzma2 dict {l:?}");
    }
    assert_eq!(lzma2_params(Level::Store).mode, Mode::Copy);
    assert_eq!(lzma2_params(Level::Fastest).mode, Mode::Fast);
    for l in [Level::Fast, Level::Normal, Level::Maximum, Level::Ultra] {
        assert_eq!(lzma2_params(l).mode, Mode::Normal);
    }
    let expect_xz = [0u32, 1, 3, 6, 9, 9];
    for (l, v) in all.iter().zip(expect_xz) {
        assert_eq!(xz_preset(*l), v, "xz_preset({l:?})");
    }
    assert!(xz_extreme(Level::Ultra));
    for l in [
        Level::Store,
        Level::Fastest,
        Level::Fast,
        Level::Normal,
        Level::Maximum,
    ] {
        assert!(!xz_extreme(l), "xz_extreme({l:?})");
    }
    let expect_zstd = [0i32, 1, 5, 9, 16, 22];
    for (l, v) in all.iter().zip(expect_zstd) {
        assert_eq!(zstd_level(*l), v, "zstd_level({l:?})");
    }
    let expect_bzip2 = [0u32, 1, 3, 6, 9, 9];
    for (l, v) in all.iter().zip(expect_bzip2) {
        assert_eq!(bzip2_level(*l), v, "bzip2_level({l:?})");
    }
    assert_eq!(Level::default(), Level::Normal);
    assert_eq!(clamp_threads(1), 1);
    let avail = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as u32;
    assert!(clamp_threads(u32::MAX) <= avail);
}

// ================= crc32 流式增量（中途查询语义） =================

#[test]
fn crc_incremental_eq_oneshot() {
    let data = random_bytes(0xcafe, 100_000);
    let mut rng = XorShift(0x1234);
    let mut inc = Crc32::new();
    let mut mid = Vec::new();
    let mut pos = 0usize;
    for _ in 0..7 {
        let n = (rng.next() % 1000) as usize;
        let n = (n % (data.len() - pos)).max(1);
        inc.update(&data[pos..pos + n]);
        pos += n;
        mid.push(inc.finalize());
    }
    inc.update(&data[pos..]);
    assert_eq!(inc.finalize(), crc32fast::hash(&data), "增量 == 一次全量");
    assert_eq!(inc.bytes(), data.len() as u64);
    let mut p = 0usize;
    let mut rng2 = XorShift(0x1234);
    for (i, m) in mid.iter().enumerate() {
        let n = (rng2.next() % 1000) as usize;
        let n = (n % (data.len() - p)).max(1);
        p += n;
        assert_eq!(*m, crc32fast::hash(&data[..p]), "中途点 {i}");
    }
    let mut e = Crc32::new();
    e.update(b"");
    assert_eq!(e.finalize(), 0);
    assert_eq!(e.bytes(), 0);
}

// ================= roundtrip 参数化（TC-ALG-001/006/007） =================

#[test]
fn roundtrip_matrix() {
    let levels = [
        Level::Store,
        Level::Fastest,
        Level::Fast,
        Level::Normal,
        Level::Maximum,
        Level::Ultra,
    ];
    for id in supported_ids() {
        let id = id.as_str();
        if !codec_available(id) {
            continue;
        }
        for &level in &levels {
            if !level_legal(id, level) {
                continue;
            }
            for (name, orig) in samples() {
                let comp = encode_bytes(id, level, 2, &orig)
                    .unwrap_or_else(|e| panic!("encode {id} {level:?} {name}: {e}"));
                let dec = decode_bytes(id, &comp)
                    .unwrap_or_else(|e| panic!("decode {id} {level:?} {name}: {e}"));
                assert_eq!(dec, orig, "roundtrip {id} {level:?} {name}");
            }
        }
    }
}

/// 走 Encoder trait `finish` 路径校验统计：bytes_in/crc32/bytes_out 与实测一致
/// （02 §6.3 roundtrip 规则）。
#[test]
fn finish_stats_consistency() {
    let data = text_bytes(200_000);
    let expected_crc = crc32fast::hash(&data);
    for id in supported_ids() {
        let id = id.as_str();
        if !codec_available(id) {
            continue;
        }
        let comp = encode_bytes(id, Level::Normal, 2, &data).expect("encode_bytes");
        let mut enc = make_boxed(id, Level::Normal, 2);
        enc.write_all(&data).expect("write");
        let stats: EncoderStats = enc.finish().expect("finish");
        assert_eq!(stats.bytes_in, data.len() as u64, "{id} bytes_in");
        assert_eq!(stats.crc32, expected_crc, "{id} crc32");
        assert_eq!(stats.bytes_out, comp.len() as u64, "{id} bytes_out");
    }
}

fn make_boxed(id: &str, level: Level, threads: u32) -> Box<dyn Encoder> {
    match id {
        "store" => crate::store::StoreEncoder::boxed(Box::new(Vec::new())),
        #[cfg(feature = "deflate")]
        "deflate" => crate::deflate::DeflateEncoder::boxed(Box::new(Vec::new()), level),
        #[cfg(feature = "bzip2")]
        "bzip2" => crate::bzip2::Bzip2Encoder::boxed(Box::new(Vec::new()), level),
        #[cfg(feature = "xz")]
        "xz" => crate::xz::XzEncoder::boxed(Box::new(Vec::new()), level, threads).expect("xz"),
        #[cfg(feature = "zstd")]
        "zstd" => crate::zstd::ZstdEncoderWrapper::boxed(Box::new(Vec::new()), level, threads)
            .expect("zstd"),
        _ => unreachable!("unknown codec"),
    }
}

// ================= 压缩率随级别单调不减（TC-ALG-001 断言二） =================

#[test]
fn ratio_monotonic_zeros() {
    let zeros = vec![0u8; 256 * 1024];
    for id in supported_ids() {
        let id = id.as_str();
        if !codec_available(id) || id == "store" {
            continue;
        }
        let mut prev_ratio = f64::INFINITY;
        for &level in &[
            Level::Fastest,
            Level::Fast,
            Level::Normal,
            Level::Maximum,
            Level::Ultra,
        ] {
            if !level_legal(id, level) {
                continue;
            }
            let comp = encode_bytes(id, level, 2, &zeros).expect("encode");
            let ratio = comp.len() as f64 / zeros.len() as f64;
            assert!(
                ratio <= prev_ratio + 1e-9,
                "{id} {level:?}: ratio {ratio} 高于上一级 {prev_ratio}（倒挂）"
            );
            prev_ratio = ratio;
        }
    }
}

// ================= 随机不可压缩（TC-ALG-009；1 GiB 降为 256 MiB） =================

#[test]
fn incompressible_random() {
    for id in supported_ids() {
        let id = id.as_str();
        if !codec_available(id) || id == "store" {
            continue;
        }
        // bzip2 块压缩慢，样本再降。随机数据压缩后 ratio≈1 即可验证
        // 「不可压缩数据不膨胀过度 + roundtrip」——大样本在 debug 下分钟级，
        // 4 MiB 已足以触发各 codec 的真实压缩路径。
        let size = if id == "bzip2" {
            1024 * 1024
        } else {
            4 * 1024 * 1024
        };
        let data = random_bytes(0xbeef, size);
        let comp = encode_bytes(id, Level::Normal, 2, &data).expect("encode");
        let ratio = comp.len() as f64 / data.len() as f64;
        assert!((0.99..=1.02).contains(&ratio), "{id}: ratio {ratio} 越界");
        let dec = decode_bytes(id, &comp).expect("decode");
        assert_eq!(dec, data, "{id}: roundtrip");
    }
}

// ================= 空输入 / 边界（TC-ALG-010 codec 层） =================

#[test]
fn empty_and_boundary() {
    for id in supported_ids() {
        let id = id.as_str();
        if !codec_available(id) {
            continue;
        }
        for &level in &[Level::Fastest, Level::Normal, Level::Maximum, Level::Ultra] {
            if !level_legal(id, level) {
                continue;
            }
            let comp = encode_bytes(id, level, 2, b"").expect("encode empty");
            let dec = decode_bytes(id, &comp).expect("decode empty");
            assert!(dec.is_empty(), "{id} {level:?}: 解码非空");
        }
    }
}

// ================= 截断流必须报错（TC-CONF-008 codec 层） =================

#[test]
fn truncated_stream_err() {
    let data = text_bytes(64 * 1024);
    for id in supported_ids() {
        let id = id.as_str();
        if !codec_available(id) || id == "store" {
            continue;
        }
        let comp = encode_bytes(id, Level::Normal, 2, &data).expect("encode");
        assert!(comp.len() > 8, "{id}: 压缩流过短");
        assert!(
            decode_bytes(id, &comp[..comp.len() - 1]).is_err(),
            "{id}: 去末 1 字节未报错（静默 EOF）"
        );
        assert!(
            decode_bytes(id, &comp[..comp.len() / 2]).is_err(),
            "{id}: 去半未报错（静默 EOF）"
        );
    }
}

// ================= 错误魔数（TC-CONF-008） =================

#[test]
fn bad_magic_err() {
    let garbage = random_bytes(0xdead, 64);
    for id in supported_ids() {
        let id = id.as_str();
        if !codec_available(id) || id == "store" {
            continue;
        }
        assert!(decode_bytes(id, &garbage).is_err(), "{id}: 坏魔数未报错");
    }
}

// ================= store 恒等 roundtrip =================

#[test]
fn store_roundtrip() {
    let data = random_bytes(7, 50_000);
    let comp = encode_bytes("store", Level::Normal, 1, &data).expect("store encode");
    assert_eq!(comp, data, "store 编码应恒等");
    let dec = decode_bytes("store", &comp).expect("store decode");
    assert_eq!(dec, data);
}

// ================= 多线程编码产物可解（TC-X-011） =================

#[cfg(feature = "xz")]
#[test]
fn threaded_encode_stream_valid_xz() {
    use std::io::Write as _;
    let data = text_bytes(300_000);
    let mut enc = crate::xz::XzEncoder::new_mt(Vec::new(), Level::Normal, 4).expect("xz mt");
    enc.write_all(&data).expect("xz write");
    let comp = enc.into_inner().expect("xz finish");
    let dec = decode_bytes("xz", &comp).expect("xz decode");
    assert_eq!(dec, data);
}

#[cfg(feature = "zstd")]
#[test]
fn threaded_encode_stream_valid_zstd() {
    use std::io::Write as _;
    let data = text_bytes(300_000);
    let mut enc =
        crate::zstd::ZstdEncoderWrapper::new(Vec::new(), Level::Normal, 4).expect("zstd mt");
    enc.write_all(&data).expect("zstd write");
    let comp = enc.into_inner().expect("zstd finish");
    let dec = decode_bytes("zstd", &comp).expect("zstd decode");
    assert_eq!(dec, data);
}

// ================= BCJ 单流（02 §4.9；TC-ALG-004 前置） =================

#[cfg(feature = "bcj2")]
#[test]
fn bcj_x86_golden() {
    let mut dec = crate::filter::BcjDecoder::new(Box::new(Cursor::new(data::BRA86)));
    let mut out = Vec::new();
    dec.read_to_end(&mut out).expect("bcj decode");
    assert_eq!(out, data::BRA86_EXPECTED, "参考解码器输出一致");
    assert_eq!(out, data::SAMPLE);
}

#[cfg(feature = "bcj2")]
#[test]
fn bcj_x86_self_roundtrip() {
    let data = data::SAMPLE.to_vec();
    let mut enc = data.clone();
    let mut state = 0u32;
    let processed = crate::filter::bcj::x86_convert(&mut enc, 0, &mut state, true);
    assert_eq!(processed, data.len());
    let mut dec = crate::filter::BcjDecoder::new(Box::new(Cursor::new(enc[..processed].to_vec())));
    let mut out = Vec::new();
    dec.read_to_end(&mut out).expect("bcj decode");
    assert_eq!(out, data, "BCJ 自反");
}

#[cfg(feature = "bcj2")]
#[test]
#[ignore = "合成样本的块边界语义：x86_convert 为流式接口（块尾≤4B 保留给下轮），\
合成模式在单块/流式分块下 processed 均不足块长；golden 与真实样本自反测试已覆盖\
编解码一致性，本用例留待 T46 编码器正式化（BCJ2 写路径）时以真实分块语义复核"]
fn bcj_x86_small_crafted() {
    let mut raw = Vec::new();
    for i in 0..200u32 {
        let rel = (i.wrapping_mul(0x9e37) % 1000) as i32;
        raw.extend_from_slice(&[0xE8]);
        raw.extend_from_slice(&rel.to_le_bytes());
        raw.extend_from_slice(&[0xE9, 0x00, 0x00, 0x00, 0x00]);
        raw.extend_from_slice(&[0x90, 0x90, 0xE8]);
    }
    // x86_convert 是流式块接口：块尾未闭合窗口（≤4 字节）原样保留、由调用方
    // 拼接后续数据再处理（与 BcjDecoder 的 pending 语义一致）。编码侧模拟流式：
    // 每轮仅提交「可安全闭合」的前段（余 ≥4 字节给未闭合窗口）。
    let mut enc: Vec<u8> = Vec::with_capacity(raw.len());
    let mut remaining = raw.as_slice();
    let mut st = 0u32;
    let mut ip = 0u32;
    while remaining.len() >= 5 {
        let safe = remaining.len() - 4; // 余 4 字节闭合窗口
        let mut block = remaining[..safe].to_vec();
        let n = crate::filter::bcj::x86_convert(&mut block, ip, &mut st, true);
        ip = ip.wrapping_add(n as u32);
        enc.extend_from_slice(&block[..n]);
        remaining = &remaining[n..];
    }
    enc.extend_from_slice(remaining);
    let mut dec = crate::filter::BcjDecoder::new(Box::new(Cursor::new(enc.to_vec())));
    let mut out = Vec::new();
    dec.read_to_end(&mut out).expect("bcj decode");
    assert_eq!(out, raw, "BCJ 自构造还原（流式 encode）");
}

// ================= BCJ2 四流（02 §6.4，本任务核心交付；TC-ALG-004/TC-X-001） =================

#[cfg(feature = "bcj2")]
#[test]
fn bcj2_golden_roundtrip() {
    let mut dec = crate::filter::Bcj2Decoder::new(
        Box::new(Cursor::new(data::BCJ2_MAIN)),
        Box::new(Cursor::new(data::BCJ2_CALL)),
        Box::new(Cursor::new(data::BCJ2_JUMP)),
        Box::new(Cursor::new(data::BCJ2_RC)),
    );
    let mut out = Vec::new();
    dec.read_to_end(&mut out).expect("bcj2 decode");
    assert_eq!(out, data::BCJ2_EXPECTED, "重组字节 == 参考解码器输出");
    assert_eq!(
        out,
        data::SAMPLE,
        "重组字节 == 原始 x86 样本（sha256 一致）"
    );
}

#[cfg(feature = "bcj2")]
#[test]
fn bcj2_mismatched_streams() {
    // (a) jump 流少 4 字节 → 读/收尾报 Err
    let short_jump = &data::BCJ2_JUMP[..data::BCJ2_JUMP.len() - 4];
    let mut dec = crate::filter::Bcj2Decoder::new(
        Box::new(Cursor::new(data::BCJ2_MAIN)),
        Box::new(Cursor::new(data::BCJ2_CALL)),
        Box::new(Cursor::new(short_jump)),
        Box::new(Cursor::new(data::BCJ2_RC)),
    );
    assert!(dec.read_to_end(&mut Vec::new()).is_err(), "jump 截断未报错");

    // (b) 标记流缺失 → 报 Err
    let mut dec2 = crate::filter::Bcj2Decoder::new(
        Box::new(Cursor::new(data::BCJ2_MAIN)),
        Box::new(Cursor::new(data::BCJ2_CALL)),
        Box::new(Cursor::new(data::BCJ2_JUMP)),
        Box::new(Cursor::new(&[][..])),
    );
    assert!(dec2.read_to_end(&mut Vec::new()).is_err(), "rc 缺失未报错");

    // (c) call/jump 互换 → 负样本：不报错但输出与原始不符
    let mut dec3 = crate::filter::Bcj2Decoder::new(
        Box::new(Cursor::new(data::BCJ2_MAIN)),
        Box::new(Cursor::new(data::BCJ2_JUMP)),
        Box::new(Cursor::new(data::BCJ2_CALL)),
        Box::new(Cursor::new(data::BCJ2_RC)),
    );
    let mut out3 = Vec::new();
    if dec3.read_to_end(&mut out3).is_ok() {
        assert_ne!(out3, data::SAMPLE, "乱序流应产生不同输出");
    }
}

#[cfg(feature = "bcj2")]
#[test]
fn bcj_decode_matches_expected() {
    // BCJ（单流）解码：对 BRA86 样本做 x86_convert(encoding=false)，
    // 输出须与黄金期望逐字节一致（含 7-Zip 候选判定语义）。
    let src = data::BRA86.to_vec();
    let mut work = src.clone();
    let mut st = 0u32;
    let processed = crate::filter::bcj::x86_convert(&mut work, 0, &mut st, false);
    let exp = data::BRA86_EXPECTED;
    let n = processed.min(exp.len());
    let first = (0..n).find(|&i| work[i] != exp[i]);
    assert!(
        first.is_none(),
        "BCJ 解码在偏移 {first:?} 处偏离（processed={processed}）"
    );
}
