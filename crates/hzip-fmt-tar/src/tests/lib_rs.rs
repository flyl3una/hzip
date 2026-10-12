use super::*;
use crate::consts::BLOCK_BYTES;
use hzip_format::FormatCaps;

/// 构造一个含指定 512B 头样本的数组（其余填零）。
fn head_at(magic: Option<&[u8]>, typeflag: u8) -> [u8; BLOCK_BYTES] {
    let mut h = [0u8; BLOCK_BYTES];
    if let Some(m) = magic {
        let off = crate::consts::TAR_MAGIC_OFFSET;
        h[off..off + m.len()].copy_from_slice(m);
    }
    h[156] = typeflag;
    h
}

#[test]
fn variant_detect_matrix() {
    // GNU：magic "ustar " + version " \0"（T22 §7-2 陷阱：空格+空字符，非 "00"）
    assert_eq!(
        crate::variant::detect(&head_at(Some(b"ustar  \x00"), b'0')),
        Some(crate::variant::TarVariant::Gnu)
    );
    // POSIX ustar：magic "ustar\0" + version "00"
    assert_eq!(
        crate::variant::detect(&head_at(Some(b"ustar\x0000"), b'0')),
        Some(crate::variant::TarVariant::Ustar)
    );
    // 禁止 starts_with("ustar") 混判：GNU 头应判 Gnu 而非 Ustar——上面的第一断言已保证
    // v7：无魔数
    assert_eq!(
        crate::variant::detect(&head_at(None, b'0')),
        Some(crate::variant::TarVariant::V7)
    );
    // 坏魔数（GNU version 错位等）→ None
    assert_eq!(
        crate::variant::detect(&head_at(Some(b"ustar  xx"), b'0')),
        None
    );
    assert_eq!(
        crate::variant::detect(&head_at(Some(b"tarustar"), b'0')),
        None
    );
}

#[test]
fn caps_decl() {
    let c = crate::TarHandler::new().caps();
    let want = FormatCaps {
        read: true,
        write: true,
        append: true,
        edit: false,
        ..Default::default()
    };
    assert_eq!(c, want);
}

#[test]
fn signature_offset_257() {
    assert_eq!(
        crate::TarHandler::new().signature().offset,
        crate::consts::TAR_MAGIC_OFFSET
    );
}
