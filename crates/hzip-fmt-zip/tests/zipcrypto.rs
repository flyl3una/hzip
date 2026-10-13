//! TC-CONF-007 ZipCrypto 密码预置样本 + 错误密码 → Password 3101 / 正确密码读回。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Read;
use std::path::PathBuf;

use hzip_format::{FormatHandler, PasswordRef, Source};
use zeroize::Zeroizing;

// 黄金样本（7-Zip 24.07 生成）：
//   - zipcrypto-7z.zip：ZipCrypto + deflate 加密档案，密码 "hzip-pw-test"
//   - zipcrypto-7z.plain.txt：明文原文
const CRYPTO_ZIP: &[u8] = include_bytes!("fixtures/zipcrypto-7z.zip");
const CRYPTO_PLAIN: &[u8] = include_bytes!("fixtures/zipcrypto-7z.plain.txt");

const PW: &str = "hzip-pw-test";

/// 样本必须**逐字节**按仓库里的样子到位——EOL 转换会毁掉字节比对。
///
/// # 为什么单独立一条（2026-10-13）
///
/// `zipcrypto-7z.plain.txt` 是纯 ASCII，git 按「文本」入库；若 `.gitattributes`
/// 不标 `-text`，Windows 检出（`core.autocrlf=true`，GitHub 的 windows runner 即如此）
/// 会把它写成 CRLF，而档案内部存的是 **LF** ⇒ 下面 `zipcrypto_correct_password` 的
/// `assert_eq!(buf, CRYPTO_PLAIN)` 会以「左 LF / 右 CRLF」的几百字节数组差炸开，
/// 看起来像「解密错了」——极难查（CI 上表现为「只有 Windows 腿红」，Linux/macOS 不转换）。
/// 这里先把「样本被 EOL 转换了」这件事单独说清（e2e 侧同因的前置自检见
/// `hzip-cli/tests/e2e.rs::tc_cli_009_password_channels`）。
#[test]
fn fixture_is_delivered_verbatim() {
    assert!(
        !CRYPTO_PLAIN.contains(&b'\r'),
        "样本 zipcrypto-7z.plain.txt 被 EOL 转换成了 CRLF（档案里存的是 LF）——\
         检查 .gitattributes 是否覆盖 `crates/*/tests/fixtures/**`（标 `-text`）与 core.autocrlf；\
         已污染的检出先删文件再 `git checkout -- crates/hzip-fmt-zip/tests/fixtures/zipcrypto-7z.plain.txt`"
    );
    assert!(
        CRYPTO_PLAIN.ends_with(b"\n"),
        "样本应以换行结尾（防被编辑器/工具截断）"
    );
}

/// 固定密码源。
struct FixedPw(String);
impl hzip_format::PasswordSource for FixedPw {
    fn password(&self, _entry: Option<&hzip_format::EntryName>) -> Option<Zeroizing<String>> {
        Some(Zeroizing::new(self.0.clone()))
    }
}

