//! TC-ENC-7Z：写侧加密（头加密默认开 / `encrypt_names=false` 数据加密）→
//! 读侧接线验收（设计稿 11 §7 用例 4/5/6）：无密码 3102、带密码 roundtrip、
//! 错密码 Password 错、folder 级 `encrypted` 标志。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::path::PathBuf;

use hzip_error::HzipError;
use hzip_fmt_7z::SevenZipFormatHandler;
use hzip_format::{
    AesKey, CreateOptions, EncryptionRequest, EntryMeta, FormatHandler, OpenOptions, PasswordRef,
    Sink, Source,
};

const PAYLOAD: &[u8] = b"7z secret payload \xe4\xb8\xad\xe6\x96\x87";

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// 自清理临时目录。
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "hzip-7z-enc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn join(&self, n: &str) -> PathBuf {
        self.0.join(n)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 写加密 7z：`encrypt_names` None → 默认头加密（决策②）。
fn write_encrypted(path: &std::path::Path, encrypt_names: Option<bool>, pw: &str) {
    let pwz = zeroize::Zeroizing::new(pw.to_string());
    let h = SevenZipFormatHandler::new();
    let mut w = h
        .create_writer(
            Sink::File(path.to_path_buf()),
            &CreateOptions {
                encryption: Some(EncryptionRequest::Aes(AesKey::Bits256)),
                encrypt_names,
                ..Default::default()
            },
        )
        .unwrap();
    for (name, data, is_dir) in [
        ("d/", None::<&[u8]>, true),
        ("d/a.txt", Some(PAYLOAD), false),
        ("top.bin", Some(&[0u8, 1, 2, 3][..]), false),
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

/// 开档（带/不带 OpenOptions.password）。
fn open_with(
    path: &std::path::Path,
    pw: Option<&str>,
) -> Result<Box<dyn hzip_format::ArchiveReader>, HzipError> {
    let h = SevenZipFormatHandler::new();
    h.open_reader(
        Source::File(path.to_path_buf()),
        &OpenOptions {
            password: pw.map(|s| zeroize::Zeroizing::new(s.to_string())),
            ..Default::default()
        },
    )
}

/// TC-ENC-7Z-001：头加密（默认）——无密码开档 3102、错密码 Password 错、
/// 正确密码列出 + 读回一致 + 加密标志。
#[test]
fn header_encrypted_open_requires_password() {
    let d = Dir::new("hdr");
    let p = d.join("enc.7z");
    write_encrypted(&p, Some(true), "hdr-pw");

    let e = open_with(&p, None).err().expect("无密码开头部加密档应失败");
    assert!(
        matches!(e, HzipError::Password { .. }),
        "无密码开头加密档 → Password/HZ-3102: {e}（码 {}）",
        e.code()
    );

    let e = open_with(&p, Some("wrong"))
        .err()
        .expect("错密码开档应失败");
    assert!(
        matches!(e, HzipError::Password { .. }),
        "错密码开档 → Password 错: {e}（码 {}）",
        e.code()
    );

    let r = open_with(&p, Some("hdr-pw")).unwrap();
    let entries = r.entries().unwrap();
    let files: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();
    assert!(!files.is_empty(), "应列出条目");
    assert!(
        files.iter().all(|e| e.encrypted),
        "头加密档条目应带 encrypted 标志: {files:?}"
    );
    // 正确密码读回内容一致
    let pwz = zeroize::Zeroizing::new("hdr-pw".to_string());
    let mut found = false;
    for e in &files {
        let mut s = r.read_entry(e.id, PasswordRef::new(&pwz)).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        if e.name.as_str() == "d/a.txt" {
            assert_eq!(sha256_hex(&buf), sha256_hex(PAYLOAD), "内容 sha256 一致");
            found = true;
        }
    }
    assert!(found, "d/a.txt 应被读到");
}

/// TC-ENC-7Z-002：`encrypt_names=false` → 数据加密、档头明文（不带密码可
/// 开档列名），folder 级 encrypted 标志为真；读条目须密码、错密码 Password 错。
#[test]
fn data_encrypted_open_without_password_lists_names() {
    let d = Dir::new("data");
    let p = d.join("plain-hdr.7z");
    write_encrypted(&p, Some(false), "data-pw");

    let r = open_with(&p, None).unwrap();
    let entries = r.entries().unwrap();
    let files: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();
    assert!(
        files.iter().all(|e| e.encrypted),
        "数据加密（folder AES）→ 条目 encrypted 标志须为真（build_infos 接线）: {files:?}"
    );

    // 无密码读加密条目 → Password 错（非乱码）
    let e = r
        .read_entry(files[0].id, PasswordRef::none())
        .err()
        .expect("无密码读数据加密条目应失败");
    assert!(
        matches!(e, HzipError::Password { .. }),
        "无密码读数据加密条目 → Password: {e}"
    );

    // 错密码 → **整条读链必须失败且不产出数据**（红线 3）。
    //
    // 为什么不能只认 `Password`（2026-09-24 实测的抖动）：7z 的 AES-256 有随机
    // salt/IV，错密码解出的中间字节逐次不同 ⇒ 有时先撞上 AES 的 SHA-256 校验
    // （报 `Password`，HZ-3101），有时先撞上解压器的乱码流（报 `Format`，HZ-2004）。
    // 两者都守红线 3（禁静默乱码），故按「必须报错 + 绝不静默产出字节」断言；
    // 无密码路径的 `Password` 首选语义由上面的用例段钉死。
    let bad = zeroize::Zeroizing::new("nope".to_string());
    let outcome = {
        let read_all = || -> Result<Vec<u8>, HzipError> {
            let mut s = r.read_entry(files[0].id, PasswordRef::new(&bad))?;
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).map_err(HzipError::from)?;
            Ok(buf)
        };
        read_all()
    };
    match outcome {
        Err(HzipError::Password { .. }) | Err(HzipError::Format { .. }) => {}
        Err(other) => panic!("错密码应报 Password 或 Format 错：{other}"),
        Ok(buf) => panic!("错密码不得静默产出数据（红线 3）：{} 字节", buf.len()),
    }

    let ok = zeroize::Zeroizing::new("data-pw".to_string());
    let mut checked = false;
    for e in &files {
        let mut s = r.read_entry(e.id, PasswordRef::new(&ok)).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        if e.name.as_str() == "d/a.txt" {
            assert_eq!(sha256_hex(&buf), sha256_hex(PAYLOAD));
            checked = true;
        }
    }
    assert!(checked, "d/a.txt 应读回一致");
}

/// caps 写能力声明（设计稿 11 §3.1 B：7z 写 = AES-256 唯一）。
#[test]
fn sevenz_caps_declare_aes256_only() {
    let h = SevenZipFormatHandler::new();
    let caps = h.caps();
    assert!(
        caps.write_encryption
            .contains(&EncryptionRequest::Aes(AesKey::Bits256)),
        "7z 写白名单应含 AES-256"
    );
    assert!(
        !caps
            .write_encryption
            .contains(&EncryptionRequest::ZipCrypto),
        "7z 不支持 ZipCrypto"
    );
}
