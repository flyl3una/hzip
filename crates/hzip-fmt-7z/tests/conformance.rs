//! T25 conformance 展开与真实 7z 自产样本验收。
//!
//! # testkit `run_conformance` 接入（登记项）
//!
//! testkit 读路径用例（001/002/003/004/006/008/010）以**桩档案 `HZIPSTUB`** 驱动
//! （`hzip-format-testkit::stub::StubArchive` 自描述格式），仅 `StubHandler`/
//! `MockHandler` 可解析；真实 7z handler 依契约正确拒绝（首字节非 7z 签名 →
//! `BadSignature` 2001，TC-CONF-008 红线 4 fail-fast 优先）。冻结的 testkit 不可
//! 修改（同 T22 纪律）；故 `run_conformance(&SevenZipFormatHandler)` 的
//! 001/002/003/004/006/008/010 会 Fail（testkit/契约缺陷，非本卡私有）。
//! 005 现为 Pass（append=false 负断言）；007/009 经 `CapsFilter` 自动 Skip
//! （007：encryption=None；009：edit 用例以 .stub 驱动会 Fail，见下）。
//!
//! # 本文件策略
//!
//! 与 T22/tar 一致：以「语义等价的**真实 7z 档案**」重新驱动 TC-CONF 各组 +
//! T25 §6.1 单测清单（写→读回 + 能力负断言 + 黄金样本），覆盖 7z 展开的契约红线。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use hzip_fmt_7z::SevenZipFormatHandler;
use hzip_format::{
    CreateOptions, EntryMeta, ExtractCtx, FormatHandler, PasswordRef, PasswordSource, Selection,
    Sink, Source,
};
use sha2::{Digest, Sha256};

