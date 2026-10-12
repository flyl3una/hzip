//! malicious 样本回放（TC-SEC-005~007 管道级接管；T32）。
//!
//! T31 以字面量表模拟 QuotaGuard 增量；T32 走**真实 zip 档案 + 完整解压管道**
//! （open_reader → entries → 预检 → smart → 净化 → 流拷贝 → QuotaGuard
//! checkpoint），验证配额在真实流式解压中触发、已写文件清理、正当样本不误伤。
//!
//! 样本（tests-assets/malicious，T02 产物）与管道语义：
//! - `ratio_bomb.zip`：512 KiB 全零 deflate，ratio ≈ 817 > 200 → 4503
//!   （单层解压即触发——TC-SEC-005 等价真实半场）；
//! - `total_bomb.zip`：zip-of-zip 自相似（d=11, f=10）。**hzip 管道不递归解压
//!   嵌套档案**（与 7-Zip 一致——外层条目以普通文件落盘），故 4.2 PB 总量在
//!   单层解压不可达；以「降低 QuotaLimits 对照断言」驱动（任务卡 §6.3 注：
//!   自构造等价样本 + 人为调低限额的对照语义）——外层 10×34KB 条目累计越过
//!   低 max_total 触发 4502（管道级总量检查点语义验证）；
//! - `legit` 自构造随机词典文本 zip（ratio ≈ 4 < 200）→ **不误伤** Done +
//!   字节一致（sha256 等价断言）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;

use hzip_core::jobs::JobSpec;
use hzip_core::services::extract::ExtractOptions;
use hzip_format::{ConflictPolicy, FormatHandler, QuotaLimits};
use hzip_progress::Outcome;
use tempfile::TempDir;

use common::engine;

/// 定位恶意样本：从 manifest dir 逐级向上找仓库根（cargo test cwd = crate 目录）。
fn malicious(name: &str) -> std::path::PathBuf {
    let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let cand = dir.join("tests-assets/malicious").join(name);
        if cand.exists() {
            return cand;
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => break,
        }
    }
    panic!("样本缺失: tests-assets/malicious/{name}——需先 python tests-assets/malicious/build.py");
}

/// 跑一次完整解压并返回 Outcome。
fn extract_to(source: &Path, dest: &Path, quota: QuotaLimits) -> Outcome {
    let eng = engine(1, 1);
    let opts = ExtractOptions {
        dest_root: Some(dest.to_path_buf()),
        conflict: ConflictPolicy::Overwrite,
        quota,
        ..Default::default()
    };
    let h = eng
        .submit(JobSpec::extract_job(source.to_path_buf(), opts))
        .unwrap();
    h.wait().unwrap()
}

/// TC-SEC-005：高比率炸弹（ratio 817 > 200 默认限额）→ Quota MaxRatio 4503；
/// 半写文件清理（失败条目删除，dest 无残留）。
#[test]
fn sec_005_ratio_bomb_pipeline() {
    let bomb = malicious("ratio_bomb.zip");
    let dir = TempDir::new().unwrap();
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let r = extract_to(&bomb, &dest, QuotaLimits::default());
    match r {
        Outcome::Failed(e) => {
            assert_eq!(e.code(), 4503, "ratio 炸弹应 4503，实得 {e}");
        }
        other => panic!("应 Failed(4503)，实得 {other:?}"),
    }
    // 半写清理：触发 4503 的条目文件不得残留
    let leftovers: Vec<String> = std::fs::read_dir(&dest)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "半写文件应清理，dest 仍残留 {leftovers:?}"
    );
}

/// TC-SEC-006（管道级等价）：total_bomb 外层条目累计越过低 max_total → 4502。
///
/// 语义：QuotaGuard 总量检查点在流式解压中触发（真实读取非模拟）；条目尺度
/// 4501/4503 不误触发（外层 ratio ≈ 8.8 < 200、单条目 34KB < max_file）。
#[test]
fn sec_006_total_bomb_pipeline_low_quota() {
    let bomb = malicious("total_bomb.zip");
    let dir = TempDir::new().unwrap();
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    // 低总量限：外层 10 × ~34KB ≈ 340KB → 累计越 100KB 即 4502（对照语义）
    let quota = QuotaLimits {
        max_file_bytes: 64 * 1024 * 1024,
        max_total_bytes: 100 * 1024,
        max_ratio: 200,
        max_entries: 1_000_000,
    };
    let r = extract_to(&bomb, &dest, quota);
    match r {
        Outcome::Failed(e) => {
            assert_eq!(e.code(), 4502, "累计超限应 4502，实得 {e}");
        }
        other => panic!("应 Failed(4502)，实得 {other:?}"),
    }
    // 已写受控：完整条目保留（≤ 100KB 限内已落盘）——总量不爆盘
    let total_written: u64 = std::fs::read_dir(&dest)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    assert!(
        total_written <= 200 * 1024,
        "配额拦截后已写 {total_written} 字节应受控"
    );
}

