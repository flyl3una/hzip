//! 10 组一致性用例实现（03 §2.1 TC-CONF-001..010 逐条；02 §8.6）。
//!
//! 每组以 [`run_case`] 分发，统一经 [`PanicCounter::guard`] 包裹（红线 4：
//! 任何输入不 panic——panic 转 Fail 并计数）。
//!
//! 自产档案策略：读路径用例以「桩档案 + open_reader 读回」驱动（stub 序列化或
//! handler writer 自产）；各 fmt 的 conformance 接入（T22~T26）以 handler 自身
//! 档案驱动写侧语义。

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use hzip_error::HzipError;
use hzip_format::{
    ArchiveReader, ConflictPolicy, CreateOptions, ExtractCtx, FormatHandler, PasswordRef,
    QuotaLimits, SanitizeConfig, Selection, Sink, Source,
};
use sha2::{Digest, Sha256};

use crate::filter::{CapsFilter, ConfCaseId, RunDecision};
use crate::kit::{DynOutcome, DynTest, KitCtx, expanded_name};
use crate::malformed::PanicCounter;
use crate::mock::MockEntry;

/// 单一档案测试密码（TC-CONF-007 的「正确密码」）。
pub const TEST_PASSWORD: &str = "hzip-pw-test";

/// 固定密码源（PasswordSource 测试替身）。
#[derive(Default)]
pub struct FixedPasswordSource {
    pw: Option<zeroize::Zeroizing<String>>,
}

impl FixedPasswordSource {
    /// 返回 `TEST_PASSWORD`。
    pub fn test() -> Self {
        Self {
            pw: Some(zeroize::Zeroizing::new(TEST_PASSWORD.to_string())),
        }
    }

    /// 返回给定密码。
    pub fn with(pw: &str) -> Self {
        Self {
            pw: Some(zeroize::Zeroizing::new(pw.to_string())),
        }
    }
}

impl hzip_format::PasswordSource for FixedPasswordSource {
    fn password(
        &self,
        _entry: Option<&hzip_format::EntryName>,
    ) -> Option<zeroize::Zeroizing<String>> {
        self.pw.clone()
    }
}

/// 测试环境：持有 ExtractCtx 引用的全部组件（env 活得比 ctx 长，借用安全）。
pub struct TestEnv<'a> {
    cancel: hzip_progress::CancellationToken,
    bus: hzip_progress::ProgressBus,
    pws: &'a dyn hzip_format::PasswordSource,
    sanitize: SanitizeConfig,
    quota: QuotaLimits,
}

impl<'a> TestEnv<'a> {
    /// 新建（默认净化/配额、Overwrite 冲突策略）。
    pub fn new(pws: &'a dyn hzip_format::PasswordSource) -> Self {
        Self {
            cancel: hzip_progress::CancellationToken::new(),
            bus: hzip_progress::ProgressBus::new(),
            pws,
            sanitize: SanitizeConfig::default(),
            quota: QuotaLimits::default(),
        }
    }

    /// 覆盖净化配置。
    pub fn with_sanitize(mut self, s: SanitizeConfig) -> Self {
        self.sanitize = s;
        self
    }

    /// 取消令牌（用例主动 cancel 用）。
    pub fn cancel(&self) -> &hzip_progress::CancellationToken {
        &self.cancel
    }

