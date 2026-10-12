//! conformance 接入：对 `TarHandler` 展开 TC-CONF-001..010（05 T22 交付物 ③）。
//!
//! # 关于 testkit `run_conformance`（登记项，如实记录）
//!
//! 实测（`probe_run_conformance` 实验已删，结论存档）：testkit 的读路径用例
//! （001/003/004/006/008/010）以「桩档案 `HZIPSTUB`」驱动（`StubArchive` 自描述
//! 格式，`hzip-format-testkit::stub`）——该桩仅 `StubHandler`/`MockHandler` 可
//! 解析，真实 tar handler 依契约正确拒绝（首 512B 非 ustar → `BadSignature`
//! 2001，TC-CONF-008 红线 4 fail-fast 优先）。冻结的 testkit 不可修改（T22 §8
//! 纪律）；因此 `run_conformance(&TarHandler)` 的 001~004/010 会 Fail（属
//! testkit/契约缺陷，本任务不私改），005 现为 Pass（append 退化新建 + 尾块
//! 语义）、006/008 Pass、007/009 Skip。
//!
//! # 本文件策略
//!
//! 按「语义等价的真实 tar 档案」重新驱动 10 组（写→读回 + 能力负断言），每组
//! 断言对齐 03 §2.1 TC-CONF 预期与 T22 §6 单测清单，覆盖 tar 展开的契约红线：
//! 001 元数据 / 002 roundtrip / 003 选择性 / 004 乱序 / 005 裸 tar 追加（正向）
//! / 006 取消 / 007 无加密直读 / 008 畸形无 panic / 009 edit 恒 Unsupported /
//! 010 entries 零 IO。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::path::PathBuf;

use hzip_format::{
    CreateOptions, EntryMeta, ExtractCtx, FormatHandler, PasswordRef, PasswordSource, Selection,
    Sink, Source,
};
use sha2::{Digest, Sha256};

fn handler() -> hzip_fmt_tar::TarHandler {
    hzip_fmt_tar::TarHandler::new()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

fn tmp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "hzip-tar-conf-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

struct Ctx {
    dir: PathBuf,
    handler: hzip_fmt_tar::TarHandler,
}

impl Drop for Ctx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Ctx {
    fn new(tag: &str) -> Self {
        Self {
            dir: tmp_dir(tag),
            handler: handler(),
        }
    }
    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
    fn write_archive(&self, entries: &[(String, Vec<u8>)]) -> PathBuf {
        let p = self.file("archive.tar");
        let mut w = self
            .handler
            .create_writer(Sink::File(p.clone()), &Default::default())
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
        p
    }
}

fn read_all(mut r: Box<dyn Read + Send>) -> Vec<u8> {
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).unwrap();
    buf
}

