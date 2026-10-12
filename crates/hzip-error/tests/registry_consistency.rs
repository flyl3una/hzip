//! TC-CORE-021 错误码「三处一致性」前两处锁定：
//! `code.rs REGISTRY` ↔ 02 附录 A.1 ↔ `i18n-keys.md`（任务卡 §6 用例 12/13）。
//! 第三处（locales 文案）由后续 CLI/GUI 任务补齐后全锁。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use hzip_error::code;

/// 从 02 附录 A.1 表格行提取 `(错误码, i18n key, 退出码单元格)`。
fn parse_appendix_a(md: &str) -> Vec<(u32, String, String)> {
    let mut rows = Vec::new();
    for line in md.lines() {
        let t = line.trim();
        if !t.starts_with("| HZ-") {
            continue;
        }
        let cells: Vec<&str> = t.split('|').map(str::trim).collect();
        // "| HZ-1001 | 错误 | 变体 | key | 动作 | 退出码 |" → cells[1]..cells[6]
        if cells.len() < 8 {
            continue;
        }
        // cells[1] == "HZ-1001"
        let code_str = cells[1].strip_prefix("HZ-").unwrap_or_default();
        if code_str.len() == 4 && code_str.bytes().all(|b| b.is_ascii_digit()) {
            let num: u32 = code_str.parse().unwrap_or(0);
            let key = cells[4].trim_matches('`').to_string();
            let exit_cell = cells[cells.len() - 2].to_string();
            rows.push((num, key, exit_cell));
        }
    }
    rows
}

/// 提取字符串中出现的全部 `err.hz_XXXX` key（数字段为 4 位）。
fn extract_i18n_keys(text: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let mut from = 0usize;
    while let Some(pos) = text.get(from..).and_then(|s| s.find("err.hz_")) {
        let start = from + pos + "err.hz_".len();
        let digits: String = text
            .get(start..)
            .unwrap_or_default()
            .bytes()
            .take_while(|b| b.is_ascii_digit())
            .map(|b| b as char)
            .collect();
        if digits.len() == 4 {
            keys.insert(format!("err.hz_{digits}"));
        }
        from = start;
    }
    keys
}

/// 用例 12（TC-CORE-021a）：REGISTRY ↔ 02 附录 A.1 双向相等 + 退出码一致。
#[test]
fn registry_matches_appendix_a() {
    let doc_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/design/02-程序详细设计.md");
    let md = std::fs::read_to_string(&doc_path)
        .unwrap_or_else(|e| panic!("无法读取设计文档 {}: {e}", doc_path.display()));
    let rows = parse_appendix_a(&md);
    assert_eq!(rows.len(), 30, "附录 A.1 应为 30 行（29 登记码 + HZ-0000）");

    let doc_codes: BTreeSet<u32> = rows.iter().map(|(c, _, _)| *c).collect();
    let reg_codes: BTreeSet<u32> = code::REGISTRY.iter().map(|(c, ..)| *c).collect();
    let missing: Vec<&u32> = doc_codes.difference(&reg_codes).collect();
    let extra: Vec<&u32> = reg_codes.difference(&doc_codes).collect();
    assert!(missing.is_empty(), "REGISTRY 缺失附录 A 码: {missing:?}");
    assert!(extra.is_empty(), "REGISTRY 存在附录 A 之外的码: {extra:?}");

    // 逐码退出码一致（附录 A 允许「1/2」类归并写法：REGISTRY 值须属于该单元格数字集）
    for (num, doc_key, exit_cell) in &rows {
        let entry = code::REGISTRY
            .iter()
            .find(|(c, ..)| c == num)
            .unwrap_or_else(|| panic!("REGISTRY 缺少码 {num}"));
        let allowed: Vec<u8> = exit_cell
            .split(|ch: char| !ch.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<u8>().unwrap_or(255))
            .collect();
        assert!(
            allowed.contains(&entry.3),
            "码 {num} 退出码不一致: 附录 A 为 {exit_cell:?}, REGISTRY 为 {}",
            entry.3
        );
        assert_eq!(&entry.1, doc_key, "码 {num} 的 i18n key 与附录 A 不一致");
    }
    // 关键锚点（任务卡 §6 用例 12）
    let exit_of = |n: u32| {
        code::REGISTRY
            .iter()
            .find(|(c, ..)| *c == n)
            .map(|(_, _, _, e, _)| *e)
    };
    assert_eq!(exit_of(6001), Some(130));
    assert_eq!(exit_of(6002), Some(1));
    assert_eq!(exit_of(3001), Some(2));
}

/// 用例 13（TC-CORE-021b）：i18n-keys.md ↔ REGISTRY key 集合完全一致（含 err.hz_0000）。
#[test]
fn i18n_keys_cover_registry() {
    const KEYS_MD: &str = include_str!("../i18n-keys.md");
    let file_keys = extract_i18n_keys(KEYS_MD);
    let reg_keys: BTreeSet<String> = code::REGISTRY
        .iter()
        .map(|(_, k, ..)| (*k).to_string())
        .collect();
    assert_eq!(
        file_keys, reg_keys,
        "i18n-keys.md 与 REGISTRY key 集合不一致（缺失或多余）"
    );
    assert!(file_keys.contains("err.hz_0000"));
    assert_eq!(file_keys.len(), 30);
}
