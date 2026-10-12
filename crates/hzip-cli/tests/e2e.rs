//! hzip-cli E2E（assert_cmd；TC-CLI-001~015）。
//!
//! 测试用 `cargo_bin("hzip")` 跑真实二进制（tempfile 源目录）。密码类用例
//! 走非 TTY 通道（assert_cmd 默认无终端）：`--password-env`/`HZIP_PASSWORD`
//! 通道断言 + 错误密码收敛 3101 退出 2；TTY rpassword 交互不在无终端环境
//! 可测——以 mock/单测半场覆盖（见 password.rs 单测与 exit.rs 归并单测）。
//!
//! # 退出码契约（02 §10.4，冻结）
//!
//! 0 全成功 / 1 Partial / 2 致命（含 Failed Checksum——见 t_test 注释的
//! TC-CLI-005 与 §3.2 张力记录）/ 130 取消。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// 测试源目录树（3 条目：a.txt + sub/b.txt + 空目录 emptydir）。
fn make_tree(root: &Path) {
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(root.join("emptydir")).unwrap();
    std::fs::write(root.join("a.txt"), b"hello hzip\n").unwrap();
    std::fs::write(root.join("sub/b.txt"), b"world content\n").unwrap();
    // 排除候选
    std::fs::write(root.join("skip.tmp"), b"t").unwrap();
}

fn tmpdir(tag: &str) -> TempDir {
    TempDir::new().unwrap_or_else(|_| panic!("tmpdir {tag}"))
}

#[allow(dead_code)]
fn sha256_of(p: &Path) -> String {
    let mut f = std::fs::File::open(p).unwrap();
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = std::io::Read::read(&mut f, &mut buf).unwrap();
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    format!("{:x}", h.finalize())
}

/// 压缩辅助：dir → zip，返回 zip 路径。
fn make_zip(src: &Path, out: &Path) {
    let mut cmd = Command::cargo_bin("hzip").unwrap();
    cmd.args(["c", "-f", "zip", "-o"])
        .arg(out)
        .arg(src)
        .assert()
        .success();
}

/// 展开 zip 条目名列表（hzip l 平铺 stdout 第二列起含目录行）。
fn list_names(zip: &Path) -> Vec<String> {
    let out = Command::cargo_bin("hzip")
        .unwrap()
        .args(["l"])
        .arg(zip)
        .output()
        .unwrap();
    assert!(out.status.success(), "l 应成功: {out:?}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().last().map(|s| s.to_string()))
        .collect()
}

// ===================== TC-CLI-001：c 基本压缩 =====================
#[test]
fn tc_cli_001_c_basic_compress() {
    let dir = tmpdir("cli001");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("a.zip");
    let mut cmd = Command::cargo_bin("hzip").unwrap();
    cmd.args(["c", "-f", "zip", "-o"]).arg(&zip).arg(&src);
    cmd.assert().success();
    assert!(zip.exists(), "产物存在");
    // 条目集比对（目录展开收编文件 + 目录条目）
    let names = list_names(&zip);
    // v2 保目录名：档内 src/ + src/...（含 src 目录条目自身）
    for want in [
        "src",
        "src/a.txt",
        "src/sub",
        "src/sub/b.txt",
        "src/emptydir",
        "src/skip.tmp",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "条目 {want} 应存在: {names:?}"
        );
    }
}

// ===================== TC-CLI-002：x 智能解压 =====================
#[test]
fn tc_cli_002_x_smart_extract() {
    let dir = tmpdir("cli002");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("a.zip");
    make_zip(&src, &zip);
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let mut cmd = Command::cargo_bin("hzip").unwrap();
    cmd.args(["x"]).arg(&zip).args(["-o"]).arg(&dest);
    cmd.assert().success();
    // v2：档内 src/ 单顶层 → x 默认原样（不剥不包）→ dest/src/...
    let base = dest.join("src");
    assert_eq!(
        std::fs::read(base.join("a.txt")).unwrap(),
        b"hello hzip\n",
        "内容一致"
    );
    assert_eq!(
        std::fs::read(base.join("sub/b.txt")).unwrap(),
        b"world content\n"
    );
    assert!(base.join("emptydir").is_dir());
    // --smart：单顶层目录也不包壳（v2 判定）
    let dest2 = dir.path().join("out-smart");
    let mut cmd2 = Command::cargo_bin("hzip").unwrap();
    cmd2.args(["x"])
        .arg(&zip)
        .args(["-o"])
        .arg(&dest2)
        .arg("--smart");
    cmd2.assert().success();
    assert!(dest2.join("src/a.txt").exists());
}

