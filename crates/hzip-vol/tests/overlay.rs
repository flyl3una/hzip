//! 覆盖设备的三条不变量（**契约冻结**：后续所有写回路径都建在它上面）。
//!
//! | 不变量 | 为什么是硬要求 |
//! |---|---|
//! | 提交前原设备**一字节不动** | 用户裁定 ② 的安全底线：取消/崩溃 ⇒ 原档完好 |
//! | 回放**只碰脏块** | 否则「改一个文件」会变成「重写整盘」，40 GB 镜像不可接受 |
//! | 回放**幂等** | core 的密码重试会对同一会话重复提交，幂等是重试正确性的前提 |
//!
//! 另外覆盖了「读穿透」「reset 丢弃改动」「取消可中断」「越界拒绝」四条边界。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例（同 hzip-fmt-cpio/tests）

use std::sync::Arc;

use fs_core::{BlockDevice as _, BlockRead};
use hzip_format_testkit::vol::MemDevice;
use hzip_progress::CancellationToken;
use hzip_vol::error::VolError;
use hzip_vol::overlay::{OVERLAY_BLOCK, OverlayDevice};

/// 造一块填充了 `fill` 的 base 盘 + 一个空覆盖层。
fn fixture(blocks: u64, fill: u8) -> (Arc<MemDevice>, OverlayDevice) {
    let base = Arc::new(MemDevice::from_bytes(vec![
        fill;
        (blocks * OVERLAY_BLOCK) as usize
    ]));
    let spill = tempfile::tempfile().expect("溢出临时文件");
    let ov = OverlayDevice::new(base.clone(), spill, OVERLAY_BLOCK);
    (base, ov)
}

fn read(dev: &dyn BlockRead, off: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    dev.read_at(off, &mut buf).unwrap();
    buf
}

#[test]
fn reads_through_until_written() {
    let (base, ov) = fixture(3, 0xAA);
    assert_eq!(read(&ov, 0, 8), vec![0xAA; 8], "未写块必须穿透到 base");

    ov.write_at(OVERLAY_BLOCK + 5, &[0x11, 0x22]).unwrap();

    assert_eq!(
        read(&ov, OVERLAY_BLOCK + 5, 2),
        vec![0x11, 0x22],
        "写后读自己的改动"
    );
    assert_eq!(
        read(&base, OVERLAY_BLOCK + 5, 2),
        vec![0xAA; 2],
        "提交前原设备一字节不动"
    );
    assert_eq!(ov.dirty_blocks(), 1, "只标脏被触及的块");
    assert_eq!(ov.dirty_bytes(), OVERLAY_BLOCK);
    // 跨块写：两块的其余字节仍是 base（读穿透）与写入值（脏块）拼接
    ov.write_at(OVERLAY_BLOCK - 2, &[1, 2, 3, 4]).unwrap();
    assert_eq!(
        read(&ov, OVERLAY_BLOCK - 2, 4),
        vec![1, 2, 3, 4],
        "跨界写应两块都生效"
    );
    assert_eq!(ov.dirty_blocks(), 2);
}

/// 回归（先炸后修的真实缺陷）：只写块内几个字节后，**整块读**必须给出
/// 「本次写入 + 该块其余部分来自 base」——绝不是零。
#[test]
fn partial_write_keeps_the_rest_of_the_block_from_base() {
    let (_base, ov) = fixture(2, 0xAA);
    let at = OVERLAY_BLOCK + 100;
    ov.write_at(at, b"payload").unwrap();
    let block = read(&ov, OVERLAY_BLOCK, OVERLAY_BLOCK as usize);
    assert_eq!(&block[100..107], b"payload", "写入部分是新值");
    assert_eq!(&block[..100], &[0xAA; 100][..], "前置部分来自 base");
    assert_eq!(
        &block[107..],
        &vec![0xAA; OVERLAY_BLOCK as usize - 107][..],
        "后置部分来自 base（曾被读成洞里的零）"
    );
}

