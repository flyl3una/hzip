//! T23 验收集成测试（roundtrip / mtime / multi-stream / cancel / 错误边界）。
//! 测试文件头部 allow 豁免（workspace unwrap_used/expect_used 例外）。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::Write;
use std::path::Path;
#[cfg(feature = "gz")]
use std::time::{Duration, Instant};

#[cfg(feature = "gz")]
use hzip_error::HzipError;
use hzip_format::EntryName;
use hzip_format::{
    ConflictPolicy, CreateOptions, EntryMeta, ExtractCtx, FormatHandler, OpenOptions, PasswordRef,
    QuotaLimits, SanitizeConfig, Selection, Sink, Source,
};
use hzip_progress::{CancellationToken, ProgressBus};

#[cfg(feature = "gz")]
use hzip_fmt_single::gz;

/// 当前启用格式的 handler 集（feature 门控）。
#[allow(clippy::vec_init_then_push)]
fn handlers() -> Vec<&'static dyn FormatHandler> {
    fn leak<H: FormatHandler + 'static>(h: H) -> &'static dyn FormatHandler {
        Box::leak(Box::new(h))
    }
    let mut v: Vec<&'static dyn FormatHandler> = Vec::new();
    #[cfg(feature = "gz")]
    v.push(leak(hzip_fmt_single::gz()));
    #[cfg(feature = "bz2")]
    v.push(leak(hzip_fmt_single::bz2()));
    #[cfg(feature = "xz")]
    v.push(leak(hzip_fmt_single::xz()));
    #[cfg(feature = "zst")]
    v.push(leak(hzip_fmt_single::zst()));
    v
}

/// 在临时目录里把 data 压缩为 fmt 产物，返回产物路径。
fn compress_to(
    out_dir: &Path,
    name: &str,
    data: &[u8],
    handler: &dyn FormatHandler,
    level: hzip_codec::level::Level,
    mtime: Option<time::OffsetDateTime>,
) -> std::path::PathBuf {
    let p = out_dir.join(name);
    let mut w = handler
        .create_writer(
            Sink::File(p.clone()),
            &CreateOptions {
                level,
                threads: 0,
                ..Default::default()
            },
        )
        .expect("create_writer");
    // 条目名 = 去掉格式后缀后的 basename（派生名测试独立覆盖）
    let entry_name = {
        let f = Path::new(name)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        f.rsplit_once('.').map(|(s, _)| s.to_owned()).unwrap_or(f)
    };
    let meta = EntryMeta {
        name: EntryName::new(entry_name),
        size_raw: Some(data.len() as u64),
        mtime,
        ..Default::default()
    };
    let mut sink = w
        .next_entry(&meta, PasswordRef::none())
        .expect("next_entry");
    sink.write_all(data).expect("write");
    sink.finish().expect("sink finish");
    w.finish().expect("writer finish");
    p
}

/// 读出条目数据字节（File 源；read_entry(0)）。
fn read_bytes(handler: &dyn FormatHandler, path: &Path) -> Vec<u8> {
    let r = handler
        .open_reader(Source::File(path.to_path_buf()), &OpenOptions::default())
        .expect("open_reader");
    let mut rr = r.read_entry(0, PasswordRef::none()).expect("read_entry");
    let mut buf = Vec::new();
    rr.read_to_end(&mut buf).expect("read_to_end");
    buf
}

/// 简易测试上下文（无密码/无配额限制）。
struct TestCtx {
    cancel: CancellationToken,
    bus: ProgressBus,
    sanitize: SanitizeConfig,
    quota: QuotaLimits,
}

struct NoPw;
impl hzip_format::PasswordSource for NoPw {
    fn password(&self, _e: Option<&EntryName>) -> Option<zeroize::Zeroizing<String>> {
        None
    }
}

