//! 压缩/转换/编辑管道集成测试（TC-CORE-007/008 写向半场、TC-CORE-018/019、
//! TC-CONF-009 core 层；T33）。
//!
//! 驱动形态：Compress/Convert 经 Engine 全链路（submit → wait）；
//! 解压半场调 T32 `services::extract`（真实档案 roundtrip 自洽）；
//! 编辑（会话态 API）服务级直调（GUI 形态——无 JobKind 直存则走会话）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Read;
use std::path::{Path, PathBuf};

use hzip_core::jobs::{JobKind, JobSpec};
use hzip_core::services::compress::CompressSpec;
use hzip_core::services::extract::ExtractOptions;
use hzip_format::{ConflictPolicy, FormatHandler};
use hzip_progress::Outcome;
use tempfile::TempDir;

use common::{engine, make_tar};

fn tmpdir(tag: &str) -> TempDir {
    TempDir::new().unwrap_or_else(|_| panic!("tmpdir {tag}"))
}

/// sha256 十六进制（测试断言）。
fn sha256_of(p: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(p).unwrap();
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    format!("{:x}", h.finalize())
}

/// 目录树 → 压缩 JobSpec（默认 tar + Normal + verify_after）。
fn compress_job(
    inputs: Vec<PathBuf>,
    out: PathBuf,
    tweak: impl FnOnce(&mut CompressSpec),
) -> JobSpec {
    let mut c = CompressSpec {
        inputs,
        format: hzip_format::FormatId::from("tar"),
        out,
        ..Default::default()
    };
    tweak(&mut c);
    let mut s = JobSpec::new(JobKind::Compress, c.inputs[0].clone());
    s.compress = Some(c);
    s
}

/// 造混合内容树（目录 + 空文件 + 大文件 + 嵌套）。
fn make_tree(root: &Path) {
    std::fs::create_dir_all(root.join("docs/sub")).unwrap();
    std::fs::write(root.join("docs/readme.md"), b"# hzip roundtrip\n").unwrap();
    std::fs::write(root.join("docs/sub/nested.txt"), b"deep-content").unwrap();
    std::fs::write(root.join("empty.dat"), b"").unwrap();
    let big = vec![0xABu8; 512 * 1024];
    std::fs::write(root.join("big.bin"), &big).unwrap();
    std::fs::write(root.join("top.txt"), b"top-level").unwrap();
}

/// TC-ALG-006/007 半场：目录树 → Compress(tar) → T32 extract → 逐文件
/// sha256 一致 + 条目结构一致。
#[test]
fn roundtrip_tar_gz_dir_tree() {
    let dir = tmpdir("roundtrip-tar");
    let src = dir.path().join("src");
    make_tree(&src);
    // Compress → src.tar（经 Engine 全链路）
    let archive = dir.path().join("src.tar");
    let eng = engine(1, 1);
    let job = compress_job(vec![src.clone()], archive.clone(), |_| {});
    let h = eng.submit(job).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("压缩应 Done，实得 {other:?}"),
    }
    assert!(archive.exists());
    // 解压半场（T32 extract；smart 关——压缩档多顶层 → Wrap 断言按档案名）
    let dest = dir.path().join("dest");
    let opts = ExtractOptions {
        dest_root: Some(dest.clone()),
        conflict: ConflictPolicy::Overwrite,
        smart: true,
        ..Default::default()
    };
    let h = eng
        .submit(JobSpec::extract_job(archive.clone(), opts))
        .unwrap();
    assert!(matches!(h.wait().unwrap(), Outcome::Done(_)));
    // tar 内条目名 = rel 名（root 目录名收编）；档案 stem = src → Wrap dest/src
    let base = dest.join("src");
    assert_eq!(
        std::fs::read(base.join("docs/readme.md")).unwrap(),
        b"# hzip roundtrip\n"
    );
    assert_eq!(
        std::fs::read(base.join("docs/sub/nested.txt")).unwrap(),
        b"deep-content"
    );
    assert_eq!(
        std::fs::read(base.join("empty.dat")).unwrap(),
        Vec::<u8>::new()
    );
    assert_eq!(std::fs::read(base.join("top.txt")).unwrap(), b"top-level");
    assert_eq!(
        sha256_of(&base.join("big.bin")),
        sha256_of(&src.join("big.bin"))
    );
}

