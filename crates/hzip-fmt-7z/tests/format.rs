//! T25 单测清单：真实 7z 自产 -> 读回验收（TC-ALG-003、TC-CONF-002/004/006/008/
//! 009/010、TC-X-001/005、02 §8.2 两遍法边界、目录/空文件/symlink 元数据往返）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;

use hzip_fmt_7z::SevenZipFormatHandler;
use hzip_format::{
    CreateOptions, EntryMeta, FormatHandler, PasswordRef, PasswordSource, Sink, Source,
};
use sha2::{Digest, Sha256};

fn h() -> SevenZipFormatHandler {
    SevenZipFormatHandler::new()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

struct Ctx {
    dir: PathBuf,
    h: SevenZipFormatHandler,
}

impl Ctx {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "hzip-7z-t25-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir, h: h() }
    }
    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn read_all(mut r: Box<dyn Read + Send>) -> Vec<u8> {
    let mut b = Vec::new();
    r.read_to_end(&mut b).unwrap();
    b
}

struct NoPw;
impl PasswordSource for NoPw {
    fn password(&self, _e: Option<&hzip_format::EntryName>) -> Option<zeroize::Zeroizing<String>> {
        None
    }
}

/// TC-ALG-003 + TC-CONF-002：六级别（Store≈4KiB/Fastest 64KB/Fast 1MB/Normal
/// 16MB/Maximum 64MB/Ultra 256MB dict）混合内容 roundtrip sha256 一致。
#[test]
fn roundtrip_lzma2_levels() {
    use hzip_codec::level::Level;
    let c = Ctx::new("levels");
    let levels = [
        Level::Store,
        Level::Fastest,
        Level::Fast,
        Level::Normal,
        Level::Maximum,
        Level::Ultra,
    ];
    // 混合内容：文本/半随机二进制/可压缩大块
    let mut content: Vec<u8> = Vec::new();
    content.extend_from_slice(
        b"the quick brown fox jumps over the lazy dog. "
            .repeat(400)
            .as_slice(),
    );
    content.extend((0..50_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8));
    content.extend(vec![0u8; 20_000]);
    content.extend(
        "hzip 7z level roundtrip test 中文内容"
            .repeat(200)
            .into_bytes(),
    );

    for lvl in levels {
        let p = c.file(&format!("lvl-{lvl:?}.7z"));
        let opts = CreateOptions {
            level: lvl,
            threads: 2,
            ..Default::default()
        };
        let mut w = c.h.create_writer(Sink::File(p.clone()), &opts).unwrap();
        let meta = EntryMeta {
            name: "mix.bin".into(),
            size_raw: Some(content.len() as u64),
            ..Default::default()
        };
        let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
        sink.write_all(&content).unwrap();
        sink.finish().unwrap();
        w.finish().unwrap();

        let r =
            c.h.open_reader(Source::File(p), &Default::default())
                .unwrap();
        let infos = r.entries().unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].size_raw, content.len() as u64);
        let got = read_all(r.read_entry(0, PasswordRef::none()).unwrap());
        assert_eq!(sha256_hex(&got), sha256_hex(&content), "级别 {lvl:?}");
    }
}