/// 连续大写不应产生写时拷贝放大：未触及区间为空。
#[test]
fn contiguous_full_block_write_copies_nothing_extra() {
    let (_base, ov) = fixture(2, 0xAA);
    ov.write_at(0, &vec![0x5A; (2 * OVERLAY_BLOCK) as usize])
        .unwrap();
    assert_eq!(ov.dirty_blocks(), 2);
    assert_eq!(read(&ov, 0, 4), vec![0x5A; 4]);
}

#[test]
fn replay_touches_only_dirty_blocks() {
    let (_base, ov) = fixture(4, 0xAA);
    ov.write_at(2 * OVERLAY_BLOCK + 9, b"changed").unwrap();

    let target = MemDevice::from_bytes(vec![0x77; (4 * OVERLAY_BLOCK) as usize]);
    let n = ov.replay_into(&target, None).unwrap();
    assert_eq!(n, 1, "只回放 1 个脏块");
    assert_eq!(
        read(&target, 2 * OVERLAY_BLOCK + 9, 7),
        b"changed".to_vec(),
        "脏块写入目标"
    );
    assert_eq!(
        read(&target, 0, 8),
        vec![0x77; 8],
        "未脏的块不得被覆盖（目标原有内容保留）"
    );
    assert_eq!(read(&target, 3 * OVERLAY_BLOCK, 8), vec![0x77; 8]);
}

#[test]
fn replay_is_idempotent() {
    let (_base, ov) = fixture(2, 0x10);
    ov.write_at(0, b"abc").unwrap();
    let target = MemDevice::new(2 * OVERLAY_BLOCK);
    ov.replay_into(&target, None).unwrap();
    let once = target.to_bytes();
    ov.replay_into(&target, None).unwrap();
    assert_eq!(
        target.to_bytes(),
        once,
        "重复回放结果必须一致（core 会重试）"
    );
}

#[test]
fn stream_out_matches_the_device_view() {
    let (_base, ov) = fixture(3, 0x20);
    ov.write_at(OVERLAY_BLOCK + 1, b"x").unwrap();
    let mut out = Vec::new();
    let n = ov.stream_out(&mut out, None).unwrap();
    assert_eq!(n, 3 * OVERLAY_BLOCK, "整流量拷长度 = 设备长度");
    assert_eq!(out, read(&ov, 0, out.len()), "流出的字节 = 当前设备视图");
}

#[test]
fn reset_discards_pending_changes() {
    let (_base, ov) = fixture(2, 0x30);
    ov.write_at(0, b"zzz").unwrap();
    assert_eq!(ov.dirty_blocks(), 1);
    ov.reset().unwrap();
    assert_eq!(ov.dirty_blocks(), 0, "reset 后无脏块");
    assert_eq!(read(&ov, 0, 3), vec![0x30; 3], "reset 后读回 base 原值");
}

#[test]
fn cancel_during_replay_stops_with_cancelled() {
    let (_base, ov) = fixture(2, 0x40);
    ov.write_at(0, b"a").unwrap();
    let token = CancellationToken::new();
    token.cancel();
    let target = MemDevice::new(2 * OVERLAY_BLOCK);
    let err = ov.replay_into(&target, Some(&token)).unwrap_err();
    assert!(matches!(err, VolError::Cancelled), "取消应报 Cancelled");
}

#[test]
fn bounds_are_enforced_on_both_paths() {
    let (_base, ov) = fixture(2, 0x50);
    assert!(
        ov.write_at(2 * OVERLAY_BLOCK - 4, &[0; 8]).is_err(),
        "跨界写"
    );
    assert!(read_at_err(&ov, 2 * OVERLAY_BLOCK, 1), "越界读");
    // 贴尾的空写合法
    assert!(ov.write_at(2 * OVERLAY_BLOCK, &[]).is_ok());
    // 容量不可变
    assert!(ov.set_len(1).is_err(), "本期不支持改容量");
    assert!(!ov.can_grow());
}

#[test]
fn overlay_reports_base_size() {
    let (base, ov) = fixture(2, 0x60);
    assert_eq!(ov.size_bytes(), base.size_bytes());
}

fn read_at_err(dev: &dyn BlockRead, off: u64, len: usize) -> bool {
    let mut buf = vec![0u8; len];
    dev.read_at(off, &mut buf).is_err()
}