fn write_fixture(bytes: &[u8]) -> PathBuf {
    let dir = common::tmp_dir("crypto");
    let p = dir.join("crypto.zip");
    std::fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn zipcrypto_wrong_password() {
    let p = write_fixture(CRYPTO_ZIP);
    let h = common::handler();
    let r = h
        .open_reader(Source::File(p.clone()), &Default::default())
        .unwrap();
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 1);
    assert!(infos[0].encrypted, "条目应标记加密");

    // 错误密码 → Password 3101（ZipCrypto 校验头预验；禁乱码流）
    let pw = Zeroizing::new("wrong-password".to_string());
    let res = r.read_entry(0, PasswordRef::new(&pw));
    let mut buf = Vec::new();
    let code = match res {
        Ok(mut rd) => match rd.read_to_end(&mut buf) {
            Ok(_) => None,
            Err(_) => Some(3101u32), // 读中密码预验/CRC 失败 → 归 3101 语义
        },
        Err(e) => Some(e.code()),
    };
    match code {
        Some(3101) => {}
        Some(other) => panic!("错误密码应 Password 3101，实得 {other}"),
        None => {
            // 1/256 假阳性：内容必须不是明文
            assert_ne!(buf, CRYPTO_PLAIN, "错误密码不得产出正确明文");
        }
    }
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn zipcrypto_correct_password() {
    let p = write_fixture(CRYPTO_ZIP);
    let h = common::handler();
    let r = h
        .open_reader(Source::File(p.clone()), &Default::default())
        .unwrap();
    let pw = Zeroizing::new(PW.to_string());
    let mut rd = r.read_entry(0, PasswordRef::new(&pw)).unwrap();
    let mut buf = Vec::new();
    rd.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, CRYPTO_PLAIN, "正确密码解压一致");

    // extract 路径（经 ctx.passwords 正确密码）
    let dest = common::tmp_dir("crypto-x").join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let r2 = h
        .open_reader(Source::File(p.clone()), &Default::default())
        .unwrap();
    let pw_src = FixedPw(PW.to_string());
    let env = common::TestEnv::new(&pw_src);
    let rep = r2
        .extract(&hzip_format::Selection::All, &env.ctx(), &dest)
        .unwrap();
    assert!(rep.failures.is_empty());
    let on_disk = std::fs::read(dest.join("secret.txt")).unwrap();
    assert_eq!(on_disk, CRYPTO_PLAIN);
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

// ---- test() 密码询问门控（2026-09-15 用户报障回归锚点）----
//
// 未加密条目不得发起密码询问（core PwRouter 会把它升级为弹窗请求，
// 无人应答 120 s → 任务超时取消）；加密条目**必须**询问（防线别矫枉过正）。

/// 计数密码源（恒 None；统计询问次数）。
struct CountPw(std::sync::atomic::AtomicUsize);
impl hzip_format::PasswordSource for CountPw {
    fn password(&self, _e: Option<&hzip_format::EntryName>) -> Option<Zeroizing<String>> {
        use std::sync::atomic::Ordering;
        self.0.fetch_add(1, Ordering::Relaxed);
        None
    }
}

/// 未加密档案 test()：密码询问次数必须为 0。
#[test]
fn test_unencrypted_zip_never_prompts_password() {
    let dir = common::tmp_dir("test-gate-plain");
    let p = dir.join("plain.zip");
    let h = common::handler();
    {
        use hzip_format::{EntryMeta, PasswordRef, Sink};
        use std::io::Write as _;
        let mut w = h
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        let meta = EntryMeta {
            name: "a.txt".into(),
            size_raw: Some(5),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(b"hello").unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
    }
    let r = h
        .open_reader(Source::File(p.clone()), &Default::default())
        .unwrap();
    let counter = CountPw(std::sync::atomic::AtomicUsize::new(0));
    let env = common::TestEnv::new(&counter);
    let report = r.test(&env.ctx()).unwrap();
    assert_eq!(report.entries_done, 1);
    let asked = counter.0.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(asked, 0, "未加密条目不应发起密码询问，实际 {asked} 次");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 加密档案 test()：密码询问必须 ≥1（仍走请求-应答链）。
#[test]
fn test_encrypted_zip_prompts_password() {
    let p = write_fixture(CRYPTO_ZIP);
    let h = common::handler();
    let r = h
        .open_reader(Source::File(p.clone()), &Default::default())
        .unwrap();
    assert!(r.entries().unwrap()[0].encrypted);
    let counter = CountPw(std::sync::atomic::AtomicUsize::new(0));
    let env = common::TestEnv::new(&counter);
    // CountPw 恒 None → fmt 层 Password 3101（条目加密而无密码）——
    // 关键断言是「询问发生过」，而非 test 成败
    let res = r.test(&env.ctx());
    let asked = counter.0.load(std::sync::atomic::Ordering::Relaxed);
    assert!(asked >= 1, "加密条目必须发起密码询问");
    assert!(res.is_err(), "无密码源时加密档案 test 应失败（3101）");
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}
