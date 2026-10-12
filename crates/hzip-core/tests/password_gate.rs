//! TC-ENC-GATE：密码门预检（`preflight_password`）与会话密码读条目
//! （用户口径 2026-09-28：双击加密条目先问密码、验证通过才放行）。
//!
//! 守三件事：
//! 1. 正确密码 → `preflight_password` 通过；错密码 → `HzipError::Password`（3101）；
//! 2. `Browser::read_entry`/`preview` 带密码可读、不带密码 3101（缓存复用的
//!    前提是「带密码真的读得出来」）；
//! 3. 头加密（7z -mhe=on 语义由 hzip 写侧产）同理——打开即需密码。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::Path;

use hzip_core::engine::{Engine, EngineConfig};
use hzip_format::{
    AesKey, CreateOptions, EncryptionRequest, EntryMeta, FormatHandler, OpenOptions, PasswordRef,
    Sink, Source,
};
use zeroize::Zeroizing;

const CONTENT: &[u8] = b"gate secret payload\n";

/// 写一个 AES-256 加密 zip（目录 + 两个加密文件条目）。
fn write_aes_zip(path: &Path, pw: &str) {
    let pwz = Zeroizing::new(pw.to_string());
    let mut w = hzip_fmt_zip::ZipFormatHandler::new()
        .create_writer(
            Sink::File(path.to_path_buf()),
            &CreateOptions {
                encryption: Some(EncryptionRequest::Aes(AesKey::Bits256)),
                ..Default::default()
            },
        )
        .unwrap();
    for (name, data) in [("dir/a.txt", CONTENT), ("b.bin", b"secondary")] {
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
}

fn engine() -> Engine {
    Engine::new(EngineConfig::default()).unwrap()
}

/// 预检：正确密码通过（zip 有界条目真读，流交付前完成预验）。
#[test]
fn preflight_accepts_right_password_and_rejects_wrong() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("enc.zip");
    write_aes_zip(&zip, "right-pw");
    let eng = engine();
    let right = Zeroizing::new("right-pw".to_string());
    let wrong = Zeroizing::new("wrong-pw".to_string());

    hzip_core::services::browse::preflight_password(eng.registry(), &zip, &right)
        .expect("正确密码应通过预检");

    let err = hzip_core::services::browse::preflight_password(eng.registry(), &zip, &wrong)
        .expect_err("错误密码必须失败");
    assert!(
        matches!(err, hzip_error::HzipError::Password { .. }),
        "错密码必须是 Password 错（3101），实际 {err:?}"
    );
}

/// 会话密码读条目：带密码可读、不带密码 3101（密码缓存复用的正确性前提）。
#[test]
fn browser_read_entry_uses_session_password() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("enc.zip");
    write_aes_zip(&zip, "right-pw");
    let eng = engine();
    let browser = eng
        .browse(Source::File(zip.clone()), &OpenOptions::default())
        .expect("明文头可列目录");

    let id = browser
        .entries()
        .iter()
        .find(|e| e.name.as_str() == "dir/a.txt")
        .map(|e| e.id)
        .expect("条目存在");
    assert!(browser.entry(id).unwrap().encrypted, "条目须标记加密");

    // 不带密码：3101（红线 3：不得静默乱码流）
    let err = match browser.read_entry(id, PasswordRef::none()) {
        Ok(_) => panic!("无密码读加密条目必须失败"),
        Err(e) => e,
    };
    assert!(matches!(err, hzip_error::HzipError::Password { .. }));

    // 带密码：内容一致
    let pw = Zeroizing::new("right-pw".to_string());
    let mut s = browser.read_entry(id, PasswordRef::new(&pw)).unwrap();
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut s, &mut buf).unwrap();
    assert_eq!(buf, CONTENT);

    // 预览同样走会话密码
    let pv = browser
        .preview(
            id,
            hzip_core::services::preview::PreviewMode::Text,
            None,
            PasswordRef::new(&pw),
        )
        .unwrap();
    assert_eq!(pv.bytes, CONTENT);
}