/// TestEnv 最小（ExtractCtx 借用组件）。
struct Env<'a> {
    cancel: hzip_progress::CancellationToken,
    bus: hzip_format::ProgressBus,
    _pw: &'a dyn hzip_format::PasswordSource,
    sanitize: hzip_format::SanitizeConfig,
    quota: hzip_format::QuotaLimits,
}
impl<'a> Env<'a> {
    fn new(pw: &'a dyn hzip_format::PasswordSource) -> Self {
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

fn no_pw() -> NoPasswordSource {
    NoPasswordSource
}

/// tar 无加密：密码源恒返回 None（PasswordSource 测试替身）。
#[derive(Default)]
struct NoPasswordSource;
impl PasswordSource for NoPasswordSource {
    fn password(
        &self,
        _entry: Option<&hzip_format::EntryName>,
    ) -> Option<zeroize::Zeroizing<String>> {
        None
    }
}

#[test]
fn conf001_meta_integrity() {
    let c = Ctx::new("001");
    let p = c.write_archive(&[
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
    ]);
    let r = c
        .handler
        .open_reader(Source::File(p), &Default::default())
        .unwrap();
    let infos = r.entries().unwrap();
    assert!(!infos.is_empty());
    for (i, info) in infos.iter().enumerate() {
        assert_eq!(info.id, i, "id 连续");
        hzip_format::parse_entry_name(info.name.as_str()).unwrap();
        assert!(info.codec.as_deref() == Some("store"));
        assert!(!info.encrypted && !info.header_encrypted);
    }
    // 全字段类型合法
    for info in &infos {
        if let Some(mt) = info.mtime {
            let unix = mt.unix_timestamp();
            assert!(unix >= 0, "mtime 合理");
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
            (0..256u32).map(|i| (i % 251) as u8).collect(),
        ),
    ];
    let p = c.write_archive(&entries);
    let r = c
        .handler
        .open_reader(Source::File(p.clone()), &Default::default())
        .unwrap();
    let dest = c.file("out");
    std::fs::create_dir_all(&dest).unwrap();
    let pw = no_pw();
    let env = Env::new(&pw);
    let report = r.extract(&Selection::All, &env.ctx(), &dest).unwrap();
    assert!(report.failures.is_empty());
    for (name, data) in &entries {
        let on_disk = std::fs::read(dest.join(name)).unwrap();
        assert_eq!(sha256_hex(&on_disk), sha256_hex(data), "{name}");
    }
    // 再读回一致性
    let r2 = c
        .handler
        .open_reader(Source::File(p), &Default::default())
        .unwrap();
    let infos = r2.entries().unwrap();
    assert_eq!(infos.len(), entries.len());
}

#[test]
fn conf003_selective() {
    let c = Ctx::new("003");
    let entries: Vec<(String, Vec<u8>)> = (0..100)
        .map(|i| (format!("f{i:03}.txt"), format!("content-{i}").into_bytes()))
        .collect();
    let p = c.write_archive(&entries);
    let r = c
        .handler
        .open_reader(Source::File(p), &Default::default())
        .unwrap();
    let dest = c.file("out");
    std::fs::create_dir_all(&dest).unwrap();
    let pw = no_pw();
    let env = Env::new(&pw);
    let _ = r
        .extract(&Selection::Ids(vec![5, 50]), &env.ctx(), &dest)
        .unwrap();
    let count = std::fs::read_dir(&dest).unwrap().count();
    assert_eq!(count, 2, "选择性解压恰好 2 落盘");
}

#[test]
fn conf004_out_of_order() {
    let c = Ctx::new("004");
    let entries: Vec<(String, Vec<u8>)> = (0..20)
        .map(|i| (format!("o{i:02}.bin"), vec![i as u8; 100]))
        .collect();
    let p = c.write_archive(&entries);
    let r = c
        .handler
        .open_reader(Source::File(p), &Default::default())
        .unwrap();
    for i in (0..20).rev() {
        let buf = read_all(r.read_entry(i, PasswordRef::none()).unwrap());
        assert_eq!(buf.len(), 100);
        assert_eq!(buf[0], i as u8);
    }
    for _ in 0..2 {
        for &i in &[0usize, 10usize] {
            let buf = read_all(r.read_entry(i, PasswordRef::none()).unwrap());
            assert_eq!(buf[0], i as u8);
        }
    }
}

#[test]
fn conf005_append_bare_tar() {
    let c = Ctx::new("005");
    // 先写两文件
    let p = c.write_archive(&[
        ("old-a.txt".into(), b"old-a".to_vec()),
        ("old-b.txt".into(), b"old-b-content".to_vec()),
    ]);
    let before = std::fs::read(&p).unwrap();
    // 记录旧条目的 tar 数据区字节位置：以追加前旧数据 sha 为准（追加后不变）
    let old_a = b"old-a".to_vec();
    let old_b = b"old-b-content".to_vec();

    // 追加一个新文件
    let mut w = c
        .handler
        .create_writer(
            Sink::File(p.clone()),
            &CreateOptions {
                append: true,
                ..Default::default()
            },
        )
        .unwrap();
    let meta = EntryMeta {
        name: "new-c.txt".into(),
        size_raw: Some(7),
        ..Default::default()
    };
    let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
    sink.write_all(b"new-c!!").unwrap();
    sink.finish().unwrap();
    w.finish().unwrap();

    let after = std::fs::read(&p).unwrap();
    // 追加后旧条目可读、字节 sha 不变
    let r = c
        .handler
        .open_reader(Source::File(p), &Default::default())
        .unwrap();
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 3, "追加后条目数 +1");
    let old_a_read = read_all(r.read_entry(0, PasswordRef::none()).unwrap());
    assert_eq!(old_a_read, old_a);
    let old_b_read = read_all(r.read_entry(1, PasswordRef::none()).unwrap());
    assert_eq!(old_b_read, old_b);
    let new_read = read_all(r.read_entry(2, PasswordRef::none()).unwrap());
    assert_eq!(new_read, b"new-c!!");
    // 旧 tar 前段（去掉旧尾零块 + 尾窗后的部分）字节不变：追加只影响旧尾零块
    // 之后——以旧档案去掉最后 1024B（尾零块）后的前缀是否仍是新档案前缀来断言。
    let old_prefix_end = before.len() - 1024.min(before.len());
    assert!(
        after.starts_with(&before[..old_prefix_end]),
        "追加后旧条目数据区不应变动"
    );
}

#[test]
fn conf005b_append_stream_unsupported() {
    // append + Sink::Stream（组合链）→ Unsupported HZ-5001
    let c = Ctx::new("005b");
    let p = c.write_archive(&[("a.txt".into(), b"x".to_vec())]);
    let f = std::fs::File::open(&p).unwrap();
    let r = c.handler.create_writer(
        Sink::Stream(Box::new(f)),
        &CreateOptions {
            append: true,
            ..Default::default()
        },
    );
    match r {
        Err(e) if e.code() == 5001 => {}
        other => panic!(
            "append Stream 应 Unsupported(5001)，实得 {:?}",
            other.map(|_| ()).map_err(|e| e.code())
        ),
    }
}

#[test]
fn conf008_malformed_no_panic() {
    let c = Ctx::new("008");
    let good = {
        let p = c.write_archive(&[
            ("a.txt".into(), b"hello".to_vec()),
            (
                "b.bin".into(),
                (0..300u32).map(|i| (i % 251) as u8).collect(),
            ),
        ]);
        std::fs::read(&p).unwrap()
    };
    let samples: Vec<Vec<u8>> = vec![
        good[..good.len() * 9 / 10].to_vec(),
        good[..good.len() / 2].to_vec(),
        good[..good.len() - 1].to_vec(),
        {
            let mut b = good.clone();
            // 位翻转 20 处
            let mut seed = 0x1234_5678u32;
            let b_len = b.len() as u32;
            let idx = |seed: &mut u32| -> usize {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 17;
                *seed ^= *seed << 5;
                (*seed % b_len) as usize
            };
            for _ in 0..20 {
                let i = idx(&mut seed);
                b[i] ^= 0x40;
            }
            b
        },
    ];
    for (i, s) in samples.iter().enumerate() {
        let p = c.file(&format!("bad-{i}.tar"));
        std::fs::write(&p, s).unwrap();
        let outcome = std::panic::catch_unwind(|| {
            let r = c
                .handler
                .open_reader(Source::File(p.clone()), &Default::default());
            if let Ok(rd) = r {
                let _ = rd.entries();
            }
        });
        assert!(outcome.is_ok(), "畸形输入不应 panic（sample {i}）");
    }
}

#[test]
fn conf010_entries_zero_io_fast() {
    let c = Ctx::new("010");
    // 大条目（8 MiB，足够验证 entries 不读数据区耗时）
    let p = c.write_archive(&[("big.bin".into(), vec![0u8; 8 * 1024 * 1024])]);
    let started = std::time::Instant::now();
    let r = c
        .handler
        .open_reader(Source::File(p), &Default::default())
        .unwrap();
    let infos = r.entries().unwrap();
    let elapsed = started.elapsed();
    assert!(!infos.is_empty());
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "entries 不应读数据区（{} ms）",
        elapsed.as_millis()
    );
}

