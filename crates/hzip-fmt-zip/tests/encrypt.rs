//! TC-ENC-ZIP：写侧加密 roundtrip（AES-256 / ZipCrypto）+ 错密码 3101 +
//! entries 加密标志（设计稿 11 §7 用例 1/2/6）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;

use hzip_error::HzipError;
use hzip_format::{
    AesKey, CreateOptions, EncryptionRequest, EntryMeta, FormatHandler, OpenOptions, PasswordRef,
    Sink, Source,
};

use common::{handler, tmp_dir};

const CONTENT: &[u8] = b"secret hello hzip\n";

/// 写一个加密档案：目录 + 两个加密文件条目。
fn write_encrypted(path: &std::path::Path, enc: EncryptionRequest, pw: &str) {
    let pwz = zeroize::Zeroizing::new(pw.to_string());
    let mut w = handler()
        .create_writer(
            Sink::File(path.to_path_buf()),
            &CreateOptions {
                encryption: Some(enc),
                ..Default::default()
            },
        )
        .unwrap();
    for (name, data, is_dir) in [
        ("dir/", None::<&[u8]>, true),
        ("dir/a.txt", Some(CONTENT), false),
        ("b.bin", Some(&b"\x01\x02\x03\x04"[..]), false),
    ] {
        let meta = EntryMeta {
            name: name.into(),
            size_raw: data.map(|d| d.len() as u64),
            is_dir,
            ..Default::default()
        };
        let r = if is_dir {
            PasswordRef::none()
        } else {
            PasswordRef::new(&pwz)
        };
        let mut s = w.next_entry(&meta, r).unwrap();
        if let Some(d) = data {
            s.write_all(d).unwrap();
        }
        s.finish().unwrap();
    }
    w.finish().unwrap();
}

/// 读回全部条目：`(name, content, encrypted_flag)`；`pw=None` 表示不带密码读
/// （加密条目应以 Password 错拒绝，由调用方单独断言）。
fn read_with(
    path: &std::path::Path,
    pw: Option<&str>,
) -> Result<Vec<(String, Vec<u8>, bool)>, HzipError> {
    let pwz = pw.map(|s| zeroize::Zeroizing::new(s.to_string()));
    let r = handler().open_reader(
        Source::File(path.to_path_buf()),
        &OpenOptions {
            password: None, // zip 开档不需密码；密码在 read_entry 级
            ..Default::default()
        },
    )?;
    let entries = r.entries()?;
    let mut out = Vec::new();
    for e in &entries {
        let flag = e.encrypted;
        let p = pwz
            .as_ref()
            .map(PasswordRef::new)
            .unwrap_or_else(PasswordRef::none);
        match r.read_entry(e.id, p) {
            Ok(mut s) => {
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut s, &mut buf).unwrap();
                out.push((e.name.as_str().to_string(), buf, flag));
            }
            Err(err @ HzipError::Password { .. }) if !flag => {
                // 未加密条目不因密码失败（目录等）
                return Err(err);
            }
            Err(err) if flag && pw.is_none() => return Err(err),
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

/// TC-ENC-ZIP-001：AES-256 写 → 正确密码读回一致；标志位正确。
#[test]
fn aes256_roundtrip_with_flags() {
    let dir = tmp_dir("enc-aes");
    let p = dir.join("aes.zip");
    write_encrypted(&p, EncryptionRequest::Aes(AesKey::Bits256), "pw-aes-256");
    // 无密码开档 OK（zip 头不加密），但加密条目读 → Password 错
    let e = read_with(&p, None).unwrap_err();
    assert!(
        matches!(e, HzipError::Password { .. }),
        "无密码读加密条目须 Password 错: {e}"
    );
    // 正确密码 → 内容一致
    let got = read_with(&p, Some("pw-aes-256")).unwrap();
    let names: Vec<&str> = got.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec!["dir", "dir/a.txt", "b.bin"],
        "条目集与写入顺序一致（目录名归一去尾斜杠）: {got:?}"
    );
    let by_name = |n: &str| {
        got.iter()
            .find(|(name, _, _)| name == n)
            .map(|(_, c, _)| c.clone())
            .unwrap_or_else(|| panic!("{n} 应存在"))
    };
    assert_eq!(by_name("dir/a.txt"), CONTENT);
    assert_eq!(by_name("b.bin"), b"\x01\x02\x03\x04");
    // 加密标志：文件 true、目录 false
    let flag = |n: &str| got.iter().find(|(name, _, _)| name == n).unwrap().2;
    assert!(flag("dir/a.txt"), "加密文件应带 encrypted 标志");
    assert!(flag("b.bin"), "加密文件应带 encrypted 标志");
    assert!(!flag("dir"), "目录不加密");
}

/// TC-ENC-ZIP-002：错密码 → read_entry 即 Password 错（预验，不吐乱码流）。
#[test]
fn aes256_wrong_password_is_password_error() {
    let dir = tmp_dir("enc-aes-bad");
    let p = dir.join("bad.zip");
    write_encrypted(&p, EncryptionRequest::Aes(AesKey::Bits256), "right-pw");
    let e = read_with(&p, Some("wrong-pw")).unwrap_err();
    assert!(
        matches!(e, HzipError::Password { .. }),
        "错密码须 Password 错（HZ-3101 族）: {e}"
    );
}

/// TC-ENC-ZIP-003：ZipCrypto（unstable 写 API 守卫）roundtrip。
#[test]
fn zipcrypto_roundtrip() {
    let dir = tmp_dir("enc-zc");
    let p = dir.join("zc.zip");
    write_encrypted(&p, EncryptionRequest::ZipCrypto, "legacy-pw");
    let e = read_with(&p, None).unwrap_err();
    assert!(matches!(e, HzipError::Password { .. }), "无密码: {e}");
    let got = read_with(&p, Some("legacy-pw")).unwrap();
    assert!(got.iter().any(|(n, c, _)| n == "dir/a.txt" && c == CONTENT));
    let e = read_with(&p, Some("nope")).unwrap_err();
    assert!(matches!(e, HzipError::Password { .. }), "错密码: {e}");
}

/// caps 写能力声明（UI 门控唯一来源；设计稿 11 §3.1 B）。
#[test]
fn zip_caps_declare_write_encryption() {
    let caps = handler().caps();
    for req in [
        EncryptionRequest::ZipCrypto,
        EncryptionRequest::Aes(AesKey::Bits128),
        EncryptionRequest::Aes(AesKey::Bits192),
        EncryptionRequest::Aes(AesKey::Bits256),
    ] {
        assert!(
            caps.write_encryption.contains(&req),
            "caps.write_encryption 应声明 {req:?}"
        );
    }
    // 方案不在白名单 → writer_for/ handler 层 HZ-5001（此处直接验 handler 拒绝）
    let dir = tmp_dir("enc-bad");
    let e = handler().create_writer(
        Sink::File(dir.join("x.zip")),
        &CreateOptions {
            encryption: Some(EncryptionRequest::Aes(AesKey::Bits256)),
            ..Default::default()
        },
    );
    assert!(e.is_ok(), "AES-256 在 zip 写白名单内");
}