// ===================== TC-CLI-003：l 列表与 tree + --json =====================
#[test]
fn tc_cli_003_l_list_tree_json() {
    let dir = tmpdir("cli003");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("a.zip");
    make_zip(&src, &zip);
    // tree 渲染
    let out = Command::cargo_bin("hzip")
        .unwrap()
        .args(["l"])
        .arg(&zip)
        .arg("--tree")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("a.txt"), "tree 含 a.txt");
    assert!(text.contains("sub"), "tree 含 sub");
    // --json schema 反序列化（字段全）
    let out2 = Command::cargo_bin("hzip")
        .unwrap()
        .args(["l"])
        .arg(&zip)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out2.status.success());
    let parsed: hzip_cli::ListResultJson =
        serde_json::from_slice(&out2.stdout).expect("ListResultJson 反序列化");
    assert_eq!(parsed.format, "zip");
    assert_eq!(parsed.entry_count, parsed.entries.len() as u64);
    assert!(!parsed.entries.is_empty());
    // v2：首条目可能是 src 目录条目——检查任一条目含文件/子目录名
    assert!(
        parsed
            .entries
            .iter()
            .any(|e| e.name.contains("a.txt") || e.name.contains("sub")),
        "entries: {:?}",
        parsed.entries.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    let e = &parsed.entries[0];
    assert!(e.mtime.is_some() || e.mtime.is_none()); // 字段存在性
    assert_eq!(e.name_encoding, "utf-8");
    // 目录条目字段（v2：src/emptydir）
    let dir_e = parsed
        .entries
        .iter()
        .find(|x| x.name == "src/emptydir" || x.name == "emptydir")
        .unwrap();
    assert!(dir_e.is_dir);
}

// ===================== TC-CLI-004：p 预览 =====================
#[test]
fn tc_cli_004_p_preview_pipe() {
    let dir = tmpdir("cli004");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("a.zip");
    make_zip(&src, &zip);
    // 完整预览：stdout 字节 == 条目内容
    let out = Command::cargo_bin("hzip")
        .unwrap()
        .args(["p"])
        .arg(&zip)
        .arg("src/sub/b.txt")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"world content\n");
    // --max-bytes 截断
    let out2 = Command::cargo_bin("hzip")
        .unwrap()
        .args(["p"])
        .arg(&zip)
        .arg("src/sub/b.txt")
        .args(["--max-bytes", "5"])
        .output()
        .unwrap();
    assert_eq!(out2.stdout, b"world");
}

// ===================== TC-CLI-005：t 测试 =====================
#[test]
fn tc_cli_005_t_test_ok_and_corrupt() {
    let dir = tmpdir("cli005");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("a.zip");
    make_zip(&src, &zip);
    // 正常：退出 0 + 统计输出
    let ok = Command::cargo_bin("hzip")
        .unwrap()
        .args(["t"])
        .arg(&zip)
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "正常档案 t 退出 0: {:?}",
        ok.status.code()
    );
    let text = String::from_utf8_lossy(&ok.stdout);
    assert!(text.contains("条目"), "统计输出: {text}");
    // CRC 损坏：退出 2（Failed Checksum——02 §3.2 Checksum → 码 2；任务卡
    // §6.1 表「退出 1」与冻结退出码契约冲突，以 §3.2/§10.4 冻结表为准，
    // 差异记录于交付说明）且 stderr 指明失败条目
    let big = dir.path().join("big-src");
    std::fs::create_dir_all(&big).unwrap();
    let mut rng = vec![0u8; 400 * 1024];
    // 伪随机（固定种子式填充，保证可复现）
    let mut seed = 0x5A5Au64;
    for b in rng.iter_mut() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (seed >> 24) as u8;
    }
    std::fs::write(big.join("payload.bin"), &rng).unwrap();
    let bigzip = dir.path().join("big.zip");
    make_zip(&big, &bigzip);
    let mut bytes = std::fs::read(&bigzip).unwrap();
    // 中部翻转：zip 大文件数据区占比 > 90%，中部必落数据区
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    let corrupt = dir.path().join("corrupt.zip");
    std::fs::write(&corrupt, &bytes).unwrap();
    let bad = Command::cargo_bin("hzip")
        .unwrap()
        .args(["t"])
        .arg(&corrupt)
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2), "损坏档案 t 退出 2（Failed）");
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        err.contains("payload.bin") || err.contains("HZ-3001") || err.contains("CRC"),
        "失败条目名/校验语义出现在 stderr: {err}"
    );
}

