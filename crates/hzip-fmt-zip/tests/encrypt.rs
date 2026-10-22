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

/// TC-ENC-ZIP：密码验证必须是 **O(1)**，不得解码整条条目。
///
/// 用户实测 2026-10-22：`qcow2.zip` 里 4.34 GB 的加密条目，密码门「验证通过」
/// 实际走了 `read_entry` 的 spool 两遍法——先把整条解码进临时盘（60 s），临时盘
/// 只剩 4.2 GB 时以 ENOSPC 收场，界面看起来「输完密码什么都没发生」。
///
/// 判据：256 MiB 全零条目（deflate 后极小）用 `verify_password` 验证，
/// 正确密码 → `Ok` 且**远快于整条解码**；错密码 → `Password`；验证后数据仍可读。
#[test]
fn verify_password_does_not_decode_the_whole_entry() {
    let dir = tmp_dir("enc-verify");
    let p = dir.join("big.zip");
    let pwz = zeroize::Zeroizing::new("verify-pw".to_string());
    const SIZE: u64 = 256 * 1024 * 1024;

    let mut w = handler()
        .create_writer(
            Sink::File(p.clone()),
            &CreateOptions {
                encryption: Some(EncryptionRequest::ZipCrypto),
                ..Default::default()
            },
        )
        .unwrap();
    let meta = EntryMeta {
        name: "disk.qcow2".into(),
        size_raw: Some(SIZE),
        is_dir: false,
        ..Default::default()
    };
    let mut s = w.next_entry(&meta, PasswordRef::new(&pwz)).unwrap();
    let chunk = vec![0u8; 1 << 20];
    for _ in 0..(SIZE / (1 << 20)) {
        s.write_all(&chunk).unwrap();
    }
    s.finish().unwrap();
    w.finish().unwrap();

    let r = handler()
        .open_reader(Source::File(p), &OpenOptions::default())
        .unwrap();
    let e = r
        .entries()
        .unwrap()
        .into_iter()
        .find(|e| e.name.as_str() == "disk.qcow2")
        .expect("条目应存在");
    assert!(e.encrypted, "fixture 必须加密");

    let started = std::time::Instant::now();
    r.verify_password(e.id, PasswordRef::new(&pwz))
        .expect("正确密码应通过");
    let took = started.elapsed();
    // 旧实现（read_entry + spool）要把 256 MiB 解码写盘（数百 ms 起）；
    // 新实现只校验加密头。留足余量给慢 CI。
    assert!(
        took < std::time::Duration::from_millis(300),
        "verify_password 解码了整条条目（{took:?}）：密码门会被大条目卡死"
    );

    let wrong = zeroize::Zeroizing::new("bad-pw".to_string());
    match r.verify_password(e.id, PasswordRef::new(&wrong)) {
        Err(HzipError::Password { .. }) => {}
        other => panic!("错密码必须报 Password，实际 {other:?}"),
    }

    // 验证不消费/破坏条目：正确密码仍能完整读出数据
    let mut s = r
        .read_entry(e.id, PasswordRef::new(&pwz))
        .expect("验证后仍应可读");
    let mut buf = vec![0u8; 1 << 20];
    assert_eq!(
        std::io::Read::read(&mut s, &mut buf).unwrap(),
        1 << 20,
        "验证后条目数据必须完整可读"
    );
}

/// `take_entry_file`：加密条目交出**已在盘上**的整条解码副本，供调用方移动
/// （避免 spool + 物化双份占用）；明文条目/错误密码/无密码一律 `None`。
#[test]
fn take_entry_file_hands_over_the_spooled_copy_only_for_encrypted_entries() {
    let dir = tmp_dir("take-entry");
    let p = dir.join("take.zip");
    let pwz = zeroize::Zeroizing::new("take-pw".to_string());
    let mut w = handler()
        .create_writer(
            Sink::File(p.clone()),
            &CreateOptions {
                encryption: Some(EncryptionRequest::ZipCrypto),
                ..Default::default()
            },
        )
        .unwrap();
    for (name, data) in [("a.bin", &b"hello-take"[..]), ("b.bin", &b"plain"[..])] {
        let meta = EntryMeta {
            name: name.into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::new(&pwz)).unwrap();
        s.write_all(data).unwrap();
        s.finish().unwrap();
    }
    w.finish().unwrap();

    let r = handler()
        .open_reader(Source::File(p), &OpenOptions::default())
        .unwrap();
    let entries = r.entries().unwrap();
    let a = entries
        .iter()
        .find(|e| e.name.as_str() == "a.bin")
        .expect("a.bin");

    // 无密码 / 错密码：不交出任何文件
    let mut seen = 0u64;
    let mut noop = |n: u64| seen = n;
    assert!(
        r.take_entry_file(a.id, PasswordRef::none(), &mut noop)
            .is_none()
    );
    let wrong = zeroize::Zeroizing::new("nope".to_string());
    assert!(
        r.take_entry_file(a.id, PasswordRef::new(&wrong), &mut noop)
            .is_none()
    );

    let sp = r
        .take_entry_file(a.id, PasswordRef::new(&pwz), &mut |n: u64| seen = n)
        .expect("正确密码应交出 spool 副本");
    assert_eq!(
        seen,
        b"hello-take".len() as u64,
        "必须报出解码进度（否则进度窗不动）"
    );
    assert!(sp.exists(), "交出的必须是真实文件：{}", sp.display());
    assert_eq!(
        std::fs::read(&sp).unwrap(),
        b"hello-take",
        "副本内容必须与条目一致"
    );
    // 交出后 reader 仍可重建副本（后续读取不依赖已交出的文件）
    let mut s = r
        .read_entry(a.id, PasswordRef::new(&pwz))
        .expect("交出后仍应可读");
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut s, &mut buf).unwrap();
    assert_eq!(buf, b"hello-take");
    std::fs::remove_file(&sp).ok();
}
