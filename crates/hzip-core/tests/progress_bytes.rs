//! 进度语义集成测试（用户实测缺陷 2026-09-24：**解压一个大文件一直 0%**）。
//!
//! # 缺陷链（三段，逐段钉住）
//!
//! 1. 单流格式（`.gz`/`.xz`/`.zst`）原先一律 `size_raw = 0`（「流式未知」）⇒
//!    解压任务的 `total_bytes = Σ size_raw = 0` ⇒ 百分比无从计算（恒未知/0%）；
//! 2. 于是「按字节的平滑进度」这个能力虽然一直在（core 每 1 MiB 发 `Bytes`），
//!    却推不出百分比；
//! 3. 修法：容器**元数据里写着**原始大小时就上报（gz 尾部 ISIZE / xz 尾部索引 /
//!    zstd 帧头），core 的 total 随之可得 ⇒ 百分比按字节平滑推进。
//!
//! 本文件断言**用户可见的结果**：任务的 `total_bytes` 等于载荷真实大小，且运行
//! 期间能观察到 `0 < done < total` 的中间快照（= 进度确实在中途推进，而不是从
//! 0% 直接跳到 100%）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;

use hzip_core::engine::{Engine, EngineConfig};
use hzip_core::jobs::JobSpec;
use hzip_core::services::extract::ExtractOptions;
use hzip_format::{ConflictPolicy, FormatHandler, Sink, Source};
use hzip_progress::{ProgressEvent, Throttle};
use tempfile::TempDir;

/// 写一个 gz 单文件流（载荷大小已知）。
fn write_gz(path: &std::path::Path, payload: &[u8]) {
    let mut w = hzip_fmt_single::gz()
        .create_writer(Sink::File(path.to_path_buf()), &Default::default())
        .expect("create gz writer");
    let meta = hzip_format::EntryMeta {
        name: "blob.bin".into(),
        size_raw: Some(payload.len() as u64),
        ..Default::default()
    };
    let mut sink = w
        .next_entry(&meta, hzip_format::PasswordRef::none())
        .unwrap();
    sink.write_all(payload).unwrap();
    sink.finish().unwrap();
    w.finish().unwrap();
}

fn engine() -> Engine {
    Engine::new(EngineConfig {
        pool_size: Some(1),
        max_parallel: 1,
        cancel_on_signal: false,
        orphan_cleanup: false,
    })
    .expect("engine")
}

#[test]
fn single_stream_extract_reports_byte_total_and_progress() {
    let dir = TempDir::new().unwrap();
    let gz = dir.path().join("blob.bin.gz");
    // 8 MiB 载荷：足够长到能在运行期观察到中间快照（core 每 1 MiB 发一次 Bytes）
    let payload: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    write_gz(&gz, &payload);

    // 条目层：容器声明的大小已如实上报
    let r = hzip_fmt_single::gz()
        .open_reader(Source::File(gz.clone()), &Default::default())
        .unwrap();
    let entries = r.entries().unwrap();
    assert_eq!(
        entries[0].size_raw,
        payload.len() as u64,
        "gz 条目应报出真实原始大小（修前恒 0 ⇒ 进度恒 0%）"
    );

    // 任务层：total = 载荷大小；运行期存在 0 < done < total 的中间快照
    let dest = dir.path().join("out");
    let eng = engine();
    let opts = ExtractOptions {
        dest_root: Some(dest.clone()),
        conflict: ConflictPolicy::Overwrite,
        ..Default::default()
    };
    let h = eng.submit(JobSpec::extract_job(gz, opts)).expect("submit");
    let rx = h.events();
    let mut th = Throttle::new();
    let mut saw_total = 0u64;
    // 字节**逐块**推进的证据：累计 `Bytes` 增量在结尾前就应越过 0 且尚未到顶。
    // 断言事件层而非快照层——10 Hz 快照窗口是 `hzip-progress` 自己的单测面，
    // 且短任务（本夹具 8 MiB ≈ 10 ms）本来就落在同一个窗口里，按快照断言会抖动。
    let mut cumulative = 0u64;
    let mut mid_flight = false;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            hzip_progress::RecvTimeout::Closed => break,
            hzip_progress::RecvTimeout::Timeout => break,
            hzip_progress::RecvTimeout::Event(ev) => {
                if let ProgressEvent::Bytes { delta } = &ev {
                    cumulative = cumulative.saturating_add(*delta);
                    if cumulative > 0 && cumulative < payload.len() as u64 {
                        mid_flight = true;
                    }
                }
                if let Some(s) = th.push(&ev) {
                    saw_total = saw_total.max(s.total);
                }
                if matches!(ev, ProgressEvent::JobFinished { .. }) {
                    break;
                }
            }
        }
    }
    assert_eq!(
        cumulative,
        payload.len() as u64,
        "字节事件累计必须等于载荷大小（进度按字节推进）"
    );
    assert_eq!(
        saw_total,
        payload.len() as u64,
        "进度快照的 total 必须等于真实载荷大小（百分比才能算得出来）"
    );
    assert!(
        mid_flight,
        "运行期必须出现 0 < done < total 的中间快照：进度是按字节平滑推进的，\
         而非 0% → 100% 跳变"
    );
    assert_eq!(
        std::fs::read(dest.join("blob.bin")).unwrap().len(),
        payload.len(),
        "内容完整落盘"
    );
}