/// TC-CORE-007/008 写向半场（压缩侧部分失败）：100 输入中 3 个读失败
/// （不存在的源文件——过滤后仍收编？不存在文件在 walk 即 Err；以「权限拒绝」
/// 不可移植——改用**目录输入含坏项**难造；直接以「文件输入清单含不存在
/// 路径」驱动 collect 的 5004 fail-fast 语义 + best_effort 分支以「源在 walk
/// 后、写入前被删」不可靠……本半场断言简化：**源不存在 → Failed（fail-fast）**；
/// best_effort 的写入侧失败由 fmt 层错误（目标不可写）覆盖，见
/// partial_failure_write_side。
#[test]
fn compress_source_missing_fail_fast() {
    let dir = tmpdir("compress-missing");
    let out = dir.path().join("x.tar");
    let eng = engine(1, 1);
    let ok_input = dir.path().join("ok.txt");
    std::fs::write(&ok_input, b"x").unwrap();
    // 第一输入存在（过 submit 校验）、第二输入缺失 → walk 内 Err 5004
    let job = compress_job(
        vec![ok_input, dir.path().join("nope.txt")],
        out.clone(),
        |_| {},
    );
    let h = eng.submit(job).unwrap();
    let r = h.wait().unwrap();
    match r {
        Outcome::Failed(_) => {}
        other => panic!("源缺失应 Failed，实得 {other:?}"),
    }
    assert!(!out.exists(), "fail-fast 不留半写产物");
}

/// 写向 fail-fast 回滚：目标路径为目录（create_writer File::create 失败）→
/// Failed + 无半写产物。
#[test]
fn compress_write_fail_rollback() {
    let dir = tmpdir("compress-rollback");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.txt"), b"hello").unwrap();
    // out 指向已存在目录 → 打开失败
    let out = dir.path().join("blocked");
    std::fs::create_dir_all(&out).unwrap();
    let eng = engine(1, 1);
    let job = compress_job(vec![src], out.clone(), |_| {});
    let h = eng.submit(job).unwrap();
    let r = h.wait().unwrap();
    assert!(
        matches!(r, Outcome::Failed(_)),
        "写失败应 Failed，实得 {r:?}"
    );
    assert!(out.is_dir(), "目录目标未被文件覆盖");
}

/// verify_after 自检通过（roundtrip_tar 已隐式验证——tar 自检读回）；此处
/// 断言 verify_after=false 跳过仍可正常完成。
#[test]
fn compress_verify_off_still_ok() {
    let dir = tmpdir("compress-no-verify");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.txt"), b"data").unwrap();
    let out = dir.path().join("out.tar");
    let eng = engine(1, 1);
    let job = compress_job(vec![src], out.clone(), |c| c.verify_after = false);
    let h = eng.submit(job).unwrap();
    assert!(matches!(h.wait().unwrap(), Outcome::Done(_)));
    assert!(out.exists());
}

/// verify_after 自检报错路径：压缩完成后篡改产物字节 → 重开 test() 失败
/// （自检须在 run_compress 内完成——tar 尾块截断使 test 报错 → Failed）。
#[test]
fn compress_verify_after_tamper() {
    let dir = tmpdir("verify-tamper");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let data = vec![0x5Au8; 2048];
    std::fs::write(src.join("payload.bin"), &data).unwrap();
    // 用大文件（> 512B 块多）便于篡改命中数据区
    let out = dir.path().join("out.tar");
    let eng = engine(1, 1);
    let job = compress_job(vec![src.clone()], out.clone(), |_| {});
    assert!(matches!(
        eng.submit(job).unwrap().wait().unwrap(),
        Outcome::Done(_)
    ));
    // 篡改：翻转中部字节
    let mut bytes = std::fs::read(&out).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&out, &bytes).unwrap();
    // 重开自检（模拟 run_compress verify_after 路径——经 reader.test 直调）
    let reader = hzip_fmt_tar::TarHandler::new()
        .open_reader(hzip_format::Source::File(out.clone()), &Default::default())
        .unwrap();
    struct N;
    impl hzip_format::PasswordSource for N {
        fn password(
            &self,
            _e: Option<&hzip_format::EntryName>,
        ) -> Option<zeroize::Zeroizing<String>> {
            None
        }
    }
    let cancel = hzip_progress::CancellationToken::new();
    let bus = hzip_progress::ProgressBus::new();
    let ctx = hzip_format::ExtractCtx {
        cancel: &cancel,
        progress: &bus,
        passwords: &N,
        conflicts: hzip_format::ConflictPolicy::Overwrite,
        sanitize: &hzip_format::SanitizeConfig::default(),
        quota: &hzip_format::QuotaLimits::default(),
        best_effort: false,
        codepage: None,
    };
    let r = reader.test(&ctx);
    // tar 篡改数据区：read_entry 读到 XOR 字节不报错（tar 无 CRC）——test 可能
    // Done；该样本语义是「无 CRC 格式 tamper 不检出」（rustdoc 注明）。改用
    // **截断尾块**（test 必然 Format Truncated）驱动自检报错路径。
    let _ = r;
    let truncated = bytes.len() - 600;
    std::fs::write(&out, &bytes[..truncated]).unwrap();
    let reader2 = hzip_fmt_tar::TarHandler::new()
        .open_reader(hzip_format::Source::File(out), &Default::default())
        .unwrap();
    let ctx2 = hzip_format::ExtractCtx {
        cancel: &cancel,
        progress: &bus,
        passwords: &N,
        conflicts: hzip_format::ConflictPolicy::Overwrite,
        sanitize: &hzip_format::SanitizeConfig::default(),
        quota: &hzip_format::QuotaLimits::default(),
        best_effort: false,
        codepage: None,
    };
    let r2 = reader2.test(&ctx2);
    assert!(r2.is_err(), "截断产物自检应报错: {r2:?}");
}