impl TestCtx {
    fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            bus: ProgressBus::new(),
            sanitize: SanitizeConfig::default(),
            quota: QuotaLimits::default(),
        }
    }

    fn ctx(&self) -> ExtractCtx<'_> {
        static NOPW: NoPw = NoPw;
        ExtractCtx {
            cancel: &self.cancel,
            progress: &self.bus,
            passwords: &NOPW,
            conflicts: ConflictPolicy::Overwrite,
            sanitize: &self.sanitize,
            quota: &self.quota,
            best_effort: false,
            codepage: None,
        }
    }
}

// ===================== roundtrip 矩阵 =====================

fn samples() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("empty", Vec::new()),
        ("zero", vec![0u8; 4096]),
        ("random", {
            let mut s = 42u64;
            (0..8192)
                .map(|_| {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    s as u8
                })
                .collect()
        }),
        (
            "text",
            b"the quick brown fox jumps over the lazy dog\n".repeat(200),
        ),
    ]
}

fn all_levels() -> Vec<hzip_codec::level::Level> {
    vec![
        hzip_codec::level::Level::Fastest,
        hzip_codec::level::Level::Normal,
        hzip_codec::level::Level::Ultra,
    ]
}

#[test]
fn roundtrip_matrix_gz() {
    #[cfg(feature = "gz")]
    roundtrip_matrix_for(&hzip_fmt_single::gz());
}

#[cfg(feature = "bz2")]
#[test]
fn roundtrip_matrix_bz2() {
    roundtrip_matrix_for(&hzip_fmt_single::bz2());
}

#[cfg(feature = "xz")]
#[test]
fn roundtrip_matrix_xz() {
    roundtrip_matrix_for(&hzip_fmt_single::xz());
}

#[cfg(feature = "zst")]
#[test]
fn roundtrip_matrix_zst() {
    roundtrip_matrix_for(&hzip_fmt_single::zst());
}

fn roundtrip_matrix_for(h: &dyn FormatHandler) {
    let d = tempfile::tempdir().expect("tempdir");
    for (sname, data) in samples() {
        for level in all_levels() {
            let fname = format!("{sname}-{level:?}.{}", h.id());
            let p = compress_to(d.path(), &fname, &data, h, level, None);
            let got = read_bytes(h, &p);
            assert_eq!(got, data, "roundtrip {sname} @ {level:?} bytes 不一致");
        }
    }
}

// ===================== mtime roundtrip（gzip）=====================

#[cfg(feature = "gz")]
#[test]
fn gz_mtime_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let mt = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    let p = compress_to(
        d.path(),
        "m.gz",
        b"data",
        &hzip_fmt_single::gz(),
        hzip_codec::level::Level::Normal,
        Some(mt),
    );
    let r = gz()
        .open_reader(Source::File(p.clone()), &OpenOptions::default())
        .unwrap();
    let es = r.entries().unwrap();
    assert_eq!(es[0].mtime.unwrap().unix_timestamp(), 1_700_000_000);
    // 无 mtime（None）→ 头 mtime=0 → None
    let p2 = compress_to(
        d.path(),
        "n.gz",
        b"data",
        &hzip_fmt_single::gz(),
        hzip_codec::level::Level::Normal,
        None,
    );
    let r2 = gz()
        .open_reader(Source::File(p2.clone()), &OpenOptions::default())
        .unwrap();
    assert!(r2.entries().unwrap()[0].mtime.is_none());
}

// ===================== 名字推导 =====================