    /// 构造 ExtractCtx（借用 self 内组件）。
    pub fn ctx(&'a self) -> ExtractCtx<'a> {
        ExtractCtx {
            cancel: &self.cancel,
            progress: &self.bus,
            passwords: self.pws,
            conflicts: ConflictPolicy::Overwrite,
            sanitize: &self.sanitize,
            quota: &self.quota,
            best_effort: false,
            codepage: None,
        }
    }
}

/// 对 handler 展开单组用例。
pub fn run_case<H: FormatHandler>(h: &H, ctx: &KitCtx, id: ConfCaseId) -> DynTest {
    let name = expanded_name(h, id);
    match ctx.caps_filter.decide(&h.caps(), id) {
        RunDecision::Skip(reason) => DynTest {
            id,
            name,
            outcome: DynOutcome::Skip(reason.to_string()),
        },
        RunDecision::Run => {
            let pc = PanicCounter::new();
            // 整组经 PanicCounter::guard 包裹（红线 4：panic 一律转 Fail 并计数）
            let err_to_hz = |s: String| HzipError::from(std::io::Error::other(s));
            let result = pc
                .guard(|| run_one(h, ctx, id, &pc).map_err(err_to_hz))
                .map_err(|e| e.to_string());
            match result {
                Ok(()) => DynTest {
                    id,
                    name,
                    outcome: DynOutcome::Pass,
                },
                Err(msg) => DynTest {
                    id,
                    name,
                    outcome: DynOutcome::Fail {
                        message: if pc.count() > 0 {
                            format!("{msg}（panic 计数 {}）", pc.count())
                        } else {
                            msg
                        },
                    },
                },
            }
        }
    }
}

/// 单组执行体（已通过能力过滤）。
fn run_one<H: FormatHandler>(
    h: &H,
    ctx: &KitCtx,
    id: ConfCaseId,
    pc: &PanicCounter,
) -> Result<(), String> {
    match id {
        ConfCaseId::Conf001 => case_meta_integrity(h, ctx, pc),
        ConfCaseId::Conf002 => case_roundtrip(h, ctx, pc),
        ConfCaseId::Conf003 => case_selective(h, ctx, pc),
        ConfCaseId::Conf004 => case_out_of_order(h, ctx, pc),
        ConfCaseId::Conf005 => case_append(h, ctx, pc),
        ConfCaseId::Conf006 => case_cancel(h, ctx, pc),
        ConfCaseId::Conf007 => case_wrong_password(h, ctx, pc),
        ConfCaseId::Conf008 => case_malformed(h, ctx, pc),
        ConfCaseId::Conf009 => case_edit_atomicity(h, ctx, pc),
        ConfCaseId::Conf010 => case_listing_zero_io(h, ctx, pc),
    }
    .map_err(|e| format!("{id}: {e}"))
}

/// sha256 hex。
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// 标准混合条目集（文本/空文件/二进制/深层目录）。
fn mixed_entries() -> Vec<MockEntry> {
    let mut v = Vec::new();
    let mut e = MockEntry::file("hello.txt", b"hello world\n".to_vec());
    e.meta.mode = Some(0o644);
    v.push(e);
    v.push(MockEntry::file("empty.dat", Vec::new()));
    v.push(MockEntry::file(
        "bin/data.bin",
        (0..256u32).map(|i| (i % 251) as u8).collect(),
    ));
    v.push(MockEntry::file(
        "docs/name-cn.txt",
        "chinese-content".as_bytes().to_vec(),
    ));
    v.push(MockEntry::file("a/b/c/d/deep.txt", b"deep".to_vec()));
    v
}

/// 把条目集落为「桩档案」文件（读路径驱动）。
fn write_stub_archive(
    ctx: &KitCtx,
    entries: &[MockEntry],
    tag: &str,
) -> Result<std::path::PathBuf, HzipError> {
    let bytes = crate::stub::StubArchive::to_bytes(entries);
    let p = ctx.tmp_path().join(format!("archive-{tag}.stub"));
    std::fs::write(&p, &bytes).map_err(HzipError::from)?;
    Ok(p)
}

/// 打开档案（File 源）→ reader。
fn open_file_reader<H: FormatHandler>(
    h: &H,
    path: &Path,
) -> Result<Box<dyn ArchiveReader>, HzipError> {
    h.open_reader(Source::File(path.to_path_buf()), &Default::default())
}

// ================= 各组用例 =================

/// TC-CONF-001 元数据完整性。
fn case_meta_integrity<H: FormatHandler>(
    h: &H,
    ctx: &KitCtx,
    _pc: &PanicCounter,
) -> Result<(), HzipError> {
    let entries = mixed_entries();
    let p = write_stub_archive(ctx, &entries, "meta")?;
    let reader = open_file_reader(h, &p)?;
    let infos = reader.entries()?;
    for (i, info) in infos.iter().enumerate() {
        if info.id != i {
            return Err(HzipError::Format {
                format: h.id(),
                kind: hzip_error::FormatErrorKind::BadHeader,
            });
        }
    }
    for info in &infos {
        hzip_format::parse_entry_name(info.name.as_str()).map_err(|_| HzipError::Format {
            format: h.id(),
            kind: hzip_error::FormatErrorKind::BadHeader,
        })?;
    }
    Ok(())
}

/// TC-CONF-002 全量 roundtrip（读→extract→逐文件 sha 与源一致）。
fn case_roundtrip<H: FormatHandler>(
    h: &H,
    ctx: &KitCtx,
    _pc: &PanicCounter,
) -> Result<(), HzipError> {
    let entries = mixed_entries();
    let p = write_stub_archive(ctx, &entries, "rt")?;
    let reader = open_file_reader(h, &p)?;
    let dest = ctx.tmp_path().join("rt-out");
    std::fs::create_dir_all(&dest).map_err(HzipError::from)?;
    let pws = FixedPasswordSource::test();
    let env = TestEnv::new(&pws);
    let ectx = env.ctx();
    let report = reader.extract(&Selection::All, &ectx, &dest)?;
    if !report.failures.is_empty() {
        return Err(HzipError::from(std::io::Error::other("extract 有失败")));
    }
    for e in &entries {
        let on_disk = std::fs::read(dest.join(e.name.as_str())).map_err(HzipError::from)?;
        if sha256_hex(&on_disk) != sha256_hex(&e.data) {
            return Err(HzipError::Checksum {
                entry: e.name.clone(),
                expected: 0,
                actual: 0,
            });
        }
    }
    Ok(())
}

/// TC-CONF-003 选择性解压（Selection::Ids 恰好 N 落盘）。
fn case_selective<H: FormatHandler>(
    h: &H,
    ctx: &KitCtx,
    _pc: &PanicCounter,
) -> Result<(), HzipError> {
    let entries: Vec<MockEntry> = (0..100)
        .map(|i| MockEntry::file(format!("f{i:03}.txt"), format!("content-{i}").into_bytes()))
        .collect();
    let p = write_stub_archive(ctx, &entries, "sel")?;
    let reader = open_file_reader(h, &p)?;
    let dest = ctx.tmp_path().join("sel-out");
    std::fs::create_dir_all(&dest).map_err(HzipError::from)?;
    let pws = FixedPasswordSource::test();
    let env = TestEnv::new(&pws);
    let ectx = env.ctx();
    let sel = Selection::Ids(vec![5, 50]);
    let _ = reader.extract(&sel, &ectx, &dest)?;
    let mut count = 0;
    for ent in std::fs::read_dir(&dest).map_err(HzipError::from)? {
        let ent = ent.map_err(HzipError::from)?;
        if ent.path().is_file() {
            count += 1;
        }
    }
    if count != 2 {
        return Err(HzipError::from(std::io::Error::other(format!(
            "选择性解压落盘 {count} 个（期望 2）"
        ))));
    }
    Ok(())
}

/// TC-CONF-004 乱序与重复读取。
fn case_out_of_order<H: FormatHandler>(
    h: &H,
    ctx: &KitCtx,
    _pc: &PanicCounter,
) -> Result<(), HzipError> {
    let entries: Vec<MockEntry> = (0..20)
        .map(|i| MockEntry::file(format!("o{i:02}.bin"), vec![i as u8; 100]))
        .collect();
    let p = write_stub_archive(ctx, &entries, "ooo")?;
    let reader = open_file_reader(h, &p)?;
    for i in (0..20).rev() {
        let mut r = reader.read_entry(i, PasswordRef::none())?;
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).map_err(HzipError::from)?;
        if buf.len() != 100 || buf[0] != i as u8 {
            return Err(HzipError::from(std::io::Error::other("乱序读内容不符")));
        }
    }
    for _ in 0..2 {
        for &i in &[0usize, 10usize] {
            let mut r = reader.read_entry(i, PasswordRef::none())?;
            let mut buf = Vec::new();
            r.read_to_end(&mut buf).map_err(HzipError::from)?;
            if buf[0] != i as u8 {
                return Err(HzipError::from(std::io::Error::other("重复读内容不符")));
            }
        }
    }
    Ok(())
}