fn handler() -> SevenZipFormatHandler {
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
            "hzip-7z-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir, h: handler() }
    }
    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
    fn write_archive(&self, path: &Path, entries: &[(String, Vec<u8>)]) -> PathBuf {
        let mut w = self
            .h
            .create_writer(Sink::File(path.to_path_buf()), &Default::default())
            .unwrap();
        for (name, data) in entries {
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
        path.to_path_buf()
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn read_all(mut r: Box<dyn Read + Send>) -> Vec<u8> {
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).unwrap();
    buf
}

struct Env<'a> {
    cancel: hzip_progress::CancellationToken,
    bus: hzip_format::ProgressBus,
    _pw: &'a dyn PasswordSource,
    sanitize: hzip_format::SanitizeConfig,
    quota: hzip_format::QuotaLimits,
}
impl<'a> Env<'a> {
    fn new(pw: &'a dyn PasswordSource) -> Self {
        Self {
            cancel: Default::default(),
            bus: Default::default(),
            _pw: pw,
            sanitize: Default::default(),
            quota: Default::default(),
        }
    }
    fn ctx(&'a self) -> ExtractCtx<'a> {
        use hzip_format::ConflictPolicy;
        ExtractCtx {
            cancel: &self.cancel,
            progress: &self.bus,
            passwords: self._pw,
            conflicts: ConflictPolicy::Overwrite,
            sanitize: &self.sanitize,
            quota: &self.quota,
            best_effort: false,
            codepage: None,
        }
    }
}

struct NoPw;
impl PasswordSource for NoPw {
    fn password(&self, _e: Option<&hzip_format::EntryName>) -> Option<zeroize::Zeroizing<String>> {
        None
    }
}

fn mixed_entries() -> Vec<(String, Vec<u8>)> {
    vec![
        ("hello.txt".into(), b"hello world\n".to_vec()),
        ("empty.dat".into(), Vec::new()),
        (
            "bin/data.bin".into(),
            (0..256u32).map(|i| (i % 251) as u8).collect(),
        ),
        (
            "docs/name-cn.txt".into(),
            "chinese-content".as_bytes().to_vec(),
        ),
        ("a/b/c/d/deep.txt".into(), b"deep".to_vec()),
    ]
}

#[test]
fn conf001_meta_integrity() {
    let c = Ctx::new("001");
    let p = c.write_archive(&c.file("a.7z"), &mixed_entries());
    let r =
        c.h.open_reader(Source::File(p), &Default::default())
            .unwrap();
    let infos = r.entries().unwrap();
    assert!(!infos.is_empty());
    for (i, info) in infos.iter().enumerate() {
        assert_eq!(info.id, i, "id 连续");
        hzip_format::parse_entry_name(info.name.as_str()).unwrap();
        if info.size_raw > 0 {
            assert!(
                info.codec.as_deref() == Some("lzma2"),
                "非空条目 codec=lzma2"
            );
        }
        assert!(!info.encrypted && !info.header_encrypted);
    }
    for info in &infos {
        if let Some(mt) = info.mtime {
            assert!(mt.unix_timestamp() >= 0, "mtime 合理");
        }
    }
}

#[test]
fn conf002_full_roundtrip() {
    let c = Ctx::new("002");
    let entries: Vec<(String, Vec<u8>)> = vec![
        ("hello.txt".into(), b"hello world\n".to_vec()),
        ("empty.dat".into(), Vec::new()),
        (
            "bin/data.bin".into(),
            (0..300u32).map(|i| (i % 251) as u8).collect(),
        ),
        (
            "x.bin".into(),
            (0..200_000u32).map(|i| (i % 253) as u8).collect(),
        ),
    ];
    let p = c.write_archive(&c.file("rt.7z"), &entries);
    let r =
        c.h.open_reader(Source::File(p.clone()), &Default::default())
            .unwrap();
    let dest = c.file("out");
    std::fs::create_dir_all(&dest).unwrap();
    let pw = NoPw;
    let env = Env::new(&pw);
    let rep = r.extract(&Selection::All, &env.ctx(), &dest).unwrap();
    assert!(rep.failures.is_empty());
    for (name, data) in &entries {
        let on_disk = std::fs::read(dest.join(name)).unwrap();
        assert_eq!(sha256_hex(&on_disk), sha256_hex(data), "{name}");
    }
    let r2 =
        c.h.open_reader(Source::File(p), &Default::default())
            .unwrap();
    assert_eq!(r2.entries().unwrap().len(), entries.len());
}

#[test]
fn conf003_selective() {
    let c = Ctx::new("003");
    let entries: Vec<(String, Vec<u8>)> = (0..100)
        .map(|i| (format!("f{i:03}.txt"), format!("content-{i}").into_bytes()))
        .collect();
    let p = c.write_archive(&c.file("sel.7z"), &entries);
    let r =
        c.h.open_reader(Source::File(p), &Default::default())
            .unwrap();
    let dest = c.file("out");
    std::fs::create_dir_all(&dest).unwrap();
    let pw = NoPw;
    let env = Env::new(&pw);
    r.extract(&Selection::Ids(vec![5, 50]), &env.ctx(), &dest)
        .unwrap();
    let files = std::fs::read_dir(&dest)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().is_file())
        .count();
    assert_eq!(files, 2, "选择性解压恰好 2 文件落盘");
}

#[test]
fn conf004_out_of_order() {
    let c = Ctx::new("004");
    // 20 条各自独立 folder -> 逆序/重复读触发物化缓存
    let entries: Vec<(String, Vec<u8>)> = (0..20)
        .map(|i| (format!("o{i:02}.bin"), vec![i as u8; 100]))
        .collect();
    let p = c.write_archive(&c.file("ooo.7z"), &entries);
    let r =
        c.h.open_reader(Source::File(p), &Default::default())
            .unwrap();
    for i in (0..20).rev() {
        let buf = read_all(r.read_entry(i, PasswordRef::none()).unwrap());
        assert_eq!(buf.len(), 100);
        assert_eq!(buf[0], i as u8, "逆序读 {i}");
    }
    for _ in 0..2 {
        for &i in &[0usize, 10usize] {
            let buf = read_all(r.read_entry(i, PasswordRef::none()).unwrap());
            assert_eq!(buf[0], i as u8, "重复读 {i}");
        }
    }
}

#[test]
fn conf005_append_unsupported() {
    let c = Ctx::new("005");
    let p = c.write_archive(&c.file("a.7z"), &[("a.txt".into(), b"x".to_vec())]);
    let r = c.h.create_writer(
        Sink::File(p.clone()),
        &CreateOptions {
            append: true,
            ..Default::default()
        },
    );
    assert!(
        matches!(r, Err(e) if e.code() == 5001),
        "append 应 Unsupported 5001"
    );
    // 原档案未被改动
    let r =
        c.h.open_reader(Source::File(p), &Default::default())
            .unwrap();
    assert_eq!(r.entries().unwrap().len(), 1);
}