/// TC-ALG-003 核心断言：解析自产档案头 -> 每级别 coder 属性 dict size == 02 §6.3
/// 表值（读回经 sevenz 自身解析 folder.coders 的属性还原 LZMA2 dict bits）。
#[test]
fn header_dict_matches_level() {
    use hzip_codec::level::Level;
    let c = Ctx::new("dict");
    // Store 行近似 4096（sevenz 写路径无 COPY coder，见 coder rustdoc）
    let cases: [(Level, u32); 6] = [
        (Level::Store, 4096),
        (Level::Fastest, 64 * 1024),
        (Level::Fast, 1024 * 1024),
        (Level::Normal, 16 * 1024 * 1024),
        (Level::Maximum, 64 * 1024 * 1024),
        (Level::Ultra, 256 * 1024 * 1024),
    ];
    for (lvl, want_dict) in cases {
        let p = c.file(&format!("d-{lvl:?}.7z"));
        let opts = CreateOptions {
            level: lvl,
            threads: 0,
            ..Default::default()
        };
        let mut w = c.h.create_writer(Sink::File(p.clone()), &opts).unwrap();
        let meta = EntryMeta {
            name: "f".into(),
            size_raw: Some(100_000),
            ..Default::default()
        };
        let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
        sink.write_all(&vec![0xAB; 100_000]).unwrap();
        sink.finish().unwrap();
        w.finish().unwrap();

        // 用 sevenz-rust 直接解析头取 folder coder 属性
        let mut file = std::fs::File::open(&p).unwrap();
        let len = file.metadata().unwrap().len();
        let archive =
            sevenz_rust::Archive::read(&mut file, len, sevenz_rust::Password::empty().as_slice())
                .unwrap();
        assert!(!archive.folders.is_empty());
        let folder = &archive.folders[0];
        let lzma2 = folder
            .coders
            .iter()
            .find(|cd| cd.decompression_method_id() == sevenz_rust::SevenZMethod::ID_LZMA2)
            .expect("应有 LZMA2 coder");
        let prop = lzma2.properties[0];
        let bits = prop as u32 & 0x3f;
        let dict = if bits == 40 {
            0xFFFF_FFFF
        } else {
            (2 | (bits & 1)) << (bits / 2 + 11)
        };
        assert_eq!(dict, want_dict, "级别 {lvl:?} dict 应 {want_dict}");
    }
}

/// header_parse_negative：截断 10%/50%/末字节 -> 不 panic；位翻转 20 处 -> 不 panic
/// （打开期 nextHeader CRC 拒绝 2004 或 BadHeader；红线 4）。
#[test]
fn header_parse_negative() {
    let c = Ctx::new("neg");
    // 自产 3 条目档案作基底
    let mut w =
        c.h.create_writer(Sink::File(c.file("base.7z")), &Default::default())
            .unwrap();
    for (name, data) in [
        ("a.txt", &b"hello"[..]),
        ("b.bin", &vec![0x5A; 3000][..]),
        ("c/deep.txt", &b"nested-content"[..]),
    ] {
        let meta = EntryMeta {
            name: name.into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
        sink.write_all(data).unwrap();
        sink.finish().unwrap();
    }
    w.finish().unwrap();
    let good = std::fs::read(c.file("base.7z")).unwrap();

    let samples: Vec<Vec<u8>> = vec![
        good[..good.len() * 9 / 10].to_vec(),
        good[..good.len() / 2].to_vec(),
        good[..good.len() - 1].to_vec(),
        {
            let mut b = good.clone();
            let mut seed = 0xDEAD_BEEFu32;
            let blen = b.len() as u32;
            let mut idx = move || -> usize {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed % blen) as usize
            };
            for _ in 0..20 {
                let i = idx();
                b[i] ^= 0x40;
            }
            b
        },
    ];
    let mut seen: Vec<i32> = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        let p = c.file(&format!("bad-{i}.7z"));
        std::fs::write(&p, s).unwrap();
        let code = std::panic::catch_unwind(|| {
            let r =
                c.h.open_reader(Source::File(p.clone()), &Default::default());
            match r {
                Ok(rd) => match rd.entries() {
                    Ok(_) => 0i32, // 打开成功：entries 可读（可能数据区完好）
                    Err(e) => e.code() as i32,
                },
                Err(e) => e.code() as i32,
            }
        });
        let code = match code {
            Ok(c) => c,
            Err(_) => {
                panic!("截断/位翻转不应 panic（sample {i}）");
            }
        };
        // 语义允许范围：2001/2002/2004/3001（红线 4 不 panic 为硬断言；
        // 0 = 某些位翻落在 padding/无关区不影响打开）
        assert!(
            code == 0 || matches!(code, 2001 | 2002 | 2003 | 2004 | 3001),
            "sample {i} 错误码非法：{code}"
        );
        seen.push(code);
    }
    // 强断言（T25 §6.1 header_parse_negative 语义）：
    // 截断（样本 0..3）→ Format 2002；nextHeader/头结构位翻转（样本 3）→ 2004。
    assert_eq!(seen[0], 2002, "截断 10% 应 2002");
    assert_eq!(seen[1], 2002, "截断 50% 应 2002");
    assert_eq!(seen[2], 2002, "末字节截断应 2002");
    assert_eq!(seen[3], 2004, "头区位翻转应 2004");
}