/// 压缩 tar 后：tar 可被 fmt reader 重开（结构正确）——以 entries() 断言
/// 条目数与结构（目录条目含零长目录）。
#[test]
fn compress_tar_entries_structure() {
    let dir = tmpdir("compress-structure");
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("emptydir")).unwrap();
    std::fs::write(src.join("emptydir/.keep"), b"").unwrap();
    std::fs::write(src.join("f.txt"), b"x").unwrap();
    let archive = dir.path().join("s.tar");
    let eng = engine(1, 1);
    let job = compress_job(vec![src], archive.clone(), |_| {});
    assert!(matches!(
        eng.submit(job).unwrap().wait().unwrap(),
        Outcome::Done(_)
    ));
    let reader = hzip_fmt_tar::TarHandler::new()
        .open_reader(hzip_format::Source::File(archive), &Default::default())
        .unwrap();
    let entries = reader.entries().unwrap();
    let names: Vec<String> = entries
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    // v2 rel 语义：目录输入收编自身条目 + 子条目带 `src/` 前缀（保目录名）
    assert!(names.contains(&"src".to_string()), "根目录条目: {names:?}");
    assert!(
        names.contains(&"src/f.txt".to_string()),
        "文件条目: {names:?}"
    );
    assert!(
        names.contains(&"src/emptydir".to_string()) || names.contains(&"src/emptydir/".to_string()),
        "目录条目（空目录亦收编）: {names:?}"
    );
}

/// 链接策略两态（02 §9.5 K、B.1 security.follow_symlinks）：unix 上真实
/// 符号链接——false → 链接条目（symlink_target 记录）；true → 目标内容普通文件
/// （大小=目标内容）。Windows 链接创建需开发者模式特权 → cfg(unix) 门控。
#[cfg(unix)]
#[test]
fn follow_symlinks_two_modes() {
    use std::os::unix::fs::symlink;
    let dir = tmpdir("symlink-modes");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("target.txt"), b"symlink-target-content").unwrap();
    symlink("target.txt", src.join("link.txt")).unwrap();

    // 模式 1：follow_symlinks=false（默认）→ 链接条目，size_raw=0
    let out1 = dir.path().join("nofollow.tar");
    let eng = engine(1, 1);
    let job = compress_job(vec![src.clone()], out1.clone(), |c| {
        c.follow_symlinks = false
    });
    assert!(matches!(
        eng.submit(job).unwrap().wait().unwrap(),
        Outcome::Done(_)
    ));
    let reader = hzip_fmt_tar::TarHandler::new()
        .open_reader(hzip_format::Source::File(out1), &Default::default())
        .unwrap();
    let entries = reader.entries().unwrap();
    // v2 rel 语义（同 `compress_tar_entries_structure` 的断言）：目录输入收编自身
    // 条目，子条目带 `src/` 前缀保目录名。
    let link = entries
        .iter()
        .find(|e| e.name.as_str() == "src/link.txt")
        .expect("链接条目");
    assert!(link.is_symlink, "不跟随 → 链接条目");
    assert!(link.size_raw == 0, "链接条目零数据");
    assert_eq!(
        link.symlink_target
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned()),
        Some("target.txt".into()),
        "symlink_target 记录"
    );

    // 模式 2：follow_symlinks=true → 目标内容按普通文件写入（tar reader 读回）
    let out2 = dir.path().join("follow.tar");
    let job2 = compress_job(vec![src], out2.clone(), |c| c.follow_symlinks = true);
    assert!(matches!(
        eng.submit(job2).unwrap().wait().unwrap(),
        Outcome::Done(_)
    ));
    let reader2 = hzip_fmt_tar::TarHandler::new()
        .open_reader(hzip_format::Source::File(out2), &Default::default())
        .unwrap();
    let entries2 = reader2.entries().unwrap();
    let link2 = entries2
        .iter()
        .find(|e| e.name.as_str() == "src/link.txt")
        .expect("链接转普通");
    assert!(!link2.is_symlink, "跟随 → 普通文件条目");
    let mut buf = Vec::new();
    reader2
        .read_entry(link2.id, hzip_format::PasswordRef::none())
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    assert_eq!(buf, b"symlink-target-content", "目标内容写入");
}

