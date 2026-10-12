use super::*;

/// newc 头样本：`magic + 13×8 位十六进制`。
fn newc(magic: &[u8; 6], fields: [u32; 13]) -> Vec<u8> {
    let mut v = magic.to_vec();
    for f in fields {
        v.extend_from_slice(format!("{f:08x}").as_bytes());
    }
    v
}

#[test]
fn detect_prefers_long_magic() {
    // odc 是 newc/crc 的前缀：必须靠逐字节对照区分，不能 starts_with("0707")
    assert_eq!(detect(b"070701...."), Some(CpioVariant::Newc));
    assert_eq!(detect(b"070702...."), Some(CpioVariant::Crc));
    assert_eq!(detect(b"070707777777"), Some(CpioVariant::Odc));
    assert_eq!(detect(&[0xC7, 0x71, 0, 0]), Some(CpioVariant::BinLe));
    assert_eq!(detect(&[0x71, 0xC7, 0, 0]), Some(CpioVariant::BinBe));
    assert_eq!(detect(b"070703...."), None);
    assert_eq!(detect(b"PK\x03\x04"), None);
    assert_eq!(detect(b"07"), None);
    assert_eq!(detect(b""), None);
}

#[test]
fn parse_newc_fields() {
    // ino=0x3c8eb, mode=0o40777, uid=0, gid=0, nlink=2, mtime=0x64c3b2a1,
    // filesize=5, dev 4 个 0, namesize=8, check=0
    let buf = newc(
        NEWC_MAGIC,
        [0x3c8eb, 0o40777, 0, 0, 2, 0x64c3b2a1, 5, 0, 0, 0, 0, 8, 0],
    );
    let h = parse_header(CpioVariant::Newc, &buf).expect("newc 头");
    assert_eq!(h.mode, 0o40777);
    assert_eq!(h.mtime, 0x64c3b2a1);
    assert_eq!(h.filesize, 5);
    assert_eq!(h.namesize, 8);
    assert_eq!(h.check, 0, "newc 不读 check 域");
}

#[test]
fn parse_crc_check_field() {
    let buf = newc(
        CRC_MAGIC,
        [1, 0o100644, 0, 0, 1, 0, 11, 0, 0, 0, 0, 5, 0x2a],
    );
    let h = parse_header(CpioVariant::Crc, &buf).expect("crc 头");
    assert_eq!(h.check, 0x2a, "crc 变体读 check 域");
}

#[test]
fn parse_odc_fields() {
    // 76B：magic(6) dev(6) ino(6) mode(6) uid(6) gid(6) nlink(6) rdev(6)
    // mtime(11) namesize(6) filesize(11)，全八进制定长
    let mut buf = ODC_MAGIC.to_vec();
    buf.extend_from_slice(b"777777"); // dev
    buf.extend_from_slice(b"000001"); // ino
    buf.extend_from_slice(b"100644"); // mode
    buf.extend_from_slice(b"000000"); // uid
    buf.extend_from_slice(b"000000"); // gid
    buf.extend_from_slice(b"000001"); // nlink
    buf.extend_from_slice(b"000000"); // rdev
    buf.extend_from_slice(b"12345670123"); // mtime（11 位八进制）
    buf.extend_from_slice(b"000010"); // namesize = 8
    buf.extend_from_slice(b"00000000005"); // filesize = 5
    assert_eq!(buf.len(), ODC_HEADER_BYTES);
    let h = parse_header(CpioVariant::Odc, &buf).expect("odc 头");
    assert_eq!(h.mode, 0o100644);
    assert_eq!(h.namesize, 8);
    assert_eq!(h.filesize, 5);
    assert_eq!(h.mtime, u64::from_str_radix("12345670123", 8).unwrap());
}

