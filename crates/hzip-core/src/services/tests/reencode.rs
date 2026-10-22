//! reencode 助手单测（T-IMG0）：编码↔解码往返 / tar 条目替换 / 失败与取消清理。
//!
//! 这三件助手只吃 [`crate::jobs::JobCtx`] 的 `registry`/`cancel`/`progress` 三个
//! 公开面，而 `JobCtx` 的通道字段是 `jobs::manager` 私有、仓库里没有测试构造器
//! ——故测试直造 `HelperCtx`（见其 rustdoc），加上一个**编译期**签名校验用例锁住
//! 公开入口的逐字签名（主线程按那份签名调用）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

use std::path::PathBuf;

use time::OffsetDateTime;

/// 测试用 ctx 三件套（`HelperCtx` 的构造件）。
struct TestCtx {
    registry: FormatRegistry,
    cancel: CancellationToken,
    progress: ProgressBus,
}

impl TestCtx {
    /// 真注册表（内置 handler 全量挂载，与 `compress.rs::verify_reopen` 同法）。
    fn new() -> Self {
        let mut registry = FormatRegistry::new();
        crate::engine::register_builtin_handlers(&mut registry).unwrap();
        Self {
            registry,
            cancel: CancellationToken::new(),
            progress: ProgressBus::new(),
        }
    }

    fn ctx(&self) -> HelperCtx<'_> {
        HelperCtx {
            registry: &self.registry,
            cancel: &self.cancel,
            progress: &self.progress,
        }
    }
}

// ===================== 夹具 =====================

/// 「两极」负载：前 1/3 全零（压缩友好）+ 其余伪随机（不可压）——压缩率与
/// 流式拷贝两条路都要踩到。
fn mixed_payload(len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for b in v.iter_mut().skip(len / 3) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x >> 24) as u8;
    }
    v
}

/// 普通条目元数据（大小/时间/权限位显式给定——元数据保真的断言基准）。
fn tar_meta(name: &str, size: u64, mtime: i64, mode: u32) -> EntryMeta {
    EntryMeta {
        name: name.into(),
        size_raw: Some(size),
        mtime: Some(OffsetDateTime::from_unix_timestamp(mtime).unwrap()),
        mode: Some(mode),
        ..Default::default()
    }
}

/// 造普通 tar（走 registry 的 tar handler 真实写出面）。
fn make_tar(reg: &FormatRegistry, path: &Path, entries: &[(EntryMeta, Vec<u8>)]) {
    let h = reg.handler("tar").unwrap();
    let mut w = h
        .create_writer(Sink::File(path.to_path_buf()), &CreateOptions::default())
        .unwrap();
    for (meta, data) in entries {
        let mut sink = w.next_entry(meta, PasswordRef::none()).unwrap();
        sink.write_all(data).unwrap();
        sink.finish().unwrap();
    }
    w.finish().unwrap();
}

/// 读回 tar：条目信息 + 逐条目字节（原序）。
fn read_tar(reg: &FormatRegistry, path: &Path) -> (Vec<EntryInfo>, Vec<Vec<u8>>) {
    let h = reg.handler("tar").unwrap();
    let r = h
        .open_reader(Source::File(path.to_path_buf()), &OpenOptions::default())
        .unwrap();
    let entries = r.entries().unwrap();
    let mut datas = Vec::new();
    for e in &entries {
        let mut buf = Vec::new();
        r.read_entry(e.id, PasswordRef::none())
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        datas.push(buf);
    }
    (entries, datas)
}

// ===================== 1. 编解码往返 =====================

/// gz 往返：空文件与 3 MiB 混合字节，解回来的字节必须逐字相等。
#[test]
fn encode_then_decode_gz_roundtrip_bytes_equal() {
    let t = TestCtx::new();
    let dir = tempfile::tempdir().unwrap();
    for (name, len) in [("empty.bin", 0usize), ("mixed.bin", 3 * 1024 * 1024)] {
        let payload = mixed_payload(len);
        let src = dir.path().join(name);
        std::fs::write(&src, &payload).unwrap();
        let gz = dir.path().join(format!("{name}.gz"));
        let back = dir.path().join(format!("{name}.back"));

        let rep = encode_file_impl(&t.ctx(), "gz", &src, &gz).unwrap();
        assert_eq!(rep.bytes_in, len as u64, "编码输入字节 = 源长（{name}）");
        assert!(rep.bytes_out > 0, "编码产物非空（{name}）");
        assert_eq!(rep.entries_written, 1, "单流恰一条目（{name}）");

        decode_file_impl(&t.ctx(), "gz", &gz, &back).unwrap();
        assert_eq!(
            std::fs::read(&back).unwrap(),
            payload,
            "往返字节相等（{name}）"
        );
    }
}