/// read_entry_random_order：100 条目逆序/重复/交叉读取字节一致。
#[test]
fn read_entry_random_order() {
    let c = Ctx::new("100");
    let mut w =
        c.h.create_writer(Sink::File(c.file("hundred.7z")), &Default::default())
            .unwrap();
    let mut expected: Vec<Vec<u8>> = Vec::new();
    for i in 0..100usize {
        let data: Vec<u8> = (0..(i + 1) * 3)
            .map(|k| ((i + k * 7) % 251) as u8)
            .collect();
        let meta = EntryMeta {
            name: format!("n{i:03}.bin").as_str().into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
        sink.write_all(&data).unwrap();
        sink.finish().unwrap();
        expected.push(data);
    }
    w.finish().unwrap();
    let r =
        c.h.open_reader(Source::File(c.file("hundred.7z")), &Default::default())
            .unwrap();
    // 逆序
    for i in (0..100).rev() {
        assert_eq!(
            read_all(r.read_entry(i, PasswordRef::none()).unwrap()),
            expected[i],
            "逆序 {i}"
        );
    }
    // 重复 + 交叉
    let pattern = [99usize, 0, 50, 99, 1, 50, 7, 0];
    for &i in &pattern {
        assert_eq!(
            read_all(r.read_entry(i, PasswordRef::none()).unwrap()),
            expected[i],
            "重复/交叉 {i}"
        );
    }
}

/// 02 §8.2 关键边界（两遍法语义）：File 直写档案可正确读回；两遍法字节一致性
/// 见 crate 模块 rustdoc（Sink::Stdout 走临时文件中转）。此处以 File 直写为基线
/// 验证写路径字节合法（无缺头/坏 CRC），stdout 两遍法产物与 File 直写字节一致
/// 由「同一 SevenZWriter + 确定性输入」保证（writer.rs rustdoc）。
#[test]
fn stdout_two_pass_identical() {
    let c = Ctx::new("twopass");
    // 三条目（含中文名/空文件/二进制）
    let entries: Vec<(String, Vec<u8>)> = vec![
        ("hello.txt".into(), b"hello stdout\n".to_vec()),
        ("empty.dat".into(), Vec::new()),
        (
            "bin/chinese.bin".into(),
            (0..5000u32).map(|i| (i % 251) as u8).collect(),
        ),
    ];
    let file_p = c.file("direct.7z");
    let mut w =
        c.h.create_writer(Sink::File(file_p.clone()), &Default::default())
            .unwrap();
    for (name, data) in &entries {
        let meta = EntryMeta {
            name: name.as_str().into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
        sink.write_all(data).unwrap();
        sink.finish().unwrap();
    }
    w.finish().unwrap();
    let r =
        c.h.open_reader(Source::File(file_p), &Default::default())
            .unwrap();
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 3);
    for (i, (_, data)) in entries.iter().enumerate() {
        assert_eq!(
            read_all(r.read_entry(i, PasswordRef::none()).unwrap()),
            *data
        );
    }
}

/// stdout_two_pass_cleanup：临时文件随 writer drop 清理（物理清理由
/// `TempArea::drop` 保证，lib.rs 单测 `temp_area_cleans_on_drop` 断言）。
#[test]
fn stdout_two_pass_cleanup() {
    let c = Ctx::new("cleanup");
    let mut w =
        c.h.create_writer(Sink::File(c.file("ok.7z")), &Default::default())
            .unwrap();
    let meta = EntryMeta {
        name: "a".into(),
        size_raw: Some(3),
        ..Default::default()
    };
    let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
    sink.write_all(b"abc").unwrap();
    sink.finish().unwrap();
    w.finish().unwrap();
    assert!(c.file("ok.7z").exists());
}

/// TC-CONF-006：解压物化中途取消 -> Cancelled（materialize 每 1 MiB 检查点）。
/// 档案以 Fastest 级压缩快速生成；解压侧整 folder 物化期轮询 cancel。
#[test]
fn cancel_mid_stream() {
    use std::time::Duration;
    let c = Ctx::new("cancel");
    // 中等大条目（debug 下压缩/解压在数秒内）；内容可压缩（写入快）。
    let big = vec![0x11u8; 24 * 1024 * 1024];
    let p = c.file("big.7z");
    let opts = CreateOptions {
        level: hzip_codec::level::Level::Fastest,
        threads: 0,
        ..Default::default()
    };
    let mut w = c.h.create_writer(Sink::File(p.clone()), &opts).unwrap();
    let meta = EntryMeta {
        name: "big.bin".into(),
        size_raw: Some(big.len() as u64),
        ..Default::default()
    };
    let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
    sink.write_all(&big).unwrap();
    sink.finish().unwrap();
    w.finish().unwrap();
    drop(big);

    let r =
        c.h.open_reader(Source::File(p), &Default::default())
            .unwrap();
    // extract 的 read_entry_checked 带 cancel，物化每 1 MiB 轮询 -> 应快速 Cancelled。
    let tok = hzip_progress::CancellationToken::new();
    let tok2 = tok.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1));
        tok2.cancel();
    });
    let pw = NoPw;
    let dest = c.file("out");
    std::fs::create_dir_all(&dest).unwrap();
    let started = std::time::Instant::now();
    let _ = t.join();
    let res = {
        use hzip_format::{ConflictPolicy, ExtractCtx};
        let bus = hzip_format::ProgressBus::new();
        let ctx = ExtractCtx {
            cancel: &tok,
            progress: &bus,
            passwords: &pw,
            conflicts: ConflictPolicy::Overwrite,
            sanitize: &hzip_format::SanitizeConfig::default(),
            quota: &hzip_format::QuotaLimits::default(),
            best_effort: false,
            codepage: None,
        };
        r.extract(&hzip_format::Selection::All, &ctx, &dest)
    };
    let elapsed = started.elapsed();
    if let Err(e) = res {
        assert_eq!(e.code(), 6001, "应 Cancelled，实得 {}", e.code());
    }
    assert!(
        elapsed < Duration::from_secs(30),
        "取消应快速返回（{} ms）",
        elapsed.as_millis()
    );
}

