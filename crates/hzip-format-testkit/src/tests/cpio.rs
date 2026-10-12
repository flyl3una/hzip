use super::*;

#[test]
fn newc_record_is_aligned_and_self_describing() {
    let v = build(CpioVariant::Newc, &[EntrySpec::file("a.txt", b"hello")]);
    // 头 110 + 名 6（"a.txt"+NUL）→ 补 0 到 4 的倍数：110+6=116 ✅ 无填充
    assert_eq!(&v[..6], b"070701");
    assert_eq!(&v[110..116], b"a.txt\0");
    assert_eq!(&v[116..121], b"hello");
    // 尾条目：名 11+1 = 12 → 110+12 = 122 → 补 2（到 124）
    let tail_off = 124;
    assert_eq!(
        &v[tail_off..tail_off + 6],
        b"070701",
        "第二条记录（尾条目）"
    );
    // 名区 = "TRAILER!!!"（10）+ 尾 NUL + 填充 3（110+11=121 → 124）
    assert_eq!(&v[tail_off + 110..tail_off + 124], b"TRAILER!!!\0\0\0\0");
    assert_eq!(v.len(), tail_off + 124, "尾条目自身也按 4 对齐收尾");
}

#[test]
fn crc_check_is_byte_sum() {
    let v = build(CpioVariant::Crc, &[EntrySpec::file("a", b"\x01\x02\x03")]);
    assert_eq!(&v[..6], b"070702");
    // check 域 = 最后一个 8 位十六进制域（偏移 6 + 12*8）
    let check = std::str::from_utf8(&v[6 + 12 * 8..6 + 13 * 8]).expect("hex");
    assert_eq!(u32::from_str_radix(check, 16), Ok(6), "1+2+3 = 0x6");
}

/// 对齐口径**取自 GNU cpio 2.14 实测**（`/tmp/cpioprobe/newc.cpio`）：
/// 名区 `5` → 补 1（`110+5=115` → `116`），数据区 `5` → 补 3（`116+5=121`
/// → `124`），故下一条头落在**记录起点 + 124**。名字区与数据区**各按自己
/// 的起点对齐**——按「统一自头起点对齐」会得到 121+1=122 的错位布局。
#[test]
fn alignment_matches_gnu_cpio_probe() {
    let gnu = build_without_trailer(
        CpioVariant::Newc,
        &[EntrySpec::symlink("link", b"a.txt")], // namesize = 5, filesize = 5
    );
    assert_eq!(gnu.len(), 124, "单条 link 记录长度 = 110+5+1 +5+3");
    assert_eq!(&gnu[110..116], b"link\0\0", "名区补 1");
    assert_eq!(&gnu[116..121], b"a.txt", "数据区起点 = 116");
    assert_eq!(&gnu[121..124], &[0, 0, 0], "数据区补 3");
    // 尾条目接在其后（真实归档里 next header 就在 124）
    let with_tail = build(CpioVariant::Newc, &[EntrySpec::symlink("link", b"a.txt")]);
    assert_eq!(&with_tail[124..130], b"070701");
}

#[test]
fn odc_layout_is_76_bytes() {
    let v = build(CpioVariant::Odc, &[EntrySpec::file("a", b"xy")]);
    assert_eq!(&v[..6], b"070707");
    assert_eq!(v[6..12], *b"777777", "dev");
    assert_eq!(v[18..24], *b"100644", "mode（八进制）");
    // namesize(59..65) = 2（"a" + NUL）
    assert_eq!(v[59..65], *b"000002");
    // filesize(65..76) = 2
    assert_eq!(v[65..76], *b"00000000002");
    assert_eq!(&v[76..79], b"a\0x", "odc 无对齐：数据紧接名字（不含填充）");
    // odc 无任何填充：第二条记录（尾条目）的头紧接在 80
    assert_eq!(&v[80..86], b"070707", "第二条记录紧接（无块填充）");
    // 尾条目名偏移 = 80 + 6(magic) + 42(7×6) + 11(mtime) + 6(namesize) + 11(filesize)
    assert_eq!(&v[156..167], b"TRAILER!!!\0");
}

#[test]
fn bin_words_match_endianness() {
    for variant in [CpioVariant::BinLe, CpioVariant::BinBe] {
        let v = build(variant, &[EntrySpec::file("a", b"xy")]);
        assert_eq!(v.len() % 2, 0, "bin 归档按 16 位字对齐");
        let le = variant.little();
        let magic = if le {
            u16::from_le_bytes([v[0], v[1]])
        } else {
            u16::from_be_bytes([v[0], v[1]])
        };
        assert_eq!(magic, 0o070707);
        // mode 字（第 4 个 16 位字）
        let mode = if le {
            u16::from_le_bytes([v[6], v[7]])
        } else {
            u16::from_be_bytes([v[6], v[7]])
        };
        assert_eq!(mode, 0o100644);
    }
}

#[test]
fn blocked_archive_pads_to_512() {
    let v = build_blocked(CpioVariant::Newc, &[EntrySpec::file("a", b"x")]);
    assert_eq!(v.len() % 512, 0);
    assert!(v.ends_with(&[0, 0]), "块填充是零");
}

#[test]
fn truncated_helpers_are_shorter_than_full_record() {
    let full = build(CpioVariant::Newc, &[EntrySpec::file("abc", b"0123456789")]);
    assert_eq!(header_only(CpioVariant::Newc, "abc", 10).len(), 110);
    assert_eq!(
        header_and_name(CpioVariant::Newc, "abc", 10).len(),
        110 + 4 + 2,
        "头 + 名（含尾 NUL）+ 名区对齐填充（110+4=114 → 116）"
    );
    assert!(header_only(CpioVariant::Newc, "abc", 10).len() < full.len());
}