#[cfg(feature = "gz")]
#[test]
fn name_derivation() {
    let d = tempfile::tempdir().unwrap();
    // FNAME 优先：写端 filename 进入头
    let meta = EntryMeta {
        name: EntryName::new("from-fname.txt"),
        size_raw: Some(4),
        ..Default::default()
    };
    let p = d.path().join("orig-name.gz");
    {
        let mut w = gz()
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
        sink.write_all(b"1234").unwrap();
        sink.finish().unwrap();
        w.finish().unwrap();
    }
    // 读回名来自 FNAME
    let es = gz()
        .open_reader(Source::File(p.clone()), &Default::default())
        .unwrap()
        .entries()
        .unwrap();
    assert_eq!(es[0].name.as_str(), "from-fname.txt");

    // 无 FNAME（文件名 f.txt.gz，FNAME 省略）→ 剥一层 → f.txt
    let p2 = d.path().join("f.txt.gz");
    let data = {
        let mut e = flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::default());
        e.write_all(b"1234").unwrap();
        e.finish().unwrap()
    };
    std::fs::write(&p2, data).unwrap();
    let es2 = gz()
        .open_reader(Source::File(p2.clone()), &Default::default())
        .unwrap()
        .entries()
        .unwrap();
    assert_eq!(es2[0].name.as_str(), "f.txt");

    // --entry-name 覆盖（stdin 语义，OpenOptions.entry_name）
    let r3 = gz()
        .open_reader(
            Source::File(p2.clone()),
            &OpenOptions {
                entry_name: Some(EntryName::new("override.txt")),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(r3.entries().unwrap()[0].name.as_str(), "override.txt");
}

// ===================== 单条目 / 双次 next_entry =====================

#[test]
fn second_entry_unsupported_and_append_rejected() {
    let d = tempfile::tempdir().unwrap();
    for h in handlers() {
        // next_entry 二次调用
        let p = d.path().join(format!("a.{}", h.id()));
        let mut w = h
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        let meta = EntryMeta {
            name: EntryName::new("a"),
            size_raw: Some(1),
            ..Default::default()
        };
        let sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
        let mut sink = sink;
        sink.write_all(b"x").unwrap();
        sink.finish().unwrap();
        let err = w
            .next_entry(&meta, PasswordRef::none())
            .err()
            .expect("二次 next_entry 应报错");
        assert_eq!(err.code(), 5001);
        let _ = w.finish();

        // append=true → Unsupported
        let err2 = h
            .create_writer(
                Sink::File(d.path().join(format!("b.{}", h.id()))),
                &CreateOptions {
                    append: true,
                    ..Default::default()
                },
            )
            .err()
            .expect("append 应拒绝");
        assert_eq!(err2.code(), 5001);
    }
}

// ===================== extract / test =====================
#[test]
fn extract_and_test_full() {
    let d = tempfile::tempdir().unwrap();
    let payload = b"extract me\n".repeat(50);
    for h in handlers() {
        let fname = format!("out.{}", h.id());
        let p = compress_to(
            d.path(),
            &fname,
            &payload,
            h,
            hzip_codec::level::Level::Normal,
            None,
        );
        let r = h
            .open_reader(Source::File(p.clone()), &OpenOptions::default())
            .unwrap();
        let dest = d.path().join(format!("x-{}", h.id()));
        std::fs::create_dir_all(&dest).unwrap();
        let ctx = TestCtx::new();
        let rep = r.extract(&Selection::All, &ctx.ctx(), &dest).unwrap();
        assert_eq!(rep.entries_done, 1);
        assert_eq!(rep.bytes_written, payload.len() as u64);
        // 目标名 = derive（out.ext → out）
        let want_name = "out";
        let disk = std::fs::read(dest.join(want_name)).unwrap();
        assert_eq!(disk, payload);

        // test()
        let r2 = h
            .open_reader(Source::File(p.clone()), &OpenOptions::default())
            .unwrap();
        let rep2 = r2.test(&ctx.ctx()).unwrap();
        assert_eq!(rep2.bytes_written, payload.len() as u64);
    }
}

// ===================== 截断 / 位翻转 不 panic（TC-CONF-008 展开）=====================

#[test]
fn truncated_and_bitflip_no_panic() {
    let d = tempfile::tempdir().unwrap();
    let payload = b"integrity-test data. ".repeat(3000);
    for h in handlers() {
        let good_path = compress_to(
            d.path(),
            &format!("good.{}", h.id()),
            &payload,
            h,
            hzip_codec::level::Level::Fastest,
            None,
        );
        let good = std::fs::read(&good_path).unwrap();
        // 截断 10% / 50% / 末字节
        for (i, cut) in [good.len() * 9 / 10, good.len() / 2, good.len() - 1]
            .iter()
            .enumerate()
        {
            let t = &good[..*cut];
            let p = d.path().join(format!("trunc-{}-{i}.{}", h.id(), h.id()));
            std::fs::write(&p, t).unwrap();
            // open_reader + entries 不应 panic；test/read 可 Err 或 Ok（截断端到端由 read 校验）
            let rr = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Ok(rr) = h.open_reader(Source::File(p.clone()), &OpenOptions::default()) {
                    let ctx = TestCtx::new();
                    let _ = rr.test(&ctx.ctx());
                }
            }));
            assert!(rr.is_ok(), "截断样本不 panic");
        }
        // 位翻转（固定种子）
        let mut flipped = good.clone();
        {
            let mut s = 0xC0FFEEu64;
            for _ in 0..20 {
                if flipped.is_empty() {
                    break;
                }
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
                let pos = (s as usize) % flipped.len();
                flipped[pos] ^= 1 << ((s >> 32) as u8 % 8);
            }
        }
        let pf = d.path().join(format!("flip.{}", h.id()));
        std::fs::write(&pf, &flipped).unwrap();
        let rr = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Ok(rr) = h.open_reader(Source::File(pf.clone()), &OpenOptions::default()) {
                let ctx = TestCtx::new();
                let _ = rr.test(&ctx.ctx());
            }
        }));
        assert!(rr.is_ok(), "位翻转样本不 panic");
    }
}