// ===================== 2. tar 条目替换 =====================

/// 替换 `c.txt`：条目数/名字/顺序不变，未命中条目字节逐字相等，
/// 被替换条目 = 新内容且沿用原 mtime/mode。
#[test]
fn rebuild_tar_replacing_entry_keeps_order_bytes_and_metadata() {
    let t = TestCtx::new();
    let dir = tempfile::tempdir().unwrap();
    let tar = dir.path().join("outer.tar");
    let a = b"aaaa-keep-me".to_vec();
    let b = vec![7u8; 4096];
    let c_old = b"c-v1".to_vec();
    let (c_mtime, c_mode) = (1_700_000_000i64, 0o640u32);
    make_tar(
        &t.registry,
        &tar,
        &[
            (
                tar_meta("a.txt", a.len() as u64, 1_699_000_001, 0o644),
                a.clone(),
            ),
            (
                tar_meta("keep/dir/b.bin", b.len() as u64, 1_699_000_002, 0o600),
                b.clone(),
            ),
            (
                tar_meta("c.txt", c_old.len() as u64, c_mtime, c_mode),
                c_old,
            ),
        ],
    );

    let child = dir.path().join("child.bin");
    let new_c = b"c-v2-new-bytes-longer-than-v1".to_vec();
    std::fs::write(&child, &new_c).unwrap();
    let out = dir.path().join("rebuilt.tar");

    let rep = rebuild_tar_impl(&t.ctx(), &tar, "c.txt", &child, &out).unwrap();
    assert_eq!(rep.entries_written, 3);

    let (entries, datas) = read_tar(&t.registry, &out);
    assert_eq!(entries.len(), 3, "条目数不变");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        ["a.txt", "keep/dir/b.bin", "c.txt"],
        "名字与顺序不变"
    );
    assert_eq!(datas[0], a, "a.txt 字节逐字相等");
    assert_eq!(datas[1], b, "keep/dir/b.bin 字节逐字相等");
    assert_eq!(datas[2], new_c, "c.txt = 新内容");
    assert_eq!(
        entries[2].mtime.map(|m| m.unix_timestamp()),
        Some(c_mtime),
        "被替换条目 mtime 保留"
    );
    assert_eq!(entries[2].mode, Some(c_mode), "被替换条目 mode 保留");
}

/// 目录与符号链接条目按读侧信息直通（零数据；类型/目标/权限位不丢）。
#[test]
fn rebuild_tar_preserves_dir_and_symlink_entries() {
    let t = TestCtx::new();
    let dir = tempfile::tempdir().unwrap();
    let tar = dir.path().join("shapes.tar");
    let c = b"c-data".to_vec();
    make_tar(
        &t.registry,
        &tar,
        &[
            (
                EntryMeta {
                    is_dir: true,
                    ..tar_meta("keep/", 0, 1_699_000_003, 0o755)
                },
                Vec::new(),
            ),
            (
                EntryMeta {
                    is_symlink: true,
                    symlink_target: Some(PathBuf::from("a.txt")),
                    ..tar_meta("link", 0, 1_699_000_004, 0o777)
                },
                Vec::new(),
            ),
            (
                tar_meta("c.txt", c.len() as u64, 1_699_000_005, 0o644),
                c.clone(),
            ),
        ],
    );
    let child = dir.path().join("child.bin");
    std::fs::write(&child, b"c-v2").unwrap();
    let out = dir.path().join("shapes.rebuilt.tar");

    rebuild_tar_impl(&t.ctx(), &tar, "c.txt", &child, &out).unwrap();
    let (entries, datas) = read_tar(&t.registry, &out);
    assert_eq!(entries.len(), 3);
    assert!(entries[0].is_dir, "目录条目仍是目录");
    assert_eq!(entries[0].mode, Some(0o755), "目录权限位保留");
    assert!(entries[1].is_symlink, "链接条目仍是链接");
    assert_eq!(
        entries[1].symlink_target,
        Some(PathBuf::from("a.txt")),
        "链接目标保留"
    );
    assert!(
        datas[0].is_empty() && datas[1].is_empty(),
        "零数据条目不产内容"
    );
    assert_eq!(datas[2], b"c-v2", "普通条目被替换");
}