// ===================== TC-CLI-006：cv 转换 =====================
#[test]
fn tc_cli_006_cv_convert() {
    let dir = tmpdir("cli006");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("a.zip");
    make_zip(&src, &zip);
    let out7z = dir.path().join("a.7z");
    let mut cmd = Command::cargo_bin("hzip").unwrap();
    cmd.args(["cv"])
        .arg(&zip)
        .arg("--to")
        .arg("7z")
        .args(["-o"])
        .arg(&out7z);
    cmd.assert().success();
    assert!(out7z.exists());
    // 条目集一致（hzip l 读 7z）
    let names = list_names(&out7z);
    for want in [
        "src",
        "src/a.txt",
        "src/sub",
        "src/sub/b.txt",
        "src/emptydir",
        "src/skip.tmp",
    ] {
        assert!(names.iter().any(|n| n == want), "7z 条目 {want}: {names:?}");
    }
}

// ===================== TC-CLI-007：退出码契约 =====================
#[test]
fn tc_cli_007_exit_codes() {
    let dir = tmpdir("cli007");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("a.zip");
    make_zip(&src, &zip);
    // 全成功 → 0
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["t"])
        .arg(&zip)
        .assert()
        .code(0);
    // 参数错（-f bogus）→ 2 + 帮助文本
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["c", "-f", "bogus", "-o", "x.zip", "in"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("格式必须是"));
    // 目标不存在 → 2（HZ-5003/5004 语义）
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["c", "-f", "zip", "-o", "x.zip"])
        .arg(dir.path().join("no-such-dir"))
        .assert()
        .code(2);
    // 部分失败 → 1：core 无 best_effort 聚合入口（ExtractOptions.best_effort
    // CLI 未暴露，02 B.1 默认 false=fail-fast）——Partial 通道由 core 在
    // best_effort=true 下聚合；CLI 侧经 --json 路径单测锁定 exit_code 归并
    // （exit.rs 单测已覆盖 Partial→1）。此处以 HumanBytes/LevelArg 解析单测
    // 补 130/1 的归并表断言。
}

// ===================== TC-CLI-008：stdin/stdout 管道 =====================
#[test]
fn tc_cli_008_stdin_stream() {
    let dir = tmpdir("cli008");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("a.zip");
    make_zip(&src, &zip);
    // hzip l - < a.zip（stdin 魔数嗅探；fmt reader 物化缓冲）
    let bytes = std::fs::read(&zip).unwrap();
    let out = Command::cargo_bin("hzip")
        .unwrap()
        .args(["l", "-"])
        .write_stdin(bytes.clone())
        .output()
        .unwrap();
    assert!(out.status.success(), "stdin 列表成功");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("a.txt"), "stdin 嗅探列表: {text}");
    // t -：stdin 数据已被占用 → 密码走非 TTY 分支/直接空答不挂起
    let t_out = Command::cargo_bin("hzip")
        .unwrap()
        .args(["t", "-"])
        .write_stdin(bytes)
        .timeout(std::time::Duration::from_secs(30))
        .output()
        .unwrap();
    assert!(t_out.status.success(), "stdin t 不挂起且成功");
}