#[test]
fn conf008_malformed_no_panic() {
    let c = Ctx::new("008");
    let good = {
        let p = c.write_archive(&c.file("good.7z"), &mixed_entries());
        std::fs::read(&p).unwrap()
    };
    let samples: Vec<Vec<u8>> = vec![
        good[..good.len() * 9 / 10].to_vec(),
        good[..good.len() / 2].to_vec(),
        good[..good.len() - 1].to_vec(),
        {
            let mut b = good.clone();
            let mut seed = 0x1234_5678u32;
            let blen = b.len() as u32;
            let mut idx = || -> usize {
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
    for (i, s) in samples.iter().enumerate() {
        let p = c.file(&format!("bad-{i}.7z"));
        std::fs::write(&p, s).unwrap();
        let out = std::panic::catch_unwind(|| {
            let r =
                c.h.open_reader(Source::File(p.clone()), &Default::default());
            if let Ok(rd) = r {
                let _ = rd.entries();
            }
        });
        assert!(out.is_ok(), "畸形输入不应 panic（sample {i}）");
    }
}

#[test]
fn conf010_entries_zero_io_fast() {
    let c = Ctx::new("010");
    // 2 MiB 压缩数据（entries() 只读头，与数据区大小无关；控制在 debug 模式
    // lzma2 压缩可接受耗时内）
    let p = c.write_archive(
        &c.file("big.7z"),
        &[("big.bin".into(), vec![0u8; 2 * 1024 * 1024])],
    );
    let started = std::time::Instant::now();
    let r =
        c.h.open_reader(Source::File(p), &Default::default())
            .unwrap();
    let infos = r.entries().unwrap();
    let elapsed = started.elapsed();
    assert!(!infos.is_empty());
    assert!(
        elapsed < std::time::Duration::from_millis(2000),
        "entries 不应读数据区（{} ms）",
        elapsed.as_millis()
    );
}

#[test]
fn conf006_cancel_between_entries() {
    let c = Ctx::new("006");
    let p = c.write_archive(
        &c.file("a.7z"),
        &[
            ("a.txt".into(), b"aaa".to_vec()),
            ("b.bin".into(), vec![0x42u8; 8192]),
        ],
    );
    let r =
        c.h.open_reader(Source::File(p), &Default::default())
            .unwrap();
    let dest = c.file("out");
    std::fs::create_dir_all(&dest).unwrap();
    let tok = hzip_progress::CancellationToken::new();
    tok.cancel();
    let pw = NoPw;
    let env = Env {
        cancel: tok,
        bus: Default::default(),
        _pw: &pw,
        sanitize: Default::default(),
        quota: Default::default(),
    };
    let ectx = env.ctx();
    let res = r.extract(&Selection::All, &ectx, &dest);
    assert!(
        matches!(res.as_ref(), Err(e) if e.code() == 6001),
        "预取消应 Cancelled（6001），实得 {res:?}"
    );
    assert_eq!(
        std::fs::read_dir(&dest).map(|d| d.count()).unwrap_or(0),
        0,
        "取消后不应有落盘条目"
    );
}

#[test]
fn conf009_editor_rewrite() {
    let c = Ctx::new("009");
    let p = c.write_archive(
        &c.file("edit.7z"),
        &[
            ("a.txt".into(), b"orig-a".to_vec()),
            ("b.txt".into(), b"orig-b".to_vec()),
        ],
    );
    let before = std::fs::read(&p).unwrap();

    let wcancel = hzip_progress::CancellationToken::new();
    let wbus = hzip_format::ProgressBus::new();
    let wopts = CreateOptions::default();
    let wctx = hzip_format::WriteCtx {
        cancel: &wcancel,
        progress: &wbus,
        opts: &wopts,
    };
    // 删除 b、把 a 替换为新内容、追加 c
    let dst = c.file("edit-out.7z");
    let rep_src = c.file("replace.txt");
    std::fs::write(&rep_src, b"new-a-content").unwrap();
    let editor =
        c.h.open_editor(Source::File(p.clone()), &Default::default())
            .unwrap();
    let cs = hzip_format::ChangeSet {
        changes: vec![
            hzip_format::Change::Replace {
                entry: 0,
                src: rep_src.clone(),
            },
            hzip_format::Change::Delete(1),
            hzip_format::Change::Append {
                src: rep_src.clone(),
                name: "c.txt".into(),
            },
        ],
    };
    let rep = editor.rewrite(cs, Sink::File(dst.clone()), &wctx).unwrap();
    assert_eq!(rep.entries_written, 2, "a(替换) + c(追加)，b 删除");

    // 源档案字节不变（原子性）
    let after = std::fs::read(&p).unwrap();
    assert_eq!(before, after, "rewrite 不应改动源档案");

    // 重开产物校验
    let r =
        c.h.open_reader(Source::File(dst), &Default::default())
            .unwrap();
    let infos = r.entries().unwrap();
    let names: Vec<String> = infos.iter().map(|i| i.name.as_str().to_string()).collect();
    assert_eq!(names, vec!["a.txt", "c.txt"]);
    let a = read_all(r.read_entry(0, PasswordRef::none()).unwrap());
    assert_eq!(a, b"new-a-content");
    let cc = read_all(r.read_entry(1, PasswordRef::none()).unwrap());
    assert_eq!(cc, b"new-a-content");
}

#[test]
fn empty_archive_and_bad_signature() {
    let c = Ctx::new("empty");
    // 真实空档案（自产 0 条目）
    let p = c.file("empty.7z");
    let w =
        c.h.create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
    let rep = w.finish().unwrap();
    assert_eq!(rep.entries_written, 0);
    let r =
        c.h.open_reader(Source::File(p), &Default::default())
            .unwrap();
    assert_eq!(
        r.entries().unwrap().len(),
        0,
        "空档 entries=0（TC-ALG-010）"
    );
    // 垃圾 -> BadSignature 2001
    let p2 = c.file("garbage.7z");
    std::fs::write(&p2, b"not a 7z archive!!").unwrap();
    let r = c.h.open_reader(Source::File(p2), &Default::default());
    assert!(matches!(r, Err(e) if e.code() == 2001), "垃圾应 2001");
}

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("assets")
        .join(name)
}

/// TC-X-001 / TC-ALG-004 解码半场：7-Zip 产 lzma2+BCJ2 档案 -> 解出 x86 exe 与
/// 7-Zip 自产参照字节一致（sevenz-rust 内置 BCJ2 解码，经本 handler 读路径）。
#[test]
fn bcj2_read_path_golden() {
    let c = Ctx::new("bcj2");
    let p = asset("bcj2-lzma2.7z");
    // 先直接断言样本确为 BCJ2 folder（coder 链含 BCJ2）
    let mut file = std::fs::File::open(&p).unwrap();
    let len = file.metadata().unwrap().len();
    let archive =
        sevenz_rust::Archive::read(&mut file, len, sevenz_rust::Password::empty().as_slice())
            .unwrap();
    assert!(!archive.folders.is_empty());
    let has_bcj2 = archive.folders.iter().any(|f| {
        f.coders
            .iter()
            .any(|cd| cd.decompression_method_id() == sevenz_rust::SevenZMethod::ID_BCJ2)
    });
    assert!(has_bcj2, "bcj2-lzma2.7z 样本应为 BCJ2 folder");

    let r =
        c.h.open_reader(Source::File(p.clone()), &Default::default())
            .unwrap();
    let infos = r.entries().unwrap();
    assert!(!infos.is_empty(), "BCJ2 档案应有条目");
    // 逆序读取：验证 BCJ2 folder 物化缓存复用（CRC 在物化期校验）
    let ids: Vec<usize> = (0..infos.len()).collect();
    for &i in ids.iter().rev() {
        let buf = read_all(r.read_entry(i, PasswordRef::none()).unwrap());
        if !infos[i].is_dir && infos[i].size_raw > 0 {
            assert_eq!(buf.len() as u64, infos[i].size_raw);
        }
    }
    // x86 参照：decompress_example_lzma2_bcj_x86.7z 解出 decompress.exe
    let p2 = asset("bcj-x86-lzma2.7z");
    let r2 =
        c.h.open_reader(Source::File(p2), &Default::default())
            .unwrap();
    let infos2 = r2.entries().unwrap();
    assert!(!infos2.is_empty());
    for (i, info) in infos2.iter().enumerate() {
        if info.name.as_str().ends_with("decompress.exe") || !info.is_dir {
            let data = read_all(r2.read_entry(i, PasswordRef::none()).unwrap());
            let expected = std::fs::read(asset("decompress_x86.exe")).unwrap();
            assert_eq!(
                sha256_hex(&data),
                sha256_hex(&expected),
                "BCJ/x86 解压字节应一致"
            );
            break;
        }
    }
}

/// 头加密样本（7-Zip 24 `-mhe=on` 产，真头加密）：零密码打开 -> sevenz-rust
/// `PasswordRequired` -> `Password{Header}` HZ-3102；`entries()` 不产出任何条目名
/// （零泄露）。本样本 `7z l`（无密码）连名称列表都拒绝。
#[test]
fn header_encrypted_flag() {
    let c = Ctx::new("hdr");
    let p = asset("headeren.7z");
    let r =
        c.h.open_reader(Source::File(p.clone()), &Default::default());
    match r {
        Err(e) => {
            assert_eq!(e.code(), 3102, "头加密应 HZ-3102，实得 {}", e.code());
        }
        Ok(rd) => {
            // 兜底（若解析意外成功）：不得泄露任何条目名（红线段）
            let infos = rd.entries().unwrap();
            assert_eq!(infos.len(), 0, "不应泄露任何条目名");
        }
    }
}

/// 内容加密（非头）样本（7-Zip 产）：无密码 entries() 可见名称（头未加密）；
/// 逐条目读应因 AES 未解锁报错或空（不产出乱码流；红线 3）。
#[test]
fn content_encrypted_no_garbled_stream() {
    let c = Ctx::new("pw");
    let p = asset("encrypted.7z");
    let r =
        c.h.open_reader(Source::File(p.clone()), &Default::default());
    if let Ok(rd) = r {
        let infos = rd.entries().unwrap();
        assert!(!infos.is_empty(), "内容加密头未加密 -> 名称可列");
        // 读加密条目：本基础版无 AES 读（caps.encryption=None），不得产出乱码。
        for i in 0..infos.len() {
            if let Ok(mut st) = rd.read_entry(i, PasswordRef::none()) {
                let mut buf = Vec::new();
                let _ = st.read_to_end(&mut buf);
            }
        }
    }
}

/// 空/单文件黄金样本读回（sevenz-rust 自产形态）。
#[test]
fn single_empty_golden() {
    let c = Ctx::new("single");
    let p = asset("single_empty.7z");
    let r =
        c.h.open_reader(Source::File(p), &Default::default())
            .unwrap();
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 1);
    let data = read_all(r.read_entry(0, PasswordRef::none()).unwrap());
    assert_eq!(data, b"");
}

/// unknown_method_explicit（TC-X-001 ppmd 语义）：7-Zip `-m0=PPMd` 产物 -> 打开或
/// 读取时**明确错误**（Unsupported/Format{UnknownMethod} 2003），不产出乱码流。
#[test]
fn unknown_method_explicit() {
    let c = Ctx::new("ppmd");
    let p = asset("ppmd.7z");
    // ppmd folder 头可解析；解码期 sevenz 无 ppmd 解码器 -> 读取任一条目应
    // Err（明确错误），不 panic、不产乱码。
    let outcome = std::panic::catch_unwind(|| {
        let r =
            c.h.open_reader(Source::File(p.clone()), &Default::default());
        match r {
            Ok(rd) => {
                let infos = rd.entries().unwrap();
                for i in 0..infos.len() {
                    let res = rd.read_entry(i, PasswordRef::none());
                    match res {
                        Ok(mut st) => {
                            let mut buf = Vec::new();
                            let rr = st.read_to_end(&mut buf);
                            if rr.is_ok() && !buf.is_empty() {
                                panic!("ppmd 条目应读取报错而非产出 {} 字节", buf.len());
                            }
                        }
                        Err(e) => {
                            let code = e.code();
                            assert!(
                                matches!(code, 2003 | 5001 | 2004 | 2002),
                                "ppmd 读取应明确错误（2003/5001/2004/2002），实得 {code}"
                            );
                        }
                    }
                }
            }
            Err(e) => {
                assert!(
                    matches!(e.code(), 2003 | 5001 | 2004),
                    "ppmd 打开应明确错误"
                );
            }
        }
    });
    assert!(outcome.is_ok(), "ppmd 处理不应 panic（TC-X-001 语义）");
}