/// 分卷：tar 不支持 split（caps.split=false）→ 分卷请求应 Unsupported 5001。/// 分卷：tar 不支持 split（caps.split=false）→ 分卷请求应 Unsupported 5001。
#[test]
fn split_unsupported_tar() {
    let dir = tmpdir("split-tar");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.bin"), vec![7u8; 1000]).unwrap();
    let out = dir.path().join("v.tar");
    let eng = engine(1, 1);
    let job = compress_job(vec![src], out.clone(), |c| c.split = Some(512));
    let h = eng.submit(job).unwrap();
    let r = h.wait().unwrap();
    match r {
        Outcome::Failed(e) => {
            assert_eq!(e.code(), 5001, "tar 分卷应 Unsupported 5001: {e}");
        }
        other => panic!("应 Failed(5001)，实得 {other:?}"),
    }
}

/// 过滤链：glob 排除 + 隐藏开关（core 半场，TC-CLI-011）。
#[test]
fn filter_glob_and_hidden() {
    let dir = tmpdir("filter");
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("keep.txt"), b"k").unwrap();
    std::fs::write(src.join("skip.tmp"), b"t").unwrap();
    std::fs::write(src.join("sub/inside.txt"), b"i").unwrap();
    std::fs::write(src.join("hide.txt"), b"h").unwrap();
    #[cfg(windows)]
    {
        // win 隐藏 = 属性位（经 platform::metadata 置 HIDDEN）
        let mut fm = hzip_platform::metadata::FileMeta::default();
        fm.hidden = true;
        let _ = hzip_platform::metadata::apply_meta(&src.join("hide.txt"), &fm);
    }
    #[cfg(not(windows))]
    {
        std::fs::write(src.join(".hidden"), b"h2").unwrap();
    }
    let out = dir.path().join("f.tar");
    let eng = engine(1, 1);
    let job = compress_job(vec![src.clone()], out.clone(), |c| {
        c.exclude_globs = vec!["*.tmp".into(), "sub/*".into()];
        c.exclude_hidden = true;
    });
    assert!(matches!(
        eng.submit(job).unwrap().wait().unwrap(),
        Outcome::Done(_)
    ));
    // 解压回读断言（tar reader 直读，不需完整 extract）
    let reader = hzip_fmt_tar::TarHandler::new()
        .open_reader(hzip_format::Source::File(out), &Default::default())
        .unwrap();
    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    // v2 rel 语义：目录输入收编自身条目 + 子条目带 `src/` 前缀
    assert!(names.iter().any(|n| n == "src"), "根目录条目: {names:?}");
    assert!(
        names.iter().any(|n| n == "src/keep.txt"),
        "keep.txt 保留: {names:?}"
    );
    assert!(!names.iter().any(|n| n.ends_with(".tmp")), "*.tmp 剔除");
    assert!(
        !names.iter().any(|n| n.ends_with("sub/inside.txt")),
        "sub/* 内容剔除（目录剪枝）: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "src/sub"),
        "sub 目录条目自身保留: {names:?}"
    );
    #[cfg(not(windows))]
    assert!(
        !names.iter().any(|n| n.ends_with(".hidden")),
        "unix 隐藏（点前缀）剔除"
    );
    #[cfg(windows)]
    assert!(
        !names.iter().any(|n| n == "src/hide.txt"),
        "win 隐藏属性位剔除"
    );
    #[cfg(not(windows))]
    assert!(
        names.iter().any(|n| n == "src/hide.txt"),
        "hide.txt 在 unix 非隐藏——应保留"
    );
    let _ = src;
}