#[test]
fn conf007_wrong_password_skipped_no_crypto() {
    // tar 无加密：open/read 不涉及密码；007 经 CapsFilter 自动 Skip（no encryption）
    let c = Ctx::new("007");
    let p = c.write_archive(&[("secret.txt".into(), b"top secret".to_vec())]);
    let r = c
        .handler
        .open_reader(Source::File(p), &Default::default())
        .unwrap();
    let buf = read_all(r.read_entry(0, PasswordRef::none()).unwrap());
    assert_eq!(buf, b"top secret");
}

/// TC-CONF-006 语义：取消令牌在条目级检查；单条目流本身不轮询（tar 顺序读，
/// 取消粒度 = 条目边界）。断言：预取消后 extract 立即返回 Cancelled 且不落盘。
#[test]
fn conf006_cancel_between_entries() {
    let c = Ctx::new("006");
    let p = c.write_archive(&[
        ("a.txt".into(), b"aaa".to_vec()),
        ("b.txt".into(), vec![0x42u8; 4096]),
    ]);
    let r = c
        .handler
        .open_reader(Source::File(p), &Default::default())
        .unwrap();
    let dest = c.file("out");
    std::fs::create_dir_all(&dest).unwrap();

    let tok = hzip_progress::CancellationToken::new();
    tok.cancel();
    let pw = no_pw();
    let env = Env {
        cancel: tok,
        bus: Default::default(),
        _pw: &pw,
        sanitize: Default::default(),
        quota: Default::default(),
    };
    let ectx = env.ctx();
    let res = r.extract(&Selection::All, &ectx, &dest);
    let code = res.as_ref().map_err(|e| e.code()).err();
    assert!(
        code == Some(6001),
        "预取消应返回 Cancelled（HZ-6001），实得 {res:?}"
    );
    assert_eq!(
        std::fs::read_dir(&dest).map(|d| d.count()).unwrap_or(0),
        0,
        "取消后不应有落盘条目"
    );
}

