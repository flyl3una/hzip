//! 设备链三态 + 源形态的**契约测试**（`Chain` 是写模式开关的全部实现）。
//!
//! 断言的都是**用户可观察的语义**，不是实现细节：
//! 覆盖写在提交前原文件不变、就地写立即生效、只读拒写且给理由、就地写失败降级为
//! 覆盖写（带提示）、流源只能整量重写、spool 随链释放而清理。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

use std::path::Path;

use fs_core::{BlockDevice as _, BlockRead as _};
use hzip_format::Source;
use hzip_progress::CancellationToken;
use hzip_vol::VolOptions;
use hzip_vol::device::{Chain, WriteMode};
use hzip_vol::error::{ReadOnlyReason, VolError};

/// 造一个 192 KiB 的 base 文件（3 个覆盖块），内容为 `fill`。
fn base_file(dir: &Path, name: &str, fill: u8) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, vec![fill; 3 * 64 * 1024]).unwrap();
    p
}

fn opts_in(dir: &Path) -> VolOptions {
    VolOptions {
        writable: true,
        cache_blocks: 0,
        spool: hzip_vol::SpoolPolicy {
            dir: dir.to_path_buf(),
            prefix: hzip_vol::materialize::SPOOL_PREFIX,
            max_bytes: None,
        },
        // 设备链测试不挂文件系统：空注册表 + 默认挂载选项
        ..VolOptions::default()
    }
}

#[test]
fn overlay_mode_leaves_the_source_untouched_until_commit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path(), "disk.img", 0xAA);
    let before = std::fs::read(&src).unwrap();

    let chain = Chain::open(
        Source::File(src.clone()),
        WriteMode::Overlay,
        &opts_in(tmp.path()),
        None,
    )
    .unwrap();
    assert_eq!(chain.dirty_blocks(), 0);

    // 写一个块（模拟文件系统落盘）
    chain
        .device()
        .write_at(64 * 1024, &vec![0x11; 4096])
        .unwrap();
    assert_eq!(chain.dirty_blocks(), 1, "写过的块应记脏");
    assert_eq!(
        std::fs::read(&src).unwrap(),
        before,
        "提交前原文件必须一字节不动"
    );

    let stats = chain.commit(None, None).unwrap();
    assert_eq!(stats.dirty_blocks, 1, "提交只回放脏块");
    assert_eq!(stats.mode, WriteMode::Overlay);
    let after = std::fs::read(&src).unwrap();
    assert_ne!(after, before, "提交后原文件已变");
    assert_eq!(&after[64 * 1024..64 * 1024 + 4096], &vec![0x11; 4096][..]);
    // 未脏块保持原内容
    assert_eq!(&after[..8], &vec![0xAA; 8][..]);
}

#[test]
fn in_place_mode_writes_immediately() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path(), "disk.img", 0x22);
    let chain = Chain::open(
        Source::File(src.clone()),
        WriteMode::InPlace,
        &opts_in(tmp.path()),
        None,
    )
    .unwrap();
    chain.device().write_at(0, b"direct").unwrap();
    let on_disk = std::fs::read(&src).unwrap();
    assert_eq!(&on_disk[..6], b"direct", "就地写应立即可见");
    assert_eq!(chain.dirty_blocks(), 0, "就地写不经覆盖层");
}

#[test]
fn read_only_mode_refuses_writes_and_reports_the_reason() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path(), "disk.img", 0x33);
    let before = std::fs::read(&src).unwrap();
    let chain = Chain::open(
        Source::File(src.clone()),
        WriteMode::ReadOnly,
        &opts_in(tmp.path()),
        None,
    )
    .unwrap();
    assert!(!chain.device().is_writable(), "只读链的设备必须自报不可写");
    let err = chain.device().write_at(0, b"nope").unwrap_err();
    assert!(
        matches!(err, fs_core::Error::ReadOnly),
        "只读设备应返回 ReadOnly：{err:?}"
    );
    assert_eq!(chain.read_only_reason(), Some(ReadOnlyReason::BySetting));
    assert_eq!(std::fs::read(&src).unwrap(), before);
}

