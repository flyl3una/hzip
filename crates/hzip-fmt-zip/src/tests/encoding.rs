use super::*;

fn gbk(s: &str) -> Vec<u8> {
    let (bytes, _, _) = encoding_rs::GBK.encode(s);
    bytes.into_owned()
}

fn big5(s: &str) -> Vec<u8> {
    let (bytes, _, _) = encoding_rs::BIG5.encode(s);
    bytes.into_owned()
}

#[test]
fn encoding_gbk_repair() {
    // 高置信中文长名样本：检测 → GBK → 转码正确 UTF-8
    let name = "这是一个很长的中文文件名用于测试检测置信度.txt";
    let raw = gbk(name);
    assert!(std::str::from_utf8(&raw).is_err(), "GBK 字节非合法 UTF-8");
    let g = detect_batch(&[raw.as_slice()]).expect("长 GBK 名应高置信");
    assert_eq!(g.enc, Encoding::Gbk);
    assert!(g.confident);
    let (decoded, ne) = classify(&raw, None, Some(g));
    assert_eq!(decoded, name);
    assert_eq!(ne, NameEncoding::Suspect(Encoding::Gbk));

    // 强制链 --codepage gbk（跳过置信度直接转码）
    let (dec2, ne2) = classify(&raw, Some(Encoding::Gbk), None);
    assert_eq!(dec2, name);
    assert_eq!(ne2, NameEncoding::Suspect(Encoding::Gbk));
}

#[test]
fn encoding_big5_detect() {
    let name = "這是繁體中文檔案名稱測試";
    let raw = big5(name);
    let g = detect_batch(&[raw.as_slice()]).expect("长 Big5 名应高置信");
    assert_eq!(g.enc, Encoding::Big5);
    let (decoded, _) = classify(&raw, None, Some(g));
    assert_eq!(decoded, name);
}

#[test]
fn encoding_low_confidence() {
    // 2 字节短名歧义样本：保留原始 + Suspect，不崩溃不乱猜（TC-CORE-016）
    // GBK「文件」两字 4 字节；真正歧义的是 2 字节（单字）样本
    let short_raw = gbk("文"); // 2 字节：CEC4
    assert_eq!(short_raw.len(), 2);
    let g = detect_batch(&[short_raw.as_slice()]);
    // 低置信 → 无高置信批量结果（None 或 confident=false）
    let (decoded, ne) = classify(&short_raw, None, g);
    // 保留原始（逐字节 cp437 直映）而非乱猜成其它语言
    assert_eq!(
        decoded,
        short_raw.iter().map(|&b| b as char).collect::<String>()
    );
    match ne {
        NameEncoding::Suspect(_) | NameEncoding::Unknown => {}
        NameEncoding::Utf8 => panic!("2 字节非 UTF-8 原始不得标 Utf8"),
        _ => {}
    }

    // 纯 ASCII 名：无检测样本 → Utf8（zip 写侧 flag 由 zip crate 处理）
    let ascii = b"plain.txt";
    let g2 = detect_batch(&[ascii.as_slice()]);
    assert!(g2.is_none(), "纯 ASCII 不触发编码修复");
    let (dec, ne2) = classify(ascii, None, None);
    assert_eq!(dec, "plain.txt");
    assert_eq!(ne2, NameEncoding::Utf8);
}

#[test]
fn encoding_invalid_forced_no_panic() {
    // 强制编码但字节非法 → 保留原始 + Suspect，不 panic
    let raw = b"\xff\xfe\x01bad";
    let (decoded, ne) = classify(raw, Some(Encoding::Gbk), None);
    assert_eq!(ne, NameEncoding::Suspect(Encoding::Gbk));
    assert_eq!(decoded.len(), raw.len(), "原始保留（逐字节直映）");
}
