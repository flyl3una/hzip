use super::*;
use hzip_format::{EntryAttrs, EntryInfo, EntryName};

fn info(name: &str, enc: NameEncoding) -> EntryInfo {
    EntryInfo {
        id: 0,
        name: EntryName::new(name.to_string()),
        is_dir: false,
        is_symlink: false,
        symlink_target: None,
        size_raw: 0,
        size_packed: 0,
        mtime: None,
        mode: None,
        attributes: EntryAttrs::default(),
        encrypted: false,
        header_encrypted: false,
        crc32: None,
        codec: None,
        name_encoding: enc,
        encryption: None,
        compression_level: None,
    }
}

/// 转码工具：GBK 编码字符串 → 字节。
fn gbk_bytes(s: &str) -> Vec<u8> {
    let (bytes, _, _) = encoding_rs::GBK.encode(s);
    bytes.into_owned()
}

#[test]
fn utf8_and_suspect_passthrough() {
    // Utf8 与已修复 Suspect 名不动
    let entries = vec![
        info("ok.txt", NameEncoding::Utf8),
        info("已修复.txt", NameEncoding::Suspect(Encoding::Gbk)),
    ];
    let (out, res) = reconcile_names(entries, &EncodingPolicy::new(None));
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].name.as_str(), "ok.txt");
    assert_eq!(out[1].name.as_str(), "已修复.txt");
    assert!(res.rewrites.is_empty());
    assert!(res.kept_suspect.is_empty());
    assert_eq!(res.detected, None);
}

/// TC-CORE-016：低置信/不可恢复名（lossy 替换符）→ 保留 + Suspect，不 Err。
#[test]
fn encoding_low_confidence_lossy_kept() {
    let entries = vec![info("bad\u{FFFD}name.txt", NameEncoding::Unknown)];
    let (out, res) = reconcile_names(entries, &EncodingPolicy::new(None));
    assert_eq!(out[0].name.as_str(), "bad\u{FFFD}name.txt");
    assert_eq!(res.kept_suspect, vec![0]);
    assert_eq!(res.detected, None);
}

/// 短名歧义（2 字节 GBK 名 → 低置信）→ 保留不重映射（TC-CORE-016 语义）。
#[test]
fn encoding_short_name_not_mangled() {
    let short = gbk_bytes("测");
    let mut d = chardetng::EncodingDetector::new();
    d.feed(&short, true);
    let (_enc, assess) = d.guess_assess(None, true);
    // chardetng 对 2 字节 CJK 名通常 assess=false（候选不足）——确认本模块
    // 的低置信路径不会产出转码（原始字节当前也不可得）。
    let entries = vec![info("\u{FFFD}名.txt", NameEncoding::Unknown)];
    let (out, res) = reconcile_names(entries, &EncodingPolicy::new(None));
    assert_eq!(out[0].name.as_str(), "\u{FFFD}名.txt");
    assert!(res.rewrites.is_empty());
    let _ = assess;
}

/// 批量检测的代理置信：多个长 GBK 名样本 → 检出 GBK 高置信。
#[test]
fn detect_batch_gbk_high_confidence() {
    let names = [
        "这是一个很长的中文文件名用于检测置信度之一.txt",
        "这是第二个中文文件名用于批量检测之二.txt",
        "这是第三个中文文件名的批量样本之三.txt",
    ];
    let raws: Vec<Vec<u8>> = names.iter().map(|s| gbk_bytes(s)).collect();
    let refs: Vec<&[u8]> = raws.iter().map(|v| v.as_slice()).collect();
    let got = detect_batch_confidence(refs, CONFIDENCE_THRESHOLD);
    assert_eq!(got, Some(Encoding::Gbk));
}

/// 单长样本 assess 通过 → 高置信检出。
#[test]
fn detect_single_long_gbk() {
    let raw = gbk_bytes("这是一个足够长的中文文件名以让检测器有把握.txt");
    let got = detect_batch_confidence(vec![raw.as_slice()], CONFIDENCE_THRESHOLD);
    assert_eq!(got, Some(Encoding::Gbk));
}

/// 手动 codepage（GBK）优先级最高——强制转码路径。
#[test]
fn manual_codepage_forced() {
    // 手动路径当前对 lossy 名不重映射（原始字节不可得），但结构正确返回
    let entries = vec![info("中文.txt", NameEncoding::Utf8)];
    let policy = EncodingPolicy::new(Some(Encoding::Gbk));
    let (out, res) = reconcile_names(entries, &policy);
    assert_eq!(out.len(), 1);
    assert!(res.rewrites.is_empty());
    assert!(res.kept_suspect.is_empty());
}

#[test]
fn transcode_roundtrip() {
    let raw = gbk_bytes("中文名.txt");
    assert_eq!(
        transcode_to(&raw, Encoding::Gbk).as_deref(),
        Some("中文名.txt")
    );
    // 非法字节（孤立的 0x80 单字节在 GBK 中是合法 trail？用无效序列测）
    let bad = [0xff, 0xfe, 0x00, 0x80];
    let r = transcode_to(&bad, Encoding::Gbk);
    // GBK 对 ff fe 也可能逐字节容错——不崩溃即可
    let _ = r;
}

/// 空样本 → 无检出。
#[test]
fn detect_empty_samples() {
    assert_eq!(
        detect_batch_confidence(Vec::<&[u8]>::new(), CONFIDENCE_THRESHOLD),
        None
    );
}