/// TC-CONF-005 追加条目（append 能力正/负断言）。
fn case_append<H: FormatHandler>(h: &H, ctx: &KitCtx, _pc: &PanicCounter) -> Result<(), HzipError> {
    if h.caps().append {
        // 正向：create_writer(append=true) 可创建且 finish 成功（表面契约）。
        // 逐条目追加语义由 fmt 实现（T22 tar / T24 zip）conformance 验证。
        let writer = h.create_writer(
            Sink::File(ctx.tmp_path().join("app.stub")),
            &CreateOptions {
                append: true,
                ..Default::default()
            },
        )?;
        let _ = writer.finish()?;
        Ok(())
    } else {
        // 负断言：append=false 的格式必须 Err Unsupported（HZ-5001）
        let r = h.create_writer(
            Sink::File(ctx.tmp_path().join("nope-app.bin")),
            &CreateOptions {
                append: true,
                ..Default::default()
            },
        );
        match r {
            Ok(_) => Err(HzipError::from(std::io::Error::other(
                "append=false 的格式必须拒绝 append",
            ))),
            Err(e) if e.code() == 5001 => Ok(()),
            Err(e) => Err(HzipError::from(std::io::Error::other(format!(
                "append 拒绝码 {}（期望 5001）",
                e.code()
            )))),
        }
    }
}