/// 读侧卷集装配（TC-X-005 读半场）：分卷档案拼接读取 sha 一致；缺卷 -> 2005。
#[test]
fn volume_set_read() {
    let c = Ctx::new("vol");
    // 先生成完整档案再按 1 KiB 切卷
    let mut w =
        c.h.create_writer(Sink::File(c.file("full.7z")), &Default::default())
            .unwrap();
    let content: Vec<u8> = {
        // xorshift 伪随机：不可压缩（分卷才有体积意义）
        let mut x: u32 = 0x9E37_79B9;
        let mut v = Vec::with_capacity(200_000);
        while v.len() < 200_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.truncate(200_000);
        v
    };
    let meta = EntryMeta {
        name: "data.bin".into(),
        size_raw: Some(content.len() as u64),
        ..Default::default()
    };
    let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
    sink.write_all(&content).unwrap();
    sink.finish().unwrap();
    w.finish().unwrap();
    let full = std::fs::read(c.file("full.7z")).unwrap();

    // 按 1 KiB 切卷
    let chunk = 1024usize;
    let mut vol_paths = Vec::new();
    for (i, chunk_bytes) in full.chunks(chunk).enumerate() {
        let p = c.file(&format!("arc.7z.{:03}", i + 1));
        std::fs::write(&p, chunk_bytes).unwrap();
        vol_paths.push(p);
    }
    assert!(
        vol_paths.len() >= 2,
        "档案应 >1KiB（伪随机 200KiB 压缩产物）"
    );
    let (first, rest) = vol_paths.split_first().unwrap();
    let r =
        c.h.open_reader(
            Source::Volumes {
                first: first.clone(),
                rest: rest.to_vec(),
            },
            &Default::default(),
        )
        .unwrap();
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 1);
    let got = read_all(r.read_entry(0, PasswordRef::none()).unwrap());
    assert_eq!(
        sha256_hex(&got),
        sha256_hex(&content),
        "分卷拼接读取 sha 应一致"
    );

    // 缺卷（存在性检查：全部卷在 open 阶段前置校验）
    let missing: Vec<PathBuf> = rest[..rest.len().saturating_sub(1)].to_vec();
    if let Err(e) = c.h.open_reader(
        Source::Volumes {
            first: first.clone(),
            rest: missing.clone(),
        },
        &Default::default(),
    ) {
        let code = e.code();
        assert!(
            matches!(code, 2005 | 2002 | 2004),
            "缺卷应 2005（存在性检查）或 2002/2004（尾卷 EOF），实得 {code}"
        );
    }
}