// ===================== 取消（小规模，验证半文件清理与 Cancelled）=====================

#[cfg(feature = "gz")]
#[test]
fn cancel_mid_extract_cleans_partial() {
    // 全零高压缩样本：解压量 ~16 MiB，监控达 ~8 MiB 时取消
    let big = vec![0u8; 16 * 1024 * 1024];
    let d = tempfile::tempdir().unwrap();
    let p = compress_to(
        d.path(),
        "big.gz",
        &big,
        &hzip_fmt_single::gz(),
        hzip_codec::level::Level::Fastest,
        None,
    );
    let w = gz()
        .open_reader(Source::File(p.clone()), &OpenOptions::default())
        .unwrap();
    let dest = d.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let cancel = CancellationToken::new();
    let bus = ProgressBus::new();
    static NOPW: NoPw = NoPw;
    let cancel2 = cancel.clone();
    let mon_dest = dest.clone();
    let sub = std::thread::spawn(move || {
        let started = Instant::now();
        loop {
            let size = std::fs::metadata(mon_dest.join("big"))
                .map(|m| m.len())
                .unwrap_or(0);
            if size >= 8 * 1024 * 1024 || started.elapsed() > Duration::from_secs(30) {
                cancel2.cancel();
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let ctx = ExtractCtx {
        cancel: &cancel,
        progress: &bus,
        passwords: &NOPW,
        conflicts: ConflictPolicy::Overwrite,
        sanitize: &SanitizeConfig::default(),
        quota: &QuotaLimits::default(),
        best_effort: false,
        codepage: None,
    };
    let result = w.extract(&Selection::All, &ctx, &dest);
    sub.join().unwrap();
    // 取消 → Err Cancelled（或触发取消），且半写文件不存在（target 应被清理）
    assert!(result.is_err(), "取消后 extract 应 Err");
    assert!(
        matches!(result.err(), Some(HzipError::Cancelled)),
        "取消后应为 Cancelled"
    );
    assert!(
        !dest.join("big").exists(),
        "半写文件应被清理（TC-CONF-006）"
    );
}

// ===================== 追加/固实等 caps 负断言（T23 §3.4） =====================

#[test]
fn caps_negative_assertions() {
    let d = tempfile::tempdir().unwrap();
    for h in handlers() {
        let c = h.caps();
        assert!(c.read && c.write);
        assert!(!c.append && !c.edit && !c.split && !c.solid && !c.comment);
        assert_eq!(c.encryption, hzip_format::EncryptionCaps::None);
        // open_editor → Unsupported 5001
        let err = h
            .open_editor(Source::File(d.path().join("x")), &Default::default())
            .err()
            .expect("editor 应拒绝");
        assert_eq!(err.code(), 5001);
    }
}

// ===================== entries() 不读数据（大样本秒开抽查） =====================

#[cfg(feature = "gz")]
#[test]
fn entries_no_data_read_smoke() {
    // 256 MiB 全零压缩 → 产物小；entries 只读头，应 < 1s
    let big = vec![0u8; 256 * 1024 * 1024];
    let d = tempfile::tempdir().unwrap();
    let p = compress_to(
        d.path(),
        "huge.gz",
        &big,
        &hzip_fmt_single::gz(),
        hzip_codec::level::Level::Fastest,
        None,
    );
    let t0 = Instant::now();
    let r = gz()
        .open_reader(Source::File(p.clone()), &OpenOptions::default())
        .unwrap();
    let es = r.entries().unwrap();
    let t1 = t0.elapsed();
    assert_eq!(es.len(), 1);
    assert!(t1 < Duration::from_secs(1), "entries 耗时 {t1:?} 超预算");
}

// ===================== multi-stream 串联（02 §8.5 / T23 §4.5） =====================

/// 解出 read_entry(0) 全量字节（File 源）。
fn read_entry_bytes(h: &dyn FormatHandler, path: &Path) -> Vec<u8> {
    let r = h
        .open_reader(Source::File(path.to_path_buf()), &OpenOptions::default())
        .expect("open_reader");
    let mut rr = r.read_entry(0, PasswordRef::none()).expect("read_entry");
    let mut buf = Vec::new();
    rr.read_to_end(&mut buf).expect("read_to_end");
    buf
}

#[cfg(feature = "gz")]
#[test]
fn multi_member_gzip_concat_and_bad_tail() {
    let d = tempfile::tempdir().unwrap();
    let a = b"member-A-AAAA".repeat(50);
    let b = b"member-B-BBBB".repeat(60);
    let c = b"member-C".repeat(70);
    let ga = {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&a).unwrap();
        e.finish().unwrap()
    };
    let gb = {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&b).unwrap();
        e.finish().unwrap()
    };
    let gc = {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&c).unwrap();
        e.finish().unwrap()
    };
    let mut concat = ga.clone();
    concat.extend_from_slice(&gb);
    concat.extend_from_slice(&gc);
    let p = d.path().join("cat.gz");
    std::fs::write(&p, &concat).unwrap();
    let got = read_entry_bytes(&hzip_fmt_single::gz(), &p);
    let mut want = a.clone();
    want.extend_from_slice(&b);
    want.extend_from_slice(&c);
    assert_eq!(got, want, "3 成员 gzip 拼接读出一致");

    // 第二成员尾部损坏（破坏 member2 的 ISIZE 字节）→ 读取必须 Err（不静默 EOF）
    let mut bad = ga.clone();
    bad.extend_from_slice(&gb);
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    let p2 = d.path().join("bad.gz");
    std::fs::write(&p2, &bad).unwrap();
    // ① read_to_end 返回 Err（不 panic、不静默截断）
    let rr = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let r = hzip_fmt_single::gz()
            .open_reader(Source::File(p2.clone()), &OpenOptions::default())
            .expect("open");
        let mut stream = r.read_entry(0, PasswordRef::none()).expect("read_entry");
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf); // Err 是期望结果
    }));
    assert!(rr.is_ok(), "坏尾样本 read_to_end 不 panic");
    // ② test() 全量校验 → Err（成员尾校验失败，不静默 EOF）
    let r2 = hzip_fmt_single::gz()
        .open_reader(Source::File(p2.clone()), &OpenOptions::default())
        .unwrap();
    let ctx = TestCtx::new();
    let trep = r2.test(&ctx.ctx());
    assert!(trep.is_err(), "坏第二成员尾 test() 应 Err（不静默 EOF）");
}