#[test]
fn in_place_falls_back_to_overlay_when_the_file_is_not_writable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path(), "disk.img", 0x44);
    let mut perms = std::fs::metadata(&src).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&src, perms).unwrap();

    // 以 root 跑（或平台允许）时 r/w 仍可能成功 ⇒ 那时本用例退化为「就地写」分支。
    let chain = match Chain::open(
        Source::File(src.clone()),
        WriteMode::InPlace,
        &opts_in(tmp.path()),
        None,
    ) {
        Ok(c) => c,
        Err(e) => panic!("只读文件应降级而不是失败：{e:?}"),
    };
    if chain.mode() == WriteMode::InPlace {
        eprintln!("注意：本平台允许写只读文件（root？），跳过降级断言");
        return;
    }
    assert_eq!(chain.mode(), WriteMode::Overlay, "打不开 r/w ⇒ 降级覆盖写");
    assert!(
        chain.warnings().contains(&"vol.warn.inplace-fallback"),
        "降级必须留提示（不静默）：{:?}",
        chain.warnings()
    );
    // 降级后仍可写（走覆盖层），且原文件在提交前不变
    let before = std::fs::read(&src).unwrap();
    chain.device().write_at(0, b"zz").unwrap();
    assert_eq!(std::fs::read(&src).unwrap(), before);
}

#[test]
fn stream_source_is_spooled_and_can_only_be_streamed_back_out() {
    let tmp = tempfile::TempDir::new().unwrap();
    let payload = vec![0x5A; 3 * 64 * 1024];
    let src = Source::Stream(Box::new(std::io::Cursor::new(payload.clone())));

    let chain = Chain::open(src, WriteMode::Overlay, &opts_in(tmp.path()), None).unwrap();
    assert!(chain.is_spooled(), "流源必须 spool 成临时文件");
    assert_eq!(chain.device().size_bytes(), payload.len() as u64);
    assert!(
        chain.device().read_at(0, &mut [0u8; 1]).is_ok(),
        "spool 后的设备应可读"
    );

    // 写一处，然后：① commit(None) 必须报「无处回放」；② stream_into 能把整量交给外层
    chain.device().write_at(0, b"patch").unwrap();
    let err = chain.commit(None, None).unwrap_err();
    assert!(
        matches!(err, VolError::Unsupported(k) if k == "vol.reject.commit-spooled"),
        "流源提交应明确拒绝：{err:?}"
    );

    let mut out = Vec::new();
    let n = chain.stream_into(&mut out, None).unwrap();
    assert_eq!(n, payload.len() as u64, "整量交割：长度 = 镜像长度");
    assert_eq!(&out[..5], b"patch", "交割的内容是**当前视图**（含改动）");
    assert_eq!(&out[5..9], &payload[5..9], "未改部分保持原样");

    // spool 随链释放而清理（前缀与孤儿回收口径一致）
    let spool_dir = tmp.path().to_path_buf();
    drop(chain);
    let leftovers: Vec<_> = std::fs::read_dir(&spool_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(hzip_vol::materialize::SPOOL_PREFIX)
        })
        .collect();
    assert!(leftovers.is_empty(), "spool 临时文件应在释放时删除");
}

#[test]
fn stdin_and_multi_volume_sources_are_refused_with_stable_keys() {
    let tmp = tempfile::TempDir::new().unwrap();
    let err = Chain::open(
        Source::Stdin,
        WriteMode::Overlay,
        &opts_in(tmp.path()),
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, VolError::Unsupported("vol.reject.stdin-source")),
        "{err:?}"
    );
    let err = Chain::open(
        Source::Volumes {
            first: tmp.path().join("a.z01"),
            rest: vec![],
        },
        WriteMode::Overlay,
        &opts_in(tmp.path()),
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, VolError::Unsupported("vol.reject.multi-volume")),
        "{err:?}"
    );
}

#[test]
fn reset_discards_changes_and_cancel_aborts_commit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path(), "disk.img", 0x66);
    let before = std::fs::read(&src).unwrap();
    let mut chain = Chain::open(
        Source::File(src.clone()),
        WriteMode::Overlay,
        &opts_in(tmp.path()),
        None,
    )
    .unwrap();
    chain.device().write_at(0, b"junk").unwrap();
    chain.reset().unwrap();
    assert_eq!(chain.dirty_blocks(), 0, "reset 后无脏块");
    chain.commit(None, None).unwrap();
    assert_eq!(
        std::fs::read(&src).unwrap(),
        before,
        "reset 后提交不改变原文件"
    );

    // 取消：提交阶段取消 ⇒ Cancelled，且原文件不变
    chain.device().write_at(0, b"junk2").unwrap();
    let token = CancellationToken::new();
    token.cancel();
    let err = chain.commit(None, Some(&token)).unwrap_err();
    assert!(matches!(err, VolError::Cancelled), "{err:?}");
    assert_eq!(std::fs::read(&src).unwrap(), before);
}