#[test]
fn conf009_editor_unsupported() {
    let c = Ctx::new("009");
    let p = c.write_archive(&[("a.txt".into(), b"orig".to_vec())]);
    let before = std::fs::read(&p).unwrap();
    let r = c
        .handler
        .open_editor(Source::File(p.clone()), &Default::default());
    assert!(
        matches!(r, Err(e) if e.code() == 5001),
        "open_editor 应 Unsupported(5001)"
    );
    // 原档案字节不变
    let after = std::fs::read(&p).unwrap();
    assert_eq!(before, after);
}

/// TC-ALG-013 长名：259/260+ 字符名（GNU L 头/ustar prefix 覆盖）经 append_data
/// 写入 → 读回一致（T22 §4.2：固定 append_data 保证长名可写）。
#[test]
fn alg013_long_name_roundtrip() {
    let c = Ctx::new("013");
    for n in [259usize, 260usize, 300usize] {
        // 构造恰好 n 字节名（含分隔）
        let mut name = String::new();
        while name.len() + 60 < n {
            name.push_str("seg/");
            name.push_str(&"x".repeat(40));
            name.push('/');
        }
        name.truncate(n);
        name = name.trim_end_matches('/').to_string();
        name = format!("{name}-{}", n % 10); // 稳定尾缀避免全 '/' 剥空
        let data = format!("content-{n}").into_bytes();
        let p = c.file(&format!("long-{n}.tar"));
        let mut w = c
            .handler
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        let meta = EntryMeta {
            name: name.as_str().into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
        sink.write_all(&data).unwrap();
        sink.finish().unwrap();
        w.finish().unwrap();

        let r = c
            .handler
            .open_reader(Source::File(p.clone()), &Default::default())
            .unwrap();
        let infos = r.entries().unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name.as_str(), name, "长名 {n} 读回一致");
        let got = read_all(r.read_entry(0, PasswordRef::none()).unwrap());
        assert_eq!(got, data);
    }
}