#[cfg(feature = "bz2")]
#[test]
fn multi_member_bz2_concat() {
    let d = tempfile::tempdir().unwrap();
    let a = b"AAA".repeat(100);
    let b = b"BBB".repeat(80);
    let b1 = {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(3));
        e.write_all(&a).unwrap();
        e.finish().unwrap()
    };
    let b2 = {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(3));
        e.write_all(&b).unwrap();
        e.finish().unwrap()
    };
    let mut cat = b1;
    cat.extend_from_slice(&b2);
    let p = d.path().join("cat.bz2");
    std::fs::write(&p, &cat).unwrap();
    let got = read_entry_bytes(&hzip_fmt_single::bz2(), &p);
    let mut want = a;
    want.extend_from_slice(&b);
    assert_eq!(got, want, "2 成员 bz2 拼接读出一致");
}

#[cfg(feature = "xz")]
#[test]
fn multi_member_xz_concat() {
    let d = tempfile::tempdir().unwrap();
    let a = b"XZXZXZ".repeat(90);
    let b = b"Q".repeat(70);
    let x1 = {
        let mut e = xz2::write::XzEncoder::new(Vec::new(), 6);
        e.write_all(&a).unwrap();
        e.finish().unwrap()
    };
    let x2 = {
        let mut e = xz2::write::XzEncoder::new(Vec::new(), 6);
        e.write_all(&b).unwrap();
        e.finish().unwrap()
    };
    let mut cat = x1;
    cat.extend_from_slice(&x2);
    let p = d.path().join("cat.xz");
    std::fs::write(&p, &cat).unwrap();
    let got = read_entry_bytes(&hzip_fmt_single::xz(), &p);
    let mut want = a;
    want.extend_from_slice(&b);
    assert_eq!(got, want, "2 流 xz 拼接读出一致");
}