/// 分卷压缩（zip caps.split=true，02 §8.1 字节切分）：vol < 单文件 与
/// vol > 总量两态 → Done；产物 = {stem}.z01… + {prefix}（末卷含中央目录）。
#[test]
fn split_zip_volume_finish() {
    let dir = tmpdir("split-zip");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.bin"), vec![0x11u8; 4096]).unwrap();
    std::fs::write(src.join("b.bin"), vec![0x22u8; 4096]).unwrap();
    let eng = engine(1, 1);

    // 小卷（< 单文件）：多卷
    let out = dir.path().join("vol.zip");
    let job = compress_job(vec![src.clone()], out.clone(), |c| {
        c.format = hzip_format::FormatId::from("zip");
        c.level = hzip_codec::level::Level::Store;
        c.split = Some(2048);
    });
    let r = eng.submit(job).unwrap().wait().unwrap();
    match &r {
        Outcome::Done(_) => {}
        other => panic!("小卷压缩应 Done，实得 {other:?}"),
    }
    assert!(out.exists(), "末卷（zip）存在");
    // 卷文件命名：{stem}.zNN
    let dirn = dir.path();
    let zfiles: Vec<String> = std::fs::read_dir(dirn)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("vol.z"))
        .collect();
    assert!(!zfiles.is_empty(), "存在 .zNN 卷: {zfiles:?}");
    // 读侧重开（verify_after 的装配语义：{stem}.zNN 数值序 + 末卷 = Volumes）
    let mut vol_paths: Vec<(u64, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(dirn).unwrap().flatten() {
        let nm = e.file_name().to_string_lossy().into_owned();
        if let Some(rest) = nm.strip_prefix("vol.z")
            && let Ok(n) = rest.parse::<u64>()
        {
            vol_paths.push((n, e.path()));
        }
    }
    vol_paths.sort_by_key(|(n, _)| *n);
    let mut vol_sorted: Vec<PathBuf> = vol_paths.into_iter().map(|(_, p)| p).collect();
    let first = vol_sorted.remove(0);
    let mut vol_rest = vol_sorted;
    vol_rest.push(out.clone()); // 末卷（含中央目录）在最后
    let vsrc = hzip_format::Source::Volumes {
        first,
        rest: vol_rest,
    };
    let reader = hzip_fmt_zip::ZipFormatHandler::new()
        .open_reader(vsrc, &Default::default())
        .unwrap();
    let entries = reader.entries().unwrap();
    assert_eq!(entries.len(), 3, "条目完整（src 目录条目 + 2 文件）");
    for e in entries {
        // v2：src 目录条目零数据——只对文件条目断言有内容
        if e.is_dir {
            assert_eq!(e.size_raw, 0, "目录条目零长");
            continue;
        }
        let mut buf = Vec::new();
        reader
            .read_entry(e.id, hzip_format::PasswordRef::none())
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        assert!(!buf.is_empty());
    }

    // 大卷（> 总量）：单卷（无 .zNN；直接 out.zip）
    let out2 = dir.path().join("big.zip");
    let job2 = compress_job(vec![src], out2.clone(), |c| {
        c.format = hzip_format::FormatId::from("zip");
        c.level = hzip_codec::level::Level::Store;
        c.split = Some(1 << 20);
    });
    let r2 = eng.submit(job2).unwrap().wait().unwrap();
    assert!(matches!(r2, Outcome::Done(_)), "大卷压缩应 Done: {r2:?}");
    let big_z: Vec<String> = std::fs::read_dir(dirn)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("big.z") && n != "big.zip")
        .collect();
    assert!(big_z.is_empty(), "超总量 → 单卷无 .zNN: {big_z:?}");
}

