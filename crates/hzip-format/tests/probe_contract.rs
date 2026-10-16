//! 探针契约（T-IMG1）：内容探针（`FormatHandler::probe` / `probe_head_bytes`）、
//! `Confidence::Medium` 档位与强度序、`FormatHandler::supports_in_place_edit`、
//! `CreateOptions::volume_write` 默认值、`ArchiveEditor::write_in_place` 默认拒绝。
//!
//! 两件事同时钉死：
//!
//! 1. **新档位可用**——无魔数格式（raw 镜像 / 裸文件系统卷）靠内容启发式被识别，
//!    置信度落在 `Medium`（强于扩展名回退、弱于魔数）；
//! 2. **既有格式零变化**——`probe`/`probe_head_bytes`/`write_in_place` 全为默认实现，
//!    既有 handler 一行不改即不参与探测、不支持就地写。
//!
//! 本 crate 依赖红线禁止依赖任何 `hzip-fmt-*`（02 §2.1 红线 3），故「既有格式」
//! 的默认行为以**不覆写这三个方法**的本地 handler 验证（同一代码路径）。
#![allow(clippy::unwrap_used, clippy::expect_used)] // 测试代码：断言即意图

use hzip_error::HzipError;
use hzip_format::caps::{FormatCaps, VolumeWriteMode};
use hzip_format::{
    ArchiveEditor, ChangeSet, Confidence, CreateOptions, FormatHandler, FormatId, FormatRegistry,
    OpenOptions, Signature, Sniffed, Source, WriteCtx,
};

/// `Unsupported` 错误码（HZ-5001）。
const UNSUPPORTED: u32 = 5001;
/// `BadSignature` 错误码（HZ-2001）。
const BAD_SIGNATURE: u32 = 2001;

/// 写临时文件并返回路径。
fn write_tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("hzip-t-img1-probe");
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write tmp");
    p
}

/// 断言嗅探命中 `Single(id, conf)`（`HzipError` 未实现 `PartialEq`，故逐项比）。
#[track_caller]
fn assert_single(res: Result<Sniffed, HzipError>, id: &str, conf: Confidence) {
    match res {
        Ok(Sniffed::Single(got, c)) => {
            assert_eq!(got.as_str(), id, "格式 id");
            assert_eq!(c, conf, "置信度");
        }
        other => panic!("应命中 Single({id}, {conf:?})，实际 {other:?}"),
    }
}

/// 占位错误（测试路径不触达，仅满足 trait 签名）。
fn unsupported(what: &'static str) -> HzipError {
    HzipError::Unsupported { what: what.into() }
}

/// 无魔数、只有内容探针的假格式（模拟 raw 镜像 / 裸文件系统卷）。
struct FakeRaw;