/// Source::Stream（组合链内层）经临时 spool 缓冲后读取（T22 §4.5）。
#[test]
fn source_stream_spooled_read() {
    let c = Ctx::new("stream");
    let p = c.write_archive(&[
        ("a.txt".into(), b"stream-a".to_vec()),
        ("sub/b.bin".into(), vec![7u8; 2000]),
    ]);
    let bytes = std::fs::read(&p).unwrap();
    let reader = std::io::Cursor::new(bytes);
    let r = c
        .handler
        .open_reader(Source::Stream(Box::new(reader)), &Default::default())
        .unwrap();
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 2);
    let a = read_all(r.read_entry(0, PasswordRef::none()).unwrap());
    assert_eq!(a, b"stream-a");
    let b = read_all(r.read_entry(1, PasswordRef::none()).unwrap());
    assert_eq!(b, vec![7u8; 2000]);
}

/// 空档案（0 字节 / 全零块）→ 合法空 tar：entries()==0（TC-ALG-010）。
/// 非零垃圾 → BadSignature 2001。
#[test]
fn empty_archive_and_bad_signature() {
    let c = Ctx::new("empty");
    // 0 字节 → 空档案（entries=0）
    let p = c.file("empty.tar");
    std::fs::write(&p, b"").unwrap();
    let r = c
        .handler
        .open_reader(Source::File(p.clone()), &Default::default())
        .unwrap();
    assert_eq!(
        r.entries().unwrap().len(),
        0,
        "空档 entries=0（TC-ALG-010）"
    );
    // 全零 1024B（仅尾块）→ 空档案
    let p2 = c.file("zeros.tar");
    std::fs::write(&p2, [0u8; 1024]).unwrap();
    let r = c
        .handler
        .open_reader(Source::File(p2), &Default::default())
        .unwrap();
    assert_eq!(r.entries().unwrap().len(), 0);
    // 非 tar 垃圾 → BadSignature 2001
    let p3 = c.file("garbage.bin");
    std::fs::write(&p3, b"this is not a tar archive at all").unwrap();
    let r = c.handler.open_reader(Source::File(p3), &Default::default());
    assert!(
        matches!(r, Err(e) if e.code() == 2001),
        "垃圾应 BadSignature(2001)"
    );
}

/// 归档根的自我描述条目 `./`（`tar cf x.tar .` 的产物）必须**跳过**而不是报坏头。
///
/// 修复前：`parse_entry_name("./")` 剥净段后是空名 → 2004，于是「用 GNU tar 压一个
/// 目录」这种最常见的档案**完全打不开**（cpio 侧同样形态，由
/// `hzip_format::is_root_entry_name` 统一判定）。本用例即该缺陷的回归钉。
#[test]
fn root_dot_entry_is_skipped_not_rejected() {
    let c = Ctx::new("root-dot");
    let p = c.file("dotslash.tar");
    {
        let f = std::fs::File::create(&p).unwrap();
        let mut b = tar::Builder::new(f);
        // `tar crate` 的 path API 会规范化掉 `./`，故直接改原始名字字段（GNU 头）
        let mut dir = tar::Header::new_gnu();
        dir.as_gnu_mut().expect("gnu 头").name[..2].copy_from_slice(b"./");
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_mode(0o755);
        dir.set_size(0);
        dir.set_mtime(0);
        dir.set_cksum();
        b.append(&dir, std::io::empty()).unwrap();

        let mut fh = tar::Header::new_gnu();
        fh.set_path("a.txt").unwrap();
        fh.set_entry_type(tar::EntryType::Regular);
        fh.set_mode(0o644);
        fh.set_size(4);
        fh.set_mtime(0);
        fh.set_cksum();
        b.append(&fh, &b"data"[..]).unwrap();
        b.finish().unwrap();
    }
    let r = c
        .handler
        .open_reader(Source::File(p), &Default::default())
        .expect("`./` 根条目不得让整档打不开");
    let names: Vec<String> = r
        .entries()
        .unwrap()
        .into_iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert_eq!(names, vec!["a.txt"], "`./` 被跳过，只留真实条目");
    assert_eq!(
        read_all(r.read_entry(0, PasswordRef::none()).unwrap()),
        b"data"
    );
}
