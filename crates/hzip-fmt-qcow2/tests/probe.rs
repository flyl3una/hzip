//! qcow2 探针：只认 `QFI\xfb` + 版本 2/3（**大端**），其余一律不认。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

use std::sync::Arc;

use hzip_fmt_qcow2::{PROBE_HEAD_BYTES, Qcow2Handler, qcow2_probe};
use hzip_format::{Confidence, FormatHandler as _};
use hzip_vol::registry::FsRegistry;

fn handler() -> Qcow2Handler {
    Qcow2Handler::new(Arc::new(FsRegistry::new()))
}

#[test]
fn magic_with_version_2_or_3_is_recognised() {
    assert_eq!(qcow2_probe(b"QFI\xfb\0\0\0\x03"), Some(Confidence::High));
    assert_eq!(qcow2_probe(b"QFI\xfb\0\0\0\x02"), Some(Confidence::High));
    // 版本是**大端**：v3 落盘是 `00 00 00 03`；小端读会读成 0x0300_0000
    assert_eq!(
        qcow2_probe(b"QFI\xfb\x03\0\0\0"),
        None,
        "反过来的字节序不是版本 3"
    );
}

#[test]
fn other_magic_version_or_short_head_is_not_qcow2() {
    assert_eq!(qcow2_probe(b"KDMV\0\0\0\0"), None); // VMDK
    assert_eq!(qcow2_probe(&[0u8; 32]), None); // 全零
    assert_eq!(qcow2_probe(b"QFI"), None, "头长不足 8 ⇒ 不猜");
    assert_eq!(qcow2_probe(&[]), None);
    assert_eq!(qcow2_probe(b"QFI\xfb\0\0\0\x04"), None, "版本 1/4 不认");
    assert_eq!(qcow2_probe(b"QFI\xfb\0\0\0\0"), None);
}

#[test]
fn handler_contract_matches_the_container() {
    let h = handler();
    assert_eq!(h.id().as_str(), "qcow2");
    assert_eq!(h.probe_head_bytes(), 512);
    assert_eq!(
        PROBE_HEAD_BYTES, 512,
        "探针窗口需覆盖 v3 头（含 compression_type）"
    );
    assert_eq!(h.signature().magic, b"QFI\xfb");
    assert_eq!(h.signature().offset, 0);
    assert!(h.signature().extensions.contains(&"qcow2"));
    // 探针转发：handler 与自由函数结论一致
    assert_eq!(h.probe(b"QFI\xfb\0\0\0\x03"), Some(Confidence::High));
    assert_eq!(h.probe(b"not a qcow2 file at all........"), None);
    assert!(
        h.supports_in_place_edit(),
        "qcow2 的写落在宿主簇上 ⇒ 可就地提交"
    );
    let caps = h.caps();
    assert!(caps.read && caps.edit);
}

#[test]
fn create_writer_is_refused_by_name() {
    let h = handler();
    let err = match h.create_writer(
        hzip_format::Sink::File(std::path::PathBuf::from("x.qcow2")),
        &Default::default(),
    ) {
        Ok(_) => panic!("qcow2 不产出镜像，应拒绝"),
        Err(e) => e,
    };
    assert!(
        matches!(err, hzip_error::HzipError::Unsupported { ref what } if what == "vol.unsupported.create-image"),
        "{err:?}"
    );
}