#[cfg(feature = "zst")]
#[test]
fn multi_member_zstd_with_skippable() {
    let d = tempfile::tempdir().unwrap();
    let a = b"frame-ONE".repeat(120);
    let b = b"frame-TWO".repeat(90);
    let z1 = {
        let mut e = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        e.write_all(&a).unwrap();
        e.finish().unwrap()
    };
    let z2 = {
        let mut e = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        e.write_all(&b).unwrap();
        e.finish().unwrap()
    };
    // 中间插一个 skippable 帧（zstd CLI 语义：解码器跳过）
    let mut skip = Vec::new();
    skip.extend_from_slice(&0x184D2A50u32.to_le_bytes());
    skip.extend_from_slice(&3u32.to_le_bytes());
    skip.extend_from_slice(b"xyz");
    let mut cat = z1;
    cat.extend_from_slice(&skip);
    cat.extend_from_slice(&z2);
    let p = d.path().join("cat.zst");
    std::fs::write(&p, &cat).unwrap();
    let got = read_entry_bytes(&hzip_fmt_single::zst(), &p);
    let mut want = a;
    want.extend_from_slice(&b);
    assert_eq!(got, want, "帧间 skippable 跳过 + 帧拼接读出正确");
}

// ===================== zstd mtime 可选（hzip 私有帧，02 §8.5 / T23 §4.4） =====================