/// 条目不存在 → Err，且不留半写产物。
#[test]
fn rebuild_tar_missing_entry_errors_and_leaves_no_out() {
    let t = TestCtx::new();
    let dir = tempfile::tempdir().unwrap();
    let tar = dir.path().join("outer.tar");
    let c = b"c-data".to_vec();
    make_tar(
        &t.registry,
        &tar,
        &[(tar_meta("c.txt", c.len() as u64, 1_700_000_000, 0o644), c)],
    );
    let child = dir.path().join("child.bin");
    std::fs::write(&child, b"new").unwrap();
    let out = dir.path().join("nope.tar");

    let err = rebuild_tar_impl(&t.ctx(), &tar, "nope.txt", &child, &out).unwrap_err();
    assert!(
        matches!(err, HzipError::Unsupported { .. }),
        "条目不存在 → Err Unsupported: {err}"
    );
    assert!(err.to_string().contains("nope.txt"), "错误带条目名: {err}");
    assert!(!out.exists(), "条目不存在时不落半写产物");
}

// ===================== 3. 失败 / 取消清理 =====================

/// 取消令牌已置位 → `Cancelled`，且写了一半的 `out` 被删掉（两个写方向都验）。
#[test]
fn cancel_removes_half_written_out() {
    let t = TestCtx::new();
    let dir = tempfile::tempdir().unwrap();
    // 编码方向
    let src = dir.path().join("big.bin");
    std::fs::write(&src, mixed_payload(2 * 1024 * 1024)).unwrap();
    let gz = dir.path().join("big.bin.gz");
    t.cancel.cancel();
    let err = encode_file_impl(&t.ctx(), "gz", &src, &gz).unwrap_err();
    assert!(
        matches!(err, HzipError::Cancelled),
        "取消 → Cancelled: {err}"
    );
    assert!(!gz.exists(), "取消后不留半写产物");

    // tar 重建方向
    let tar = dir.path().join("outer.tar");
    // 夹具直写（registry 面：取消与本助手无关，写入器不看 ctx）
    make_tar(
        &t.registry,
        &tar,
        &[
            (tar_meta("a.txt", 4, 1_700_000_000, 0o644), b"aaaa".to_vec()),
            (tar_meta("c.txt", 4, 1_700_000_001, 0o644), b"cccc".to_vec()),
        ],
    );
    let child = dir.path().join("child.bin");
    std::fs::write(&child, b"new").unwrap();
    let out = dir.path().join("cancelled.tar");
    let err = rebuild_tar_impl(&t.ctx(), &tar, "c.txt", &child, &out).unwrap_err();
    assert!(
        matches!(err, HzipError::Cancelled),
        "取消 → Cancelled: {err}"
    );
    assert!(!out.exists(), "取消后不留半写产物");
}

/// 被替换条目的新内容文件缺失 → Err，且不留半写产物。
#[test]
fn rebuild_tar_missing_child_cleans_up_out() {
    let t = TestCtx::new();
    let dir = tempfile::tempdir().unwrap();
    let tar = dir.path().join("outer.tar");
    let a = b"aaaa-keep".to_vec();
    make_tar(
        &t.registry,
        &tar,
        &[
            (tar_meta("a.txt", a.len() as u64, 1_700_000_000, 0o644), a),
            (tar_meta("c.txt", 4, 1_700_000_001, 0o644), b"cccc".to_vec()),
        ],
    );
    let out = dir.path().join("should-not-exist.tar");
    let err = rebuild_tar_impl(
        &t.ctx(),
        &tar,
        "c.txt",
        &dir.path().join("no-such-child.bin"),
        &out,
    )
    .unwrap_err();
    assert!(matches!(err, HzipError::Io { .. }), "缺文件 → Io: {err}");
    assert!(!out.exists(), "失败后不留半写产物");
}

// ===================== 4. 契约签名（编译期） =====================

/// 公开入口的签名逐字校验：类型不符则**编译失败**（比运行时断言更早发现漂移）。
#[test]
fn helper_signatures_match_contract() {
    let _encode: fn(
        &crate::jobs::JobCtx,
        &str,
        &std::path::Path,
        &std::path::Path,
    ) -> Result<hzip_format::WriteReport, hzip_error::HzipError> = encode_file_with_codec;
    let _decode: fn(
        &crate::jobs::JobCtx,
        &str,
        &std::path::Path,
        &std::path::Path,
    ) -> Result<(), hzip_error::HzipError> = decode_file_with_codec;
    let _rebuild: fn(
        &crate::jobs::JobCtx,
        &std::path::Path,
        &str,
        &std::path::Path,
        &std::path::Path,
    ) -> Result<hzip_format::WriteReport, hzip_error::HzipError> = rebuild_tar_replacing_entry;
}