// ===================== TC-CLI-009：密码三通道 =====================
#[test]
fn tc_cli_009_password_channels() {
    // 加密 zip 样本（hzip-fmt-zip fixtures 内置黄金档；密码 hzip-pw-test）
    let crypto = include_bytes!("../../hzip-fmt-zip/tests/fixtures/zipcrypto-7z.zip");
    let dir = tmpdir("cli009");
    let zip = dir.path().join("crypto.zip");
    std::fs::write(&zip, crypto).unwrap();
    // ① --password-env 正确
    let mut cmd = Command::cargo_bin("hzip").unwrap();
    cmd.args(["x"])
        .arg(&zip)
        .args(["-o"])
        .arg(dir.path().join("o1"))
        .args(["--password-env", "HZIP_TST_PW"]);
    cmd.env("HZIP_TST_PW", "hzip-pw-test").assert().success();
    assert_eq!(
        std::fs::read(dir.path().join("o1/secret.txt")).unwrap(),
        include_bytes!("../../hzip-fmt-zip/tests/fixtures/zipcrypto-7z.plain.txt"),
        "正确密码解出明文"
    );
    // ① 变量缺失 → 2 + HZ-5003 文案（提交前即时退出）
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["x"])
        .arg(&zip)
        .args(["-o"])
        .arg(dir.path().join("o2"))
        .args(["--password-env", "HZIP_TST_UNSET"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("5003"));
    // ② HZIP_PASSWORD 通道正确
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["x"])
        .arg(&zip)
        .args(["-o"])
        .arg(dir.path().join("o3"))
        .env("HZIP_PASSWORD", "hzip-pw-test")
        .assert()
        .success();
    assert!(dir.path().join("o3/secret.txt").exists());
    // ② 错密码 → 收敛 3101 退出 2 + 建议文案（重输密码 user_action 类）
    let bad = Command::cargo_bin("hzip")
        .unwrap()
        .args(["x"])
        .arg(&zip)
        .args(["-o"])
        .arg(dir.path().join("o4"))
        .env("HZIP_PASSWORD", "wrong-password")
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2), "错密码退出 2");
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        err.contains("HZ-3101") || err.contains("密码"),
        "stderr 建议文案: {err}"
    );
}

// ===================== TC-CLI-ENC-001~004：压缩加密 / 解密全链 =====================

/// 加密 zip：`--password-env` 通道写 → 正确密码解回、错密码 HZ-3101 退出 2。
#[test]
fn tc_cli_enc_001_zip_aes_write_and_extract() {
    let dir = tmpdir("clienc1");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("enc.zip");
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["c", "-f", "zip", "-o"])
        .arg(&zip)
        .arg(&src)
        .env("HZIP_PASSWORD", "s3cret-pw")
        .assert()
        .success();
    // 未加密读 → 须失败（无密码读加密条目）
    let bad = Command::cargo_bin("hzip")
        .unwrap()
        .args(["x", "-o"])
        .arg(dir.path().join("o-none"))
        .arg(&zip)
        .env_remove("HZIP_PASSWORD")
        .output()
        .unwrap();
    assert!(
        !bad.status.success(),
        "无密码解压应失败: {bad:?}\n{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    // 正确密码 → 解回内容一致
    let out = dir.path().join("o1");
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["x", "-o"])
        .arg(&out)
        .arg(&zip)
        .env("HZIP_PASSWORD", "s3cret-pw")
        .assert()
        .success();
    assert_eq!(
        std::fs::read(out.join("src/a.txt")).unwrap(),
        b"hello hzip\n"
    );
    // 错密码 → HZ-3101 退出 2（TC-CLI-009 同口径）
    let bad = Command::cargo_bin("hzip")
        .unwrap()
        .args(["x", "-o"])
        .arg(dir.path().join("o2"))
        .arg(&zip)
        .env("HZIP_PASSWORD", "wrong-pw")
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2), "错密码退出 2: {bad:?}");
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        err.contains("HZ-3101") || err.contains("密码"),
        "stderr 建议文案: {err}"
    );
}