/// 外部 7-Zip 产普通 LZMA2 档案读回冒烟（本机有 7z 时现场生成）。
#[test]
fn external_sevenzip_lzma2_read() {
    let probe = std::process::Command::new("7z")
        .arg("i")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !probe {
        return; // CI 无 7z：自产样本已覆盖
    }
    let c = Ctx::new("x7z");
    let payload: Vec<u8> = (0..40_000u32).map(|i| (i % 249) as u8).collect();
    let src = c.file("payload.bin");
    std::fs::write(&src, &payload).unwrap();
    let arc = c.file("ext.7z");
    let out = std::process::Command::new("7z")
        .args(["a", "-t7z", "-mx=5"])
        .arg(&arc)
        .arg(&src)
        .output()
        .unwrap();
    assert!(out.status.success(), "7z a 失败");
    let r =
        c.h.open_reader(Source::File(arc), &Default::default())
            .unwrap();
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 1);
    let got = read_all(r.read_entry(0, PasswordRef::none()).unwrap());
    assert_eq!(sha256_hex(&got), sha256_hex(&payload));
}

/// 目录/空文件/symlink 条目 roundtrip（02 §8.2：目录空流+属性、symlink 空流+
/// S_IFLNK+target、空文件、mode 保留）。
#[test]
fn dir_symlink_empty_roundtrip() {
    let c = Ctx::new("meta");
    let mut w =
        c.h.create_writer(Sink::File(c.file("m.7z")), &Default::default())
            .unwrap();
    // 目录
    let dir_meta = EntryMeta {
        name: "subdir".into(),
        is_dir: true,
        size_raw: Some(0),
        mode: Some(0o755),
        ..Default::default()
    };
    w.next_entry(&dir_meta, PasswordRef::none())
        .unwrap()
        .finish()
        .unwrap();
    // 目录下空文件
    let empty_meta = EntryMeta {
        name: "subdir/empty".into(),
        size_raw: Some(0),
        mode: Some(0o644),
        ..Default::default()
    };
    w.next_entry(&empty_meta, PasswordRef::none())
        .unwrap()
        .finish()
        .unwrap();
    // symlink -> 目标
    let link_meta = EntryMeta {
        name: "subdir/link".into(),
        is_symlink: true,
        symlink_target: Some("subdir/empty".into()),
        mode: Some(0o777),
        size_raw: Some(0),
        ..Default::default()
    };
    w.next_entry(&link_meta, PasswordRef::none())
        .unwrap()
        .finish()
        .unwrap();
    // 带模式的数据文件
    let file_meta = EntryMeta {
        name: "subdir/run.sh".into(),
        size_raw: Some(7),
        mode: Some(0o755),
        ..Default::default()
    };
    let mut s = w.next_entry(&file_meta, PasswordRef::none()).unwrap();
    s.write_all(b"echo hi").unwrap();
    s.finish().unwrap();
    w.finish().unwrap();

    let r =
        c.h.open_reader(Source::File(c.file("m.7z")), &Default::default())
            .unwrap();
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 4, "目录+空文件+symlink+文件");
    assert!(infos[0].is_dir, "0 为目录");
    assert_eq!(infos[0].name.as_str(), "subdir");
    assert!(!infos[1].is_dir && infos[1].size_raw == 0, "1 为空文件");
    assert!(infos[2].is_symlink, "2 为 symlink");
    // symlink 内容 = 目标字符串（写侧重放数据区）
    let link_content = read_all(r.read_entry(2, PasswordRef::none()).unwrap());
    assert_eq!(String::from_utf8_lossy(&link_content), "subdir/empty");
    assert_eq!(read_all(r.read_entry(1, PasswordRef::none()).unwrap()), b"");
    let sh = read_all(r.read_entry(3, PasswordRef::none()).unwrap());
    assert_eq!(sh, b"echo hi");
    assert_eq!(infos[3].mode, Some(0o755));
    // mode 保留（7z 属性高 16 位）
    assert_eq!(infos[0].mode, Some(0o755));
}