#[cfg(feature = "zst")]
#[test]
fn zst_mtime_optional() {
    let d = tempfile::tempdir().unwrap();
    let data = b"zstd mtime frame test".repeat(40);
    // 标准帧（无私有帧）→ mtime None（外部工具产物兼容）
    let frame = {
        let mut e = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        e.write_all(&data).unwrap();
        e.finish().unwrap()
    };
    let p1 = d.path().join("plain.zst");
    std::fs::write(&p1, &frame).unwrap();
    let r1 = hzip_fmt_single::zst()
        .open_reader(Source::File(p1), &OpenOptions::default())
        .unwrap();
    let es1 = r1.entries().unwrap();
    assert_eq!(es1.len(), 1);
    assert!(
        es1[0].mtime.is_none(),
        "标准 zstd 帧（无私有 skippable）mtime=None"
    );

    // 前置 hzip 私有 mtime skippable 帧 → mtime 恢复
    let millis: u64 = 1_700_000_000_000;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"hzip-mtime");
    payload.extend_from_slice(&1u32.to_le_bytes()); // tag=1
    payload.extend_from_slice(&millis.to_le_bytes());
    let mut pfx = Vec::new();
    pfx.extend_from_slice(&0x184D2A50u32.to_le_bytes());
    pfx.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    pfx.extend_from_slice(&payload);
    let mut with_meta = pfx;
    with_meta.extend_from_slice(&frame);
    let p2 = d.path().join("meta.zst");
    std::fs::write(&p2, &with_meta).unwrap();

    let r2 = hzip_fmt_single::zst()
        .open_reader(Source::File(p2.clone()), &OpenOptions::default())
        .expect("zstd 私有帧 open_reader 应成功");
    let es2 = r2.entries().unwrap();
    let mt = es2[0].mtime.expect("私有帧应恢复 mtime");
    assert_eq!(mt.unix_timestamp(), 1_700_000_000);

    // 数据仍可解（读端跳过前置 skippable）
    let got = read_entry_bytes(&hzip_fmt_single::zst(), &p2);
    assert_eq!(got, data, "含私有帧仍能完整解出数据");
}

// ===================== gzip FEXTRA/FNAME 组合头解析 =====================

#[cfg(feature = "gz")]
#[test]
fn gz_header_extra_fields_tolerated() {
    // 用 flate2 GzBuilder 造含 FNAME + comment 的标准 gzip；多一个普通成员前置
    // 不影响——entries() 只读第一个头。重点：FNAME 仍被解析优先。
    let d = tempfile::tempdir().unwrap();
    let data = b"extra field test".repeat(20);
    let mut b = flate2::GzBuilder::new();
    b = b.filename("inside.bin").comment("hzip test comment");
    let mut e = b.write(Vec::new(), flate2::Compression::default());
    e.write_all(&data).unwrap();
    let bytes = e.finish().unwrap();
    let p = d.path().join("x.gz");
    std::fs::write(&p, &bytes).unwrap();
    let es = gz()
        .open_reader(Source::File(p), &OpenOptions::default())
        .unwrap()
        .entries()
        .unwrap();
    assert_eq!(es[0].name.as_str(), "inside.bin");
}

// ===================== 成员尾损坏 → Err（不静默 EOF，各格式）=====================

/// 每种格式的末字节损坏必须令 `test()` 报错（容器尾校验，02 §8.5 红线）。
#[test]
fn tail_corruption_rejected_all_formats() {
    let d = tempfile::tempdir().unwrap();
    let payload = b"tail-corruption-".repeat(2000);
    for h in handlers() {
        let good_path = compress_to(
            d.path(),
            &format!("tail.{}", h.id()),
            &payload,
            h,
            hzip_codec::level::Level::Fastest,
            None,
        );
        let mut bad = std::fs::read(&good_path).unwrap();
        assert!(bad.len() > 2, "产物应有足够长度");
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        let p = d.path().join(format!("tail-bad.{}", h.id()));
        std::fs::write(&p, &bad).unwrap();
        let r = h
            .open_reader(Source::File(p), &OpenOptions::default())
            .expect("open_reader");
        let ctx = TestCtx::new();
        let rep = r.test(&ctx.ctx());
        assert!(rep.is_err(), "{} 成员尾损坏应 Err（不静默 EOF）", h.id());
    }
}