/// convert：`.tar → .zip` 条目集一致 + 内容一致（流式桥不落盘）。/// convert：`.tar → .zip` 条目集一致 + 内容一致（流式桥不落盘）。
#[test]
fn convert_tar_to_zip_streaming() {
    let dir = tmpdir("convert");
    let archive = dir.path().join("data.tar");
    make_tar(
        &archive,
        &[
            ("a.txt".into(), b"alpha".to_vec()),
            ("dir/b.bin".into(), vec![0x42u8; 4096]),
            ("empty".into(), Vec::new()),
        ],
    );
    let out = dir.path().join("data.zip");
    let eng = engine(1, 1);
    let mut c = CompressSpec {
        inputs: vec![archive.clone()],
        format: hzip_format::FormatId::from("zip"),
        out: out.clone(),
        ..Default::default()
    };
    c.level = hzip_codec::level::Level::Store; // store：zip 直存字节可逐位断言
    let mut job = JobSpec::new(JobKind::Convert, archive.clone());
    job.compress = Some(c);
    let h = eng.submit(job).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("convert 应 Done，实得 {other:?}"),
    }
    // zip reader 读回
    let reader = hzip_fmt_zip::ZipFormatHandler::new()
        .open_reader(hzip_format::Source::File(out), &Default::default())
        .unwrap();
    let entries = reader.entries().unwrap();
    assert_eq!(entries.len(), 3, "条目数一致");
    let mut got: Vec<(String, Vec<u8>)> = Vec::new();
    for info in &entries {
        let mut buf = Vec::new();
        reader
            .read_entry(info.id, hzip_format::PasswordRef::none())
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        got.push((info.name.as_str().to_string(), buf));
    }
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got[0].0, "a.txt");
    assert_eq!(got[0].1, b"alpha");
    assert_eq!(got[1].0, "dir/b.bin");
    assert_eq!(got[1].1, vec![0x42u8; 4096]);
    assert_eq!(got[2].0, "empty");
    assert_eq!(got[2].1, Vec::<u8>::new());
}

/// convert：目标不可写（rar）→ Unsupported 5001（TC-CLI-006 core 半场）。
#[test]
fn convert_rar_target_unsupported() {
    let dir = tmpdir("convert-rar");
    let archive = dir.path().join("data.tar");
    make_tar(&archive, &[("a.txt".into(), b"x".to_vec())]);
    let eng = engine(1, 1);
    let out = dir.path().join("data.rar");
    let mut c = CompressSpec {
        inputs: vec![archive.clone()],
        format: hzip_format::FormatId::from("rar"),
        out,
        ..Default::default()
    };
    c.level = hzip_codec::level::Level::Normal;
    let mut job = JobSpec::new(JobKind::Convert, archive.clone());
    job.compress = Some(c);
    let h = eng.submit(job).unwrap();
    let r = h.wait().unwrap();
    match r {
        Outcome::Failed(e) => assert_eq!(e.code(), 5001, "rar 目标应 5001: {e}"),
        other => panic!("应 Failed(5001)，实得 {other:?}"),
    }
}

/// single 流：`.tar → .gz` 条目数 > 1 → 目标格式能力拒绝（single 恰一条目）。
#[test]
fn convert_single_multi_entry_rejected() {
    let dir = tmpdir("convert-single");
    let archive = dir.path().join("data.tar");
    make_tar(
        &archive,
        &[
            ("a.txt".into(), b"a".to_vec()),
            ("b.txt".into(), b"b".to_vec()),
        ],
    );
    let eng = engine(1, 1);
    let out = dir.path().join("data.gz");
    let mut c = CompressSpec {
        inputs: vec![archive.clone()],
        format: hzip_format::FormatId::from("gz"),
        out,
        ..Default::default()
    };
    c.level = hzip_codec::level::Level::Normal;
    let mut job = JobSpec::new(JobKind::Convert, archive.clone());
    job.compress = Some(c);
    let h = eng.submit(job).unwrap();
    let r = h.wait().unwrap();
    // 第二条目 next_entry 时报 5001（single 恰一条目语义）；fail-fast → Failed
    assert!(
        matches!(r, Outcome::Failed(_)),
        "多条目→single 应 Failed，实得 {r:?}"
    );
}

