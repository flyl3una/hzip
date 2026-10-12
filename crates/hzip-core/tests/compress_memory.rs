//! 压缩管道的 **RSS 上界**回归（TC-ALG-011 语义的可自动化半边）。
//!
//! 用户报障（2026-09-29）：「大文件压缩/解压是否是流式？全量加载到内存会爆」。
//! 实测结论：大文件本身走 `File → 1 MiB 桥 → sink` 流式（400 MiB 单文件档
//! 峰值 ~18 MiB）；但**小文件并行预读**的缓存从不释放，峰值随「预读命中的
//! 小文件总字节数」线性增长——400 × 1 MiB 输入峰值 419 MiB。
//!
//! 修法：预读项 `cache[idx].take()` **写完即释放**。本用例以进程级分配计数
//! 锁死该不变量：40 MiB 小文件输入（`threads=1` ⇒ 预读窗口 W = 2）的
//! **净增活跃字节峰值**必须远小于输入总量（滞留缺陷下 ≈ 40 MiB）。
//!
//! 为什么用 `Level::Store` + tar：本用例只关心「字节在内存里停留多久」，
//! store 免掉压缩 CPU（用例秒级），且 tar 不引入 fmt 层额外物化（7z 写侧
//! 自身有 4 MiB/条目缓冲 + 溢出临时文件，会污染测量口径）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use hzip_core::jobs::{JobKind, JobSpec};
use hzip_core::services::compress::CompressSpec;
use hzip_format::{FormatHandler as _, FormatId};
use hzip_progress::Outcome;
use tempfile::TempDir;

// ——— 进程级分配计数（只影响本集成测试二进制）———

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

impl Counting {
    #[inline]
    fn add(n: usize) {
        let now = LIVE.fetch_add(n, Ordering::Relaxed) + n;
        PEAK.fetch_max(now, Ordering::Relaxed);
    }
    #[inline]
    fn sub(n: usize) {
        LIVE.fetch_sub(n, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            Self::add(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        Self::sub(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                Self::add(new_size - layout.size());
            } else {
                Self::sub(layout.size() - new_size);
            }
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const FILES: usize = 40;
const FILE_BYTES: usize = 1024 * 1024;
/// 判定线：净增峰值必须低于此值。滞留缺陷下峰值 ≈ FILES × 1 MiB（40 MiB）。
const MAX_PEAK_BYTES: usize = 16 * 1024 * 1024;

/// 40 MiB 小文件输入 → tar(store)：净增活跃字节峰值 < 16 MiB。
///
/// 滞留缺陷（`cache[idx].as_ref()` 永不释放）下本用例失败于 ≈ 40 MiB。
#[test]
fn many_small_files_do_not_accumulate_in_memory() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let body = vec![0x5Au8; FILE_BYTES];
    for i in 0..FILES {
        std::fs::write(src.join(format!("f{i:03}.bin")), &body).unwrap();
    }

    let out = dir.path().join("out.tar");
    let mut c = CompressSpec {
        inputs: vec![src.clone()],
        format: FormatId::from("tar"),
        out: out.clone(),
        level: hzip_codec::level::Level::Store,
        // 预读窗口 = 2 × clamp_threads(1) = 2 → 稳态只应有 ≤ 2 个活跃小文件
        threads: 1,
        verify_after: false,
        ..Default::default()
    };
    c.keep_metadata = false;
    let mut spec = JobSpec::new(JobKind::Compress, c.inputs[0].clone());
    spec.compress = Some(c);

    let eng = common::engine(1, 1);
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("压缩应 Done，实得 {other:?}"),
    }
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);

    // 产物自洽（免得测的是「什么都没干」）
    let rd = hzip_fmt_tar::TarHandler::new()
        .open_reader(hzip_format::Source::File(out.clone()), &Default::default())
        .expect("产出 tar 应可打开");
    let n = rd.entries().unwrap().len();
    assert_eq!(n, FILES + 1, "条目数应为 {FILES} 个文件 + 根目录条目");

    assert!(
        peak < MAX_PEAK_BYTES,
        "小文件预读缓存未释放：净增活跃峰值 {:.1} MiB（输入 {} MiB，判定线 {} MiB）",
        peak as f64 / 1024.0 / 1024.0,
        (FILES * FILE_BYTES) / 1024 / 1024,
        MAX_PEAK_BYTES / 1024 / 1024
    );
}