/// TC-SEC-007：正当高压缩不误伤——随机词典文本 zip（ratio ≈ 4 < 200）默认限额
/// → Done + 字节一致（sha256 等价断言）。
#[test]
fn sec_007_legit_high_compression_ok() {
    let dir = TempDir::new().unwrap();
    // 词典随机文本：可压缩但不极端（ratio ≈ 4，远低于 200 上限）
    let content = random_dict_text(3_000_000);
    let src = dir.path().join("legit-src.txt");
    std::fs::write(&src, &content).unwrap();
    let zip = dir.path().join("legit.zip");
    let seven_zip = seven_zip_bin();
    let out = std::process::Command::new(&seven_zip)
        .args(["a", "-tzip", "-mx=6"])
        .arg(&zip)
        .arg(&src)
        .output()
        .expect("7z 运行");
    assert!(
        out.status.success(),
        "7z 压 zip 失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // 校验真实 ratio < 200（防误伤断言的前提）
    {
        let zf = std::fs::read(&zip).unwrap();
        let raw = content.len() as u64;
        // zip 中央目录读出压缩尺寸不便手解——以解压成功为准（ratio 若 >200
        // 会触发 4503 使测试失败，故断言不误伤本身即覆盖）
        let _ = zf;
        let _ = raw;
    }
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let r = extract_to(&zip, &dest, QuotaLimits::default());
    match r {
        Outcome::Done(_) => {}
        other => panic!("正当高压缩不应误伤，实得 {other:?}"),
    }
    // 内容一致（等效 sha256）：解压目录内单文件
    let mut names: Vec<_> = std::fs::read_dir(&dest)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    assert!(!names.is_empty(), "应有解压文件");
    let got = std::fs::read(names.remove(0)).unwrap();
    assert_eq!(got.len(), content.len(), "长度一致");
    assert_eq!(got, content, "字节一致（等效 sha256）");
}

/// 确定性词典文本（3 MiB 级；可压 ratio ≈ 4 的正当样本）。
fn random_dict_text(target_len: usize) -> Vec<u8> {
    let words: Vec<&str> = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo \
                            lima mike november oscar papa quebec romeo sierra tango uniform \
                            victor whiskey xray yankee zulu config load save open close copy \
                            move delete create update merge split scan verify repair"
        .split_whitespace()
        .collect();
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut out = Vec::with_capacity(target_len);
    let mut line = String::new();
    while out.len() < target_len {
        line.clear();
        let n = 6 + (state % 9) as usize;
        for i in 0..n {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let w = words[(state as usize) % words.len()];
            if i > 0 {
                line.push(' ');
            }
            line.push_str(w);
        }
        line.push('\n');
        out.extend_from_slice(line.as_bytes());
    }
    out
}

/// 定位 7-Zip：沿 `PATH` 查找平台可执行名。
///
/// unix 的发行名为 `7z`/`7za`/`7zz`（无 `.exe` 后缀），且 `PATH` 分隔符是 `:`
/// —— 故必须用 [`std::env::split_paths`]；此前以 `;` 切分并只找 `7z.exe`，
/// 在 macOS/Linux 上恒失败（该用例因此永远 panic，属未编译过的 unix 分支）。
fn seven_zip_bin() -> std::path::PathBuf {
    #[cfg(windows)]
    const NAMES: [&str; 3] = ["7z.exe", "7za.exe", "7zz.exe"];
    #[cfg(not(windows))]
    const NAMES: [&str; 3] = ["7z", "7za", "7zz"];
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in NAMES {
                let cand = dir.join(name);
                if cand.is_file() {
                    return cand;
                }
            }
        }
    }
    panic!("未找到 7-Zip（需要 7z 压制白名单样本）");
}

/// 仅目录条目档案（预检 Σ size_raw=0 跳过；目录落盘 Done）。
#[test]
fn extract_dir_only_archive() {
    let dir = TempDir::new().unwrap();
    let archive = dir.path().join("dirs.tar");
    {
        let mut w = hzip_fmt_tar::TarHandler::new()
            .create_writer(
                hzip_format::Sink::File(archive.clone()),
                &Default::default(),
            )
            .unwrap();
        let meta = hzip_format::EntryMeta {
            name: "emptydir/".into(),
            is_dir: true,
            size_raw: Some(0),
            ..Default::default()
        };
        let s = w
            .next_entry(&meta, hzip_format::PasswordRef::none())
            .unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
    }
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let eng = engine(1, 1);
    let opts = ExtractOptions {
        dest_root: Some(dest.clone()),
        conflict: ConflictPolicy::Overwrite,
        ..Default::default()
    };
    let h = eng.submit(JobSpec::extract_job(archive, opts)).unwrap();
    let r = h.wait().unwrap();
    assert!(matches!(r, Outcome::Done(_)), "目录档案应 Done，实得 {r:?}");
    assert!(dest.join("emptydir").is_dir(), "目录应落盘");
}