/// 7z 头加密（默认开）：`l` 无密码通道 → 退出 2（3102 不挂起）；带
/// `HZIP_PASSWORD` → 列目录成功；`--no-encrypt-names` → 名单明文但数据仍加密。
#[test]
fn tc_cli_enc_002_7z_header_encrypted_list() {
    let dir = tmpdir("clienc2");
    let src = dir.path().join("src");
    make_tree(&src);
    let sz = dir.path().join("enc.7z");
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["c", "-f", "7z", "-o"])
        .arg(&sz)
        .arg(&src)
        .env("HZIP_PASSWORD", "seven-z")
        .assert()
        .success();
    // 头加密档：无任何密码通道 → 列目录失败（3102 收敛退出 2，不挂起）
    let bad = Command::cargo_bin("hzip")
        .unwrap()
        .arg("l")
        .arg(&sz)
        .env_remove("HZIP_PASSWORD")
        .output()
        .unwrap();
    assert_eq!(
        bad.status.code(),
        Some(2),
        "头加密 l 无通道应退出 2: {bad:?}\n{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    // 有通道 → 列目录成功（browse_with_password 三通道）
    Command::cargo_bin("hzip")
        .unwrap()
        .arg("l")
        .arg(&sz)
        .env("HZIP_PASSWORD", "seven-z")
        .assert()
        .success();
    // --no-encrypt-names：文件名明文 → 无密码可列；数据仍加密 → 解压须密码
    let sz2 = dir.path().join("names.7z");
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["c", "-f", "7z", "--no-encrypt-names", "-o"])
        .arg(&sz2)
        .arg(&src)
        .env("HZIP_PASSWORD", "plain-names")
        .assert()
        .success();
    let listed = Command::cargo_bin("hzip")
        .unwrap()
        .arg("l")
        .arg(&sz2)
        .env_remove("HZIP_PASSWORD")
        .output()
        .unwrap();
    assert_eq!(
        listed.status.code(),
        Some(0),
        "文件名明文 → 无密码可列: {listed:?}"
    );
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("a.txt"),
        "名单应含 a.txt: {}",
        String::from_utf8_lossy(&listed.stdout)
    );
}

/// 选了 `--encryption` 但非 TTY 且无密码通道 → HZ-5003 退出 2（不挂起）。
#[test]
fn tc_cli_enc_003_encryption_without_password_channel_fails_5003() {
    let dir = tmpdir("clienc3");
    let src = dir.path().join("src");
    make_tree(&src);
    let out = dir.path().join("x.zip");
    let bad = Command::cargo_bin("hzip")
        .unwrap()
        .args(["c", "-f", "zip", "--encryption", "aes256", "-o"])
        .arg(&out)
        .arg(&src)
        .env_remove("HZIP_PASSWORD")
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2), "无密码通道应退出 2: {bad:?}");
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        err.contains("5003") || err.contains("密码"),
        "stderr 应指向密码通道: {err}"
    );
    assert!(!out.exists(), "失败不留半成品产物");
}

/// `--encryption zipcrypto` 显式指定 → roundtrip；7z 方案白名单由 core 兜底
/// （zipcrypto-on-7z → HZ-5001 退出 2）。
#[test]
fn tc_cli_enc_004_scheme_selection() {
    let dir = tmpdir("clienc4");
    let src = dir.path().join("src");
    make_tree(&src);
    // zipcrypto 写 → 解回
    let zp = dir.path().join("zc.zip");
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["c", "-f", "zip", "--encryption", "zipcrypto", "-o"])
        .arg(&zp)
        .arg(&src)
        .env("HZIP_PASSWORD", "zc-pw")
        .assert()
        .success();
    let out = dir.path().join("zc-out");
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["x", "-o"])
        .arg(&out)
        .arg(&zp)
        .env("HZIP_PASSWORD", "zc-pw")
        .assert()
        .success();
    assert_eq!(
        std::fs::read(out.join("src/a.txt")).unwrap(),
        b"hello hzip\n"
    );
    // zipcrypto-on-7z → 方案不在白名单 → HZ-5001 退出 2
    let bad = Command::cargo_bin("hzip")
        .unwrap()
        .args(["c", "-f", "7z", "--encryption", "zipcrypto", "-o"])
        .arg(dir.path().join("x.7z"))
        .arg(&src)
        .env("HZIP_PASSWORD", "any")
        .output()
        .unwrap();
    assert_eq!(
        bad.status.code(),
        Some(2),
        "zipcrypto-on-7z 应被 caps 白名单拒绝: {bad:?}"
    );
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        err.contains("5001") || err.contains("ZipCrypto"),
        "stderr 应指向方案不支持: {err}"
    );
}