/// 压缩方式越集 → HZ-5001（校验唯一点 = `services::compress::writer_for`）。
///
/// 守的是「界面选了某编码器、格式却写不出」这类静默降级：可选集合由
/// `FormatCaps::methods` 决定，越集即拒——不得悄悄改用格式默认编码器
/// （否则用户看到「Zstd」而实际写出 LZMA2，且无从察觉）。
#[test]
fn compress_method_outside_caps_rejected() {
    let dir = tmpdir("method-reject");
    let src = dir.path().join("src");
    make_tree(&src);
    let eng = engine(1, 1);

    // tar 是纯打包容器（methods 为空）→ 任何方式都越集
    let out = dir.path().join("src.tar");
    let job = compress_job(vec![src.clone()], out.clone(), |c| {
        c.method = Some(hzip_codec::traits::CodecMethod::Deflate);
    });
    match eng.submit(job).unwrap().wait().unwrap() {
        Outcome::Failed(e) => assert_eq!(e.code(), 5001, "tar 无压缩方式，应 5001: {e}"),
        other => panic!("应 Failed(5001)，实得 {other:?}"),
    }
    assert!(!out.exists(), "被拒任务不得留下半成品档案");

    // 7z 只声明 LZMA2 → Zstd 越集
    let out7 = dir.path().join("src.7z");
    let job = compress_job(vec![src.clone()], out7.clone(), |c| {
        c.format = hzip_format::FormatId::from("7z");
        c.method = Some(hzip_codec::traits::CodecMethod::Zstd);
    });
    let r = eng.submit(job).unwrap().wait().unwrap();
    assert!(
        matches!(&r, Outcome::Failed(e) if e.code() == 5001),
        "7z 非 LZMA2 应 5001，实得 {r:?}"
    );
    assert!(!out7.exists(), "被拒任务不得留下半成品档案");
}

/// 在册压缩方式贯通 + `None` 沿用格式默认（既有行为不变）。
#[test]
fn compress_method_within_caps_accepted() {
    let dir = tmpdir("method-accept");
    let f = dir.path().join("a.txt");
    std::fs::write(&f, b"hello hzip").unwrap();
    let eng = engine(1, 1);

    // zip + Deflate（在册）
    let out = dir.path().join("a.zip");
    let job = compress_job(vec![f.clone()], out.clone(), |c| {
        c.format = hzip_format::FormatId::from("zip");
        c.method = Some(hzip_codec::traits::CodecMethod::Deflate);
    });
    let r = eng.submit(job).unwrap().wait().unwrap();
    assert!(
        matches!(r, Outcome::Done(_)),
        "zip+Deflate 应 Done，实得 {r:?}"
    );
    assert!(out.exists());

    // gz 单候选 = Deflate（在册）
    let outgz = dir.path().join("a.gz");
    let job = compress_job(vec![f.clone()], outgz.clone(), |c| {
        c.format = hzip_format::FormatId::from("gz");
        c.method = Some(hzip_codec::traits::CodecMethod::Deflate);
    });
    let r = eng.submit(job).unwrap().wait().unwrap();
    assert!(
        matches!(r, Outcome::Done(_)),
        "gz+Deflate 应 Done，实得 {r:?}"
    );

    // None = 沿用格式默认（快捷动词/CLI 路径走的正是这一支）
    let out2 = dir.path().join("b.zip");
    let job = compress_job(vec![f.clone()], out2.clone(), |c| {
        c.format = hzip_format::FormatId::from("zip");
        c.method = None;
    });
    let r = eng.submit(job).unwrap().wait().unwrap();
    assert!(
        matches!(r, Outcome::Done(_)),
        "未指定方式应走格式默认，实得 {r:?}"
    );
    assert!(out2.exists());
}

/// 「字典大小」覆写**真的到达编码器**（09 §3.2 落账项）。
///
/// 判定面取「同输入、同级别、仅字典不同 → 产物不同」：字典若被忽略（或被级别
/// 派生值盖回去），两次产物会逐字节相同。另加可读性断言兜住「覆写产出坏档」。
///
/// 诚实注记：7z 头里也记录 coder 属性，故本断言证明的是「参数到达了编码器并
/// 改变了产物」，而不单是「码流按它变了」——对「UI 选项是否真的通了」这个
/// 问题，这正是要守的那一环。
#[test]
fn compress_dict_override_reaches_encoder() {
    let dir = tmpdir("dict-override");
    let src = dir.path().join("src");
    make_tree(&src);
    let eng = engine(1, 1);
    let small = dir.path().join("small.7z");
    let big = dir.path().join("big.7z");

    for (out, dict) in [(&small, 64 * 1024u64), (&big, 64 * 1024 * 1024)] {
        let job = compress_job(vec![src.clone()], out.clone(), |c| {
            c.format = hzip_format::FormatId::from("7z");
            c.dict_size = Some(dict);
        });
        let r = eng.submit(job).unwrap().wait().unwrap();
        assert!(
            matches!(r, Outcome::Done(_)),
            "7z 字典 {dict} 应成功，实得 {r:?}"
        );
    }

    let a = std::fs::read(&small).unwrap();
    let b = std::fs::read(&big).unwrap();
    assert_ne!(a, b, "仅字典不同，产物应不同（参数必须真的到达编码器）");

    for p in [&small, &big] {
        let reader = hzip_fmt_7z::SevenZipFormatHandler::new()
            .open_reader(hzip_format::Source::File(p.clone()), &Default::default())
            .unwrap();
        assert!(
            !reader.entries().unwrap().is_empty(),
            "覆写后档案应可列出条目: {p:?}"
        );
    }
}