impl FormatHandler for FakeRaw {
    fn id(&self) -> FormatId {
        FormatId::from("fakeraw")
    }
    fn signature(&self) -> &'static Signature {
        static S: Signature = Signature {
            magic: b"",
            offset: 0,
            display: "fake raw",
            extensions: &["fakeraw"],
        };
        &S
    }
    fn caps(&self) -> FormatCaps {
        FormatCaps {
            read: true,
            ..Default::default()
        }
    }
    fn probe(&self, head: &[u8]) -> Option<Confidence> {
        (head.len() >= 2048 && head[..2] == [0x53, 0xEF]).then_some(Confidence::Medium)
    }
    fn probe_head_bytes(&self) -> usize {
        2048
    }
    fn open_reader(
        &self,
        _s: Source,
        _o: &OpenOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveReader>, HzipError> {
        Err(unsupported("fake"))
    }
    fn create_writer(
        &self,
        _d: hzip_format::Sink,
        _o: &CreateOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveWriter>, HzipError> {
        Err(unsupported("fake"))
    }
    fn open_editor(
        &self,
        _s: Source,
        _o: &OpenOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveEditor>, HzipError> {
        Err(unsupported("fake"))
    }
}

/// 覆写 `probe` 的极简假格式（固定置信度；窗口取默认 512）。
struct Claim(&'static str, Confidence);

impl FormatHandler for Claim {
    fn id(&self) -> FormatId {
        FormatId::from(self.0)
    }
    fn signature(&self) -> &'static Signature {
        static S: Signature = Signature {
            magic: b"",
            offset: 0,
            display: "claim",
            extensions: &[],
        };
        &S
    }
    fn caps(&self) -> FormatCaps {
        FormatCaps {
            read: true,
            ..Default::default()
        }
    }
    fn probe(&self, head: &[u8]) -> Option<Confidence> {
        (!head.is_empty()).then_some(self.1)
    }
    fn open_reader(
        &self,
        _s: Source,
        _o: &OpenOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveReader>, HzipError> {
        Err(unsupported("claim"))
    }
    fn create_writer(
        &self,
        _d: hzip_format::Sink,
        _o: &CreateOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveWriter>, HzipError> {
        Err(unsupported("claim"))
    }
    fn open_editor(
        &self,
        _s: Source,
        _o: &OpenOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveEditor>, HzipError> {
        Err(unsupported("claim"))
    }
}

/// 声明超长探针窗口的假格式（验证窗口被钳到 `PROBE_HEAD_MAX`）。
struct Huge;

impl FormatHandler for Huge {
    fn id(&self) -> FormatId {
        FormatId::from("huge")
    }
    fn signature(&self) -> &'static Signature {
        static S: Signature = Signature {
            magic: b"",
            offset: 0,
            display: "huge",
            extensions: &[],
        };
        &S
    }
    fn caps(&self) -> FormatCaps {
        FormatCaps {
            read: true,
            ..Default::default()
        }
    }
    fn probe(&self, head: &[u8]) -> Option<Confidence> {
        (head.len() == 4096 && head[0] == 0x5A).then_some(Confidence::Medium)
    }
    fn probe_head_bytes(&self) -> usize {
        usize::MAX // 未钳制 ⇒ read_head 会尝试分配 usize::MAX 字节
    }
    fn open_reader(
        &self,
        _s: Source,
        _o: &OpenOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveReader>, HzipError> {
        Err(unsupported("huge"))
    }
    fn create_writer(
        &self,
        _d: hzip_format::Sink,
        _o: &CreateOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveWriter>, HzipError> {
        Err(unsupported("huge"))
    }
    fn open_editor(
        &self,
        _s: Source,
        _o: &OpenOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveEditor>, HzipError> {
        Err(unsupported("huge"))
    }
}

/// 不覆写 `probe`/`probe_head_bytes` 的 handler：既有 8 个 fmt handler 的形态。
struct Plain;

impl FormatHandler for Plain {
    fn id(&self) -> FormatId {
        FormatId::from("plain")
    }
    fn signature(&self) -> &'static Signature {
        static S: Signature = Signature {
            magic: b"",
            offset: 0,
            display: "plain",
            extensions: &[],
        };
        &S
    }
    fn caps(&self) -> FormatCaps {
        FormatCaps {
            read: true,
            ..Default::default()
        }
    }
    fn open_reader(
        &self,
        _s: Source,
        _o: &OpenOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveReader>, HzipError> {
        Err(unsupported("plain"))
    }
    fn create_writer(
        &self,
        _d: hzip_format::Sink,
        _o: &CreateOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveWriter>, HzipError> {
        Err(unsupported("plain"))
    }
    fn open_editor(
        &self,
        _s: Source,
        _o: &OpenOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveEditor>, HzipError> {
        Err(unsupported("plain"))
    }
}

/// 只实现 `rewrite` 的编辑器：验证 `write_in_place` 的默认实现。
struct PlainEditor;

impl ArchiveEditor for PlainEditor {
    fn rewrite(
        &self,
        _changes: ChangeSet,
        _dst: hzip_format::Sink,
        _ctx: &WriteCtx<'_>,
    ) -> Result<hzip_format::WriteReport, HzipError> {
        Err(unsupported("plain-rewrite"))
    }
}

/// 写上下文（与各 fmt crate 测试同型的持有组件）。
struct Ctx {
    cancel: hzip_progress::CancellationToken,
    bus: hzip_progress::ProgressBus,
    opts: CreateOptions,
}

impl Ctx {
    fn new() -> Self {
        Self {
            cancel: Default::default(),
            bus: Default::default(),
            opts: CreateOptions::default(),
        }
    }
    fn ctx(&self) -> WriteCtx<'_> {
        WriteCtx {
            cancel: &self.cancel,
            progress: &self.bus,
            opts: &self.opts,
        }
    }
}

/// 注册任意 handler 的注册表（注册序即迭代序）。
fn reg_with(handlers: Vec<Box<dyn FormatHandler>>) -> FormatRegistry {
    let mut r = FormatRegistry::new();
    for h in handlers {
        r.register_handler(h).expect("注册");
    }
    r
}

/// 只注册 [`FakeRaw`] 的注册表。
fn reg() -> FormatRegistry {
    reg_with(vec![Box::new(FakeRaw)])
}

#[test]
fn probe_hits_with_medium_confidence() {
    let mut head = vec![0u8; 2048];
    head[..2].copy_from_slice(&[0x53, 0xEF]); // ext 魔数（故意不是 fakeraw 的扩展名）
    let p = write_tmp("blob.bin", &head);
    assert_single(reg().sniff(&Source::File(p)), "fakeraw", Confidence::Medium);
}

#[test]
fn probe_miss_falls_back_to_extension_low() {
    let p = write_tmp("a.fakeraw", b"nothing-to-see"); // 探针不认 ⇒ 扩展名回退
    assert_single(reg().sniff(&Source::File(p)), "fakeraw", Confidence::Low);
}

#[test]
fn magic_beats_probe() {
    let p = write_tmp("p.zip", b"PK\x03\x04rest"); // 真 zip：魔数档先行，探针不得改写
    assert_single(reg().sniff(&Source::File(p)), "zip", Confidence::High);
}

#[test]
fn probe_head_bytes_is_honoured() {
    // 探针要 2048 字节：源只有 1024 ⇒ 不命中（落到扩展名），证明窗口按声明给
    let short: Vec<u8> = [0x53, 0xEF].iter().cycle().take(1024).copied().collect();
    let p = write_tmp("short.fakeraw", &short);
    assert_single(reg().sniff(&Source::File(p)), "fakeraw", Confidence::Low);
}

#[test]
fn probe_head_bytes_is_clamped_to_max() {
    // 声明 usize::MAX 的窗口被钳到 PROBE_HEAD_MAX(4096)：能命中、且不尝试天文数字分配
    let huge = [vec![0x5Au8], vec![0u8; 4095]].concat();
    let p = write_tmp("huge.bin", &huge);
    assert_single(
        reg_with(vec![Box::new(Huge)]).sniff(&Source::File(p)),
        "huge",
        Confidence::Medium,
    );
}

#[test]
fn strongest_probe_wins() {
    // 两个探针都说「是我的」：置信度高者胜（与注册序无关）
    let r = reg_with(vec![
        Box::new(Claim("weak", Confidence::Medium)),
        Box::new(Claim("strong", Confidence::High)),
    ]);
    let p = write_tmp("who.bin", &[0xABu8; 64]);
    assert_single(r.sniff(&Source::File(p)), "strong", Confidence::High);
}

#[test]
fn tie_keeps_first_registered() {
    // 并列 ⇒ 注册序先者（`probe_sweep` 用 `>` 而非 `>=`）
    let r = reg_with(vec![
        Box::new(Claim("first", Confidence::Medium)),
        Box::new(Claim("second", Confidence::Medium)),
    ]);
    let p = write_tmp("tie.bin", &[0xCDu8; 64]);
    assert_single(r.sniff(&Source::File(p)), "first", Confidence::Medium);
}

#[test]
fn confidence_orders_high_medium_low() {
    // probe_sweep 以 `>` 比较置信度 ⇒ 强度序必须是 `High > Medium > Low`
    assert!(Confidence::High > Confidence::Medium);
    assert!(Confidence::Medium > Confidence::Low);
}

#[test]
fn handlers_iterates_registration_order() {
    // 探针扫描依赖的只读迭代器：注册序、不给可变访问
    let r = reg_with(vec![
        Box::new(FakeRaw),
        Box::new(Claim("claim", Confidence::Low)),
    ]);
    let ids: Vec<String> = r.handlers().map(|h| h.id().to_string()).collect();
    assert_eq!(ids, ["fakeraw", "claim"]);
}

#[test]
fn default_probe_is_absent_and_in_place_is_unsupported() {
    // 既有格式（zip 等 8 个 handler）未实现 probe ⇒ 行为零变化
    let h = Plain;
    assert!(
        h.probe(&vec![0u8; 4096]).is_none(),
        "默认探针必须恒 None（既有格式零成本）"
    );
    assert_eq!(h.probe_head_bytes(), 512, "默认窗口 = 魔数嗅探同一次读");
    assert!(
        !h.supports_in_place_edit(),
        "默认「可就地编辑」必须为 false（既有格式零改动）"
    );

    // 未实现 write_in_place ⇒ Unsupported（core 据此回落 rewrite）
    let ctx = Ctx::new();
    let err = PlainEditor
        .write_in_place(ChangeSet::default(), &ctx.ctx())
        .expect_err("默认必须拒绝就地写");
    assert!(
        matches!(err, HzipError::Unsupported { .. }),
        "应为 Unsupported，实际 {err:?}"
    );
    assert_eq!(err.code(), UNSUPPORTED);

    // 不实现探针的 handler 注册进去也不改变嗅探结果（默认 None 短路）
    let p = write_tmp("plain.unknown-ext", b"nothing-to-see");
    let err = reg_with(vec![Box::new(Plain)])
        .sniff(&Source::File(p))
        .expect_err("未注册扩展名 + 无探针 ⇒ 仍报未知");
    assert_eq!(err.code(), BAD_SIGNATURE);
}

#[test]
fn defaults_are_overlay_and_not_in_place() {
    assert_eq!(VolumeWriteMode::default(), VolumeWriteMode::Overlay);
    assert_eq!(
        CreateOptions::default().volume_write,
        VolumeWriteMode::Overlay
    );
    assert!(
        !Plain.supports_in_place_edit(),
        "既有 handler 默认不可就地写"
    );
    // 「可就地编辑」是**行为声明**（有没有 write_in_place 实现），不是 caps 数据位：
    // 落成 caps 字段需要全仓 60 处字面量连锁补位（见 handler.rs 的 doc 说明）。
    let declared = Plain.supports_in_place_edit();
    let ctx = Ctx::new();
    let refused = PlainEditor
        .write_in_place(ChangeSet::default(), &ctx.ctx())
        .is_err();
    if declared {
        assert!(!refused, "声明能就地写，就必须真的能写");
    } else {
        assert!(
            refused,
            "声明不可就地写，就必须拒绝（core 据此回落 rewrite）"
        );
    }
}