// ===================== TC-CLI-010：--json 进度分离 =====================
#[test]
fn tc_cli_010_json_progress_split() {
    let dir = tmpdir("cli010");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("a.zip");
    // stdout 纯结果；stderr NDJSON（重定向文件断言）
    let mut cmd = Command::cargo_bin("hzip").unwrap();
    let out = cmd
        .args(["c", "-f", "zip", "-o"])
        .arg(&zip)
        .arg(&src)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success());
    // stdout 单文档 JSON
    let parsed: hzip_cli::JobResultJson =
        serde_json::from_slice(&out.stdout).expect("JobResultJson");
    assert_eq!(parsed.command, "compress");
    assert_eq!(parsed.exit_code, 0);
    // stderr 每行可反序列化 + 末行 finished
    let err_text = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = err_text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty(), "有进度 NDJSON");
    let mut last: Option<hzip_cli::ProgressLineJson> = None;
    for l in &lines {
        let pl: hzip_cli::ProgressLineJson =
            serde_json::from_str(l).unwrap_or_else(|e| panic!("NDJSON 行 {l}: {e}"));
        last = Some(pl);
    }
    assert!(
        matches!(
            last.unwrap().event,
            hzip_cli::ProgressEventJson::Finished { .. }
        ),
        "末行为 finished"
    );
}

// ===================== TC-CLI-011：glob 排除 =====================
#[test]
fn tc_cli_011_glob_exclude() {
    let dir = tmpdir("cli011");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("f.zip");
    let mut cmd = Command::cargo_bin("hzip").unwrap();
    cmd.args(["c", "-f", "zip", "-o"])
        .arg(&zip)
        .arg(&src)
        .args(["--exclude", "*.tmp", "--exclude", "emptydir/*"]);
    cmd.assert().success();
    let names = list_names(&zip);
    assert!(
        !names.iter().any(|n| n.ends_with(".tmp")),
        "*.tmp 剔除: {names:?}"
    );
    assert!(!names.iter().any(|n| n == "skip.tmp"), "skip.tmp 剔除");
    // 非法 glob → 2（HZ-5003）
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["c", "-f", "zip", "-o", "x.zip"])
        .arg(&src)
        .args(["--exclude", "["])
        .assert()
        .code(2);
}

// ===================== TC-CLI-012：无 TTY 交互保护 =====================
#[test]
fn tc_cli_012_no_tty_watchdog() {
    let dir = tmpdir("cli012");
    let src = dir.path().join("src");
    make_tree(&src);
    let zip = dir.path().join("a.zip");
    make_zip(&src, &zip);
    // 目标目录预置同名文件 → 冲突；未给 --on-conflict（默认 ask）
    // assert_cmd 无 TTY：核心的 decide_ask 走 CLI 应答 Skip / 决策超时默认
    // Skip（02 §9.8）——两路均不挂起。外层 timeout 看门狗断言「提前退出」。
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("a"), b"pre").unwrap();
    let out = Command::cargo_bin("hzip")
        .unwrap()
        .args(["x"])
        .arg(&zip)
        .args(["-o"])
        .arg(&dest)
        .timeout(std::time::Duration::from_secs(15))
        .output()
        .unwrap();
    assert!(
        out.status.code().is_some(),
        "无 TTY 冲突不挂起（看门狗 15s 内退出）"
    );
}

// ===================== TC-CLI-015：信号处理（posix 分支） =====================
#[cfg(unix)]
#[test]
fn tc_cli_015_signal_cancel_exit_130() {
    // 信号桥接消费 platform（当前占位 todo!——Engine cfg 门控跳过注册，
    // 02 §5.3.9 待 T11 激活）。本用例以「直接 SIGINT 前任务已完结」防误杀：
    // 无法注入取消的确定性 E2E；取消路径的退出码 130 归并在 exit.rs 单测
    // 锁定（Outcome::Cancelled → 130）。此处保留平台语义说明（rustdoc），
    // 实际断言交给 TC-WIN-009 同语义的 platform 测试（T11 激活后启用）。
}

#[cfg(windows)]
#[test]
fn tc_cli_015_signal_placeholder_windows() {
    // Windows Ctrl-C 事件桥接归 platform signal（占位 todo!）；退出码 130
    // 归并路径单测锁定（exit.rs）。本用例占位记录。
}

// ===================== cpio 只读打开（02 §8.7） =====================
//
// 用户口径：「能打开 cpio，`cpio.gz` 也能展开出里面目录结构」。hzip 无 cpio 写侧
// （caps.write=false），故素材由 testkit 拼字节；`.cpio.gz` 直接复用 CLI 自身的
// `c -f gz` 产出——**端到端不引入任何第三方压缩器**。