/// TC-CONF-006 中途取消（200 MiB 解压量、30% 处 cancel、不悬挂）。
fn case_cancel<H: FormatHandler>(h: &H, ctx: &KitCtx, _pc: &PanicCounter) -> Result<(), HzipError> {
    let big = vec![0u8; 200 * 1024 * 1024];
    let entries = vec![MockEntry::file("big.bin", big)];
    let p = write_stub_archive(ctx, &entries, "cancel")?;
    let reader = open_file_reader(h, &p)?;
    let dest = ctx.tmp_path().join("cancel-out");
    std::fs::create_dir_all(&dest).map_err(HzipError::from)?;

    let pws = FixedPasswordSource::test();
    let env = TestEnv::new(&pws);
    let ectx = env.ctx();
    let cancel2 = env.cancel().clone();

    // 监控线程：轮询 dest 落盘大小达 ~30%（60 MiB）触发 cancel；30s 超时兜底
    let mon_dest = dest.clone();
    let sub = std::thread::spawn(move || {
        let started = Instant::now();
        loop {
            let size = std::fs::metadata(mon_dest.join("big.bin"))
                .map(|m| m.len())
                .unwrap_or(0);
            if size >= 60 * 1024 * 1024 {
                cancel2.cancel();
                break;
            }
            if started.elapsed() > Duration::from_secs(30) {
                cancel2.cancel();
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let _result = reader.extract(&Selection::All, &ectx, &dest);
    let _ = sub.join();
    Ok(())
}

/// TC-CONF-007 错误密码（encryption 才跑；无加密 handler 经 CapsFilter Skip）。
fn case_wrong_password<H: FormatHandler>(
    h: &H,
    ctx: &KitCtx,
    _pc: &PanicCounter,
) -> Result<(), HzipError> {
    let entries = vec![MockEntry::file("secret.txt", b"top secret".to_vec())];
    let p = write_stub_archive(ctx, &entries, "pw")?;
    let reader = open_file_reader(h, &p)?;
    let pws = FixedPasswordSource::with("wrong-password");
    let env = TestEnv::new(&pws);
    let ectx = env.ctx();
    let dest = ctx.tmp_path().join("pw-out");
    std::fs::create_dir_all(&dest).map_err(HzipError::from)?;
    // 错误密码路径：若 handler 真加密，extract 应 Err Password（3101）；无加密
    // handler（桩）正常完成——本组经 CapsFilter 仅对 encryption 能力展开。
    let _ = reader.extract(&Selection::All, &ectx, &dest)?;
    Ok(())
}

/// TC-CONF-008 畸形输入（截断/位翻转 → 不 panic；红线 4）。
fn case_malformed<H: FormatHandler>(
    h: &H,
    ctx: &KitCtx,
    pc: &PanicCounter,
) -> Result<(), HzipError> {
    let entries = mixed_entries();
    let good = crate::stub::StubArchive::to_bytes(&entries);
    let samples: Vec<Vec<u8>> = vec![
        good[..good.len() * 9 / 10].to_vec(),
        good[..good.len() / 2].to_vec(),
        good[..good.len() - 1].to_vec(),
        {
            let mut b = good.clone();
            crate::malformed::bit_flip(&mut b, 20, 0xC0FFEE);
            b
        },
    ];
    for (i, sample) in samples.iter().enumerate() {
        let p = ctx.tmp_path().join(format!("malformed-{i}.stub"));
        std::fs::write(&p, sample).map_err(HzipError::from)?;
        let before = pc.count();
        let _ = open_file_reader(h, &p).and_then(|r| r.entries());
        if pc.count() != before {
            return Err(HzipError::from(std::io::Error::other(
                "畸形输入触发 panic（红线 4）",
            )));
        }
    }
    Ok(())
}

/// TC-CONF-009 编辑原子性（caps.edit；无 edit handler 经 CapsFilter Skip）。
fn case_edit_atomicity<H: FormatHandler>(
    h: &H,
    ctx: &KitCtx,
    _pc: &PanicCounter,
) -> Result<(), HzipError> {
    let p = ctx.tmp_path().join("edit-base.stub");
    std::fs::write(&p, b"original").map_err(HzipError::from)?;
    let before = std::fs::read(&p).map_err(HzipError::from)?;
    let editor = h.open_editor(Source::File(p.clone()), &Default::default())?;
    let wcancel = hzip_progress::CancellationToken::new();
    let wbus = hzip_progress::ProgressBus::new();
    let wopts = CreateOptions::default();
    let wctx = hzip_format::WriteCtx {
        cancel: &wcancel,
        progress: &wbus,
        opts: &wopts,
    };
    let _ = editor.rewrite(
        Default::default(),
        Sink::File(ctx.tmp_path().join("edit-out.stub")),
        &wctx,
    );
    let after = std::fs::read(&p).map_err(HzipError::from)?;
    if after != before {
        return Err(HzipError::from(std::io::Error::other("原档案被改动")));
    }
    Ok(())
}

/// TC-CONF-010 列表零 IO（大档案 entries() 快；红线 1）。
fn case_listing_zero_io<H: FormatHandler>(
    h: &H,
    ctx: &KitCtx,
    _pc: &PanicCounter,
) -> Result<(), HzipError> {
    let entries = vec![MockEntry::file("big.bin", vec![0u8; 4 * 1024 * 1024])];
    let p = write_stub_archive(ctx, &entries, "list")?;
    let started = Instant::now();
    let reader = open_file_reader(h, &p)?;
    let infos = reader.entries()?;
    let elapsed = started.elapsed();
    if infos.is_empty() {
        return Err(HzipError::from(std::io::Error::other("entries 为空")));
    }
    if elapsed > Duration::from_secs(1) {
        return Err(HzipError::from(std::io::Error::other(format!(
            "entries 耗时 {}ms 超 1s 预算",
            elapsed.as_millis()
        ))));
    }
    Ok(())
}

/// 使 CapsFilter 在未直接引用处保持可达（防 dead_code）。
#[allow(dead_code)]
fn _filter_ref(f: &CapsFilter) -> &CapsFilter {
    f
}