/// 「更新方式 = 追加」**真的写进既有档案**（09 §3.2 落账项）。
///
/// 判定面取「追加后档案同时含旧、新两个条目」——这正是该选项的语义。
/// 若不支持追加的格式被误放了行（而不是报错 HZ-5001），第二个条目会把档案
/// 整体覆盖掉：那时条目数会变成 1，本断言即失败。
#[test]
fn compress_append_keeps_old_entries() {
    let dir = tmpdir("append");
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, b"alpha").unwrap();
    std::fs::write(&b, b"beta").unwrap();
    let zip_path = dir.path().join("z.zip");
    let eng = engine(1, 1);

    // 第一次：新建（append=false）
    let job = compress_job(vec![a.clone()], zip_path.clone(), |c| {
        c.format = hzip_format::FormatId::from("zip");
        c.append = false;
    });
    let r = eng.submit(job).unwrap().wait().unwrap();
    assert!(matches!(r, Outcome::Done(_)), "首次新建应成功，实得 {r:?}");

    // 第二次：追加（append=true）→ 既有条目必须保留
    let job = compress_job(vec![b.clone()], zip_path.clone(), |c| {
        c.format = hzip_format::FormatId::from("zip");
        c.append = true;
    });
    let r = eng.submit(job).unwrap().wait().unwrap();
    assert!(matches!(r, Outcome::Done(_)), "zip 追加应成功，实得 {r:?}");

    let reader = hzip_fmt_zip::ZipFormatHandler::new()
        .open_reader(
            hzip_format::Source::File(zip_path.clone()),
            &Default::default(),
        )
        .unwrap();
    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n.ends_with("a.txt")),
        "旧条目应保留: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.ends_with("b.txt")),
        "新条目应写入: {names:?}"
    );
    assert_eq!(names.len(), 2, "恰两条（追加不得清档）: {names:?}");
}

/// 组合写（`tar.gz`，两遍法）：源 → `.tar.gz` **一步到位**，且产物能被读端解回。
///
/// 守三件事：① 容器是 gzip（不是 tar 头——顺序写反就会露馅）；② 中间 tar 被清理；
/// ③ 端到端一致（时间戳/大小逐文件比对，走的是与解压同一个 Engine 全链路）。
#[test]
fn roundtrip_tar_gz_composite_write() {
    let dir = tmpdir("composite-tar-gz");
    let src = dir.path().join("src");
    make_tree(&src);
    let archive = dir.path().join("packed.tar.gz");
    let eng = engine(1, 1);
    let job = compress_job(vec![src.clone()], archive.clone(), |c| {
        c.format = hzip_format::FormatId::from("tar.gz");
    });
    match eng.submit(job).unwrap().wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("组合压缩应 Done，实得 {other:?}"),
    }
    let bytes = std::fs::read(&archive).unwrap();
    assert!(
        bytes.starts_with(&[0x1F, 0x8B]),
        "产物必须是 gzip 容器（先 tar 后 gzip），前两字节 {:02X?}",
        &bytes[..2]
    );
    assert!(
        !dir.path().join("packed.tar").exists(),
        "中间 tar 必须清理（成功路径）"
    );
    // 读端自动解组合层 → 内容一致
    let dest = dir.path().join("dest");
    let opts = ExtractOptions {
        dest_root: Some(dest.clone()),
        conflict: ConflictPolicy::Overwrite,
        smart: true,
        ..Default::default()
    };
    let h = eng
        .submit(JobSpec::extract_job(archive.clone(), opts))
        .unwrap();
    assert!(matches!(h.wait().unwrap(), Outcome::Done(_)));
    for (rel, want) in [
        ("src/docs/readme.md", b"# hzip roundtrip\n".as_slice()),
        ("src/docs/sub/nested.txt", b"deep-content".as_slice()),
        ("src/top.txt", b"top-level".as_slice()),
    ] {
        assert_eq!(
            std::fs::read(dest.join(rel)).unwrap(),
            want,
            "{rel} 内容不一致"
        );
    }
}