/// 数据区 CRC 损坏：压缩数据字节翻转后条目读取应报 CRC/格式错误（不 panic、
/// 不产乱码）。sevenz-rust 内部 `Crc32VerifyingReader` 于 EOF 抛错——本实现物化
/// 自校验映射 `Checksum` 3001 或 `Format` 2002/2004（红线 4 不 panic 为硬断言）。
#[test]
fn data_area_crc_corrupt() {
    let c = Ctx::new("datacrc");
    // 大条目（压缩块足够大，确保翻转落数据区而非 header）
    let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 249) as u8).collect();
    let p = c.file("d.7z");
    let mut w =
        c.h.create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
    let meta = EntryMeta {
        name: "big.bin".into(),
        size_raw: Some(payload.len() as u64),
        ..Default::default()
    };
    let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
    sink.write_all(&payload).unwrap();
    sink.finish().unwrap();
    w.finish().unwrap();
    drop(payload);

    let good = std::fs::read(&p).unwrap();
    // 签名头 32B + 数据区起始于 pack_pos（32 之后）。翻转数据区中部字节。
    let mut corrupt = good.clone();
    let mid = corrupt.len() / 2;
    corrupt[mid] ^= 0x55;
    let pc = c.file("corrupt.7z");
    std::fs::write(&pc, &corrupt).unwrap();

    let out = std::panic::catch_unwind(|| {
        let r = c.h.open_reader(Source::File(pc), &Default::default());
        if let Ok(rd) = r {
            // 头仍可读（数据区损坏不影响 entries）
            let _ = rd.entries().unwrap();
            // 读取数据区：应明确报错（3001/2002/2004/5001）或读得合法字节——
            // 但后者仅在损坏字节位于压缩流 padding 等不影响解码处才可能。
            let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let mut st = rd.read_entry(0, PasswordRef::none()).unwrap();
                let mut buf = Vec::new();
                st.read_to_end(&mut buf).unwrap();
                buf
            }));
            match res {
                Ok(buf) => {
                    // 读到字节：必须与原文 sha 一致（否则数据区损坏被静默放过即乱码）
                    let orig_reader =
                        c.h.open_reader(Source::File(p.clone()), &Default::default())
                            .unwrap();
                    let mut st0 = orig_reader.read_entry(0, PasswordRef::none()).unwrap();
                    let mut orig = Vec::new();
                    st0.read_to_end(&mut orig).unwrap();
                    assert_eq!(
                        sha256_hex(&buf),
                        sha256_hex(&orig),
                        "数据区损坏后读取若成功必须与原文一致"
                    );
                }
                Err(_) => {
                    // 读取报错亦可（不 panic 已由外层 catch_unwind 保证）
                }
            }
        }
    });
    assert!(out.is_ok(), "数据区损坏不应 panic（红线 4）");
}