#[test]
fn parse_bin_word_order() {
    // LE：magic c7 71，mode=0o40777(0x41ff)→ ff 41，filesize=0x00010005
    let mut le = Vec::new();
    for w in [0o70707u16, 0xa2d2, 1, 0o40777, 0, 0, 2, 0] {
        le.extend_from_slice(&w.to_le_bytes());
    }
    le.extend_from_slice(&0x64c3u16.to_le_bytes()); // mtime hi
    le.extend_from_slice(&0xb2a1u16.to_le_bytes()); // mtime lo
    le.extend_from_slice(&8u16.to_le_bytes()); // namesize
    le.extend_from_slice(&0x0001u16.to_le_bytes()); // filesize hi
    le.extend_from_slice(&0x0005u16.to_le_bytes()); // filesize lo
    assert_eq!(le.len(), BIN_HEADER_BYTES);
    let h = parse_header(CpioVariant::BinLe, &le).expect("bin-le 头");
    assert_eq!(h.mode, 0o40777);
    assert_eq!(h.mtime, 0x64c3_b2a1);
    assert_eq!(h.namesize, 8);
    assert_eq!(h.filesize, 0x0001_0005);

    // BE：同字段按大端写出，读法随魔数切换
    let mut be = Vec::new();
    for w in [0o70707u16, 0xa2d2, 1, 0o40777, 0, 0, 2, 0] {
        be.extend_from_slice(&w.to_be_bytes());
    }
    be.extend_from_slice(&0x64c3u16.to_be_bytes());
    be.extend_from_slice(&0xb2a1u16.to_be_bytes());
    be.extend_from_slice(&8u16.to_be_bytes());
    be.extend_from_slice(&0x0001u16.to_be_bytes());
    be.extend_from_slice(&0x0005u16.to_be_bytes());
    assert_eq!(detect(&be), Some(CpioVariant::BinBe));
    let h2 = parse_header(CpioVariant::BinBe, &be).expect("bin-be 头");
    assert_eq!(h2, h, "同字段两种字序解析结果一致");
}

#[test]
fn parse_header_rejects_short_or_bad() {
    assert!(parse_header(CpioVariant::Newc, b"07070100").is_none());
    assert!(parse_header(CpioVariant::Odc, b"070707").is_none());
    assert!(parse_header(CpioVariant::BinLe, &[0xC7, 0x71]).is_none());
    // **被消费的**域含非十六进制字符 → None（不 panic、不静默当 0）。
    // 域序：ino(0) mode(1) uid(2) gid(3) nlink(4) mtime(5) filesize(6)
    // devmajor(7) devminor(8) rdevmajor(9) rdevminor(10) namesize(11) check(12)
    // 故把坏域放在 mode(1)：ino/uid/gid/dev/rdev 本层不消费、不参与校验
    // （不消费的域解析失败也无从影响条目语义，见模块头「只保留条目语义需要的域」）。
    let mut bad = NEWC_MAGIC.to_vec();
    bad.extend_from_slice(b"00000000"); // ino：坏也不影响（不消费）
    bad.extend_from_slice(b"zzzzzzzz"); // mode：消费域 → 必须拒绝
    bad.extend_from_slice(&[b'0'; 88]);
    assert!(parse_header(CpioVariant::Newc, &bad).is_none());
}

#[test]
fn align_and_variant_metrics() {
    assert_eq!(align_up(0, 4), Some(0));
    assert_eq!(align_up(13, 4), Some(16));
    assert_eq!(align_up(13, 1), Some(13));
    assert_eq!(align_up(u64::MAX, 4), None);
    assert_eq!(CpioVariant::Newc.header_bytes(), 110);
    assert_eq!(CpioVariant::Odc.header_bytes(), 76);
    assert_eq!(CpioVariant::BinBe.align(), 2);
    assert_eq!(CpioVariant::BinLe.as_str(), "bin-le");
}

#[test]
fn map_err_matrix() {
    let fmt = FormatId::from("cpio");
    let eof = io::Error::new(io::ErrorKind::UnexpectedEof, "boom");
    assert!(matches!(
        map_err(eof, fmt.clone()),
        HzipError::Format {
            kind: FormatErrorKind::Truncated,
            ..
        }
    ));
    let hdr = io::Error::other("bad header");
    assert!(matches!(
        map_err(hdr, fmt.clone()),
        HzipError::Format {
            kind: FormatErrorKind::BadHeader,
            ..
        }
    ));
    let sig = io::Error::new(io::ErrorKind::InvalidInput, "bad magic");
    assert!(matches!(
        map_err(sig, fmt),
        HzipError::Format {
            kind: FormatErrorKind::BadSignature,
            ..
        }
    ));
}