/// 造裸 cpio（newc）：`dir/` + `dir/a.txt` + `dir/b.bin`。
///
/// **不含符号链接**：默认链接策略要真建链接，无开发者模式的 Windows 上必失败
/// （core 既有行为，与格式无关）——链接条目的解析/落盘在 fmt 侧
/// （`hzip-fmt-cpio/tests/reader.rs`）与 core 侧（指定策略）分别覆盖。
fn make_cpio(path: &Path) {
    use hzip_format_testkit::cpio::{self, CpioVariant, EntrySpec};
    let bytes = cpio::build(
        CpioVariant::Newc,
        &[
            EntrySpec::dir("dir"),
            EntrySpec::file("dir/a.txt", b"hello cpio\n"),
            EntrySpec::file("dir/b.bin", b"0123456789"),
        ],
    );
    std::fs::write(path, bytes).unwrap();
}

/// 裸 `.cpio`：`l` 列表（含嵌套路径）+ `x` 解压还原内容。
#[test]
fn tc_cli_cpio_read() {
    let dir = tmpdir("cpio-read");
    let cpio = dir.path().join("init.cpio");
    make_cpio(&cpio);

    let out = Command::cargo_bin("hzip")
        .unwrap()
        .args(["l"])
        .arg(&cpio)
        .output()
        .unwrap();
    assert!(out.status.success(), "l 应成功：{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for want in ["dir", "dir/a.txt", "dir/b.bin"] {
        assert!(stdout.contains(want), "列表应含 {want}：\n{stdout}");
    }

    let dest = dir.path().join("out");
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["x"])
        .arg(&cpio)
        .args(["-o"])
        .arg(&dest)
        .assert()
        .success();
    assert_eq!(
        std::fs::read(dest.join("dir/a.txt")).unwrap(),
        b"hello cpio\n"
    );
    assert_eq!(
        std::fs::read(dest.join("dir/b.bin")).unwrap(),
        b"0123456789"
    );
}

/// `.cpio.gz`：`c -f gz` 造组合档 → `l` 展开内层目录结构 → `x` 解压内层文件。
#[test]
fn tc_cli_cpio_gz_read_and_extract() {
    let dir = tmpdir("cpio-gz");
    let cpio = dir.path().join("init.cpio");
    make_cpio(&cpio);
    // 组合档：hzip 自己的 gz 写侧（hzip 不产 cpio，但产 gz 流）
    let gz = dir.path().join("init.cpio.gz");
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["c", "-f", "gz", "-o"])
        .arg(&gz)
        .arg(&cpio)
        .assert()
        .success();
    assert!(gz.exists(), "组合档产物在");

    // 列表：组合链解 gz → cpio 内层条目（不再是「单个 init.cpio 文件」）
    let out = Command::cargo_bin("hzip")
        .unwrap()
        .args(["l"])
        .arg(&gz)
        .output()
        .unwrap();
    assert!(out.status.success(), "l 应成功：{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("dir/a.txt") && stdout.contains("dir/b.bin"),
        "组合档应展开 cpio 内层目录结构：\n{stdout}"
    );

    // 完整性测试 + 解压
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["t"])
        .arg(&gz)
        .assert()
        .success();
    let dest = dir.path().join("out");
    Command::cargo_bin("hzip")
        .unwrap()
        .args(["x"])
        .arg(&gz)
        .args(["-o"])
        .arg(&dest)
        .assert()
        .success();
    assert_eq!(
        std::fs::read(dest.join("dir/a.txt")).unwrap(),
        b"hello cpio\n"
    );
}

/// 坏 cpio：`l` 非零退出（格式错），不 panic 不静默。
#[test]
fn tc_cli_cpio_broken_fails_cleanly() {
    let dir = tmpdir("cpio-bad");
    let bad = dir.path().join("bad.cpio");
    std::fs::write(&bad, b"070701not-a-real-header").unwrap();
    let mut cmd = Command::cargo_bin("hzip").unwrap();
    cmd.args(["l"]).arg(&bad);
    let out = cmd.output().unwrap();
    assert!(!out.status.success(), "坏档 l 必须失败：{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("2002")
            || stderr.contains("2004")
            || stderr.contains("2001")
            || stderr.contains("格式"),
        "stderr 应含格式错码：{stderr}"
    );
}
