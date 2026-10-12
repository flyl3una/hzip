use super::*;

/// locales 目录下全部 TOML（`include_str!` 无法 glob，运行时读盘）。
fn locale_files() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .expect("locales 目录存在")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(e.path()).expect("locale 文件可读");
            (name, text)
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "locales 目录不得为空");
    out
}

/// 深度优先收集 `[ns.key.zh-CN]` / `[ns.key.en]` → `(file, stem, zh, en)`。
fn collect(
    file: &str,
    prefix: &str,
    table: &toml::Table,
    out: &mut Vec<(String, String, String, String)>,
) {
    for (k, v) in table {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Table(inner) => collect(file, &key, inner, out),
            toml::Value::String(s) => {
                if key.starts_with('_') {
                    continue; // _meta/_version 元数据
                }
                let Some((stem, lang)) = key.rsplit_once('.') else {
                    continue;
                };
                if lang != "zh-CN" && lang != "en" {
                    continue;
                }
                match out.iter_mut().find(|(f, k2, _, _)| f == file && k2 == stem) {
                    Some(slot) => {
                        if lang == "zh-CN" {
                            slot.2 = s.clone();
                        } else {
                            slot.3 = s.clone();
                        }
                    }
                    None => {
                        let entry = if lang == "zh-CN" {
                            (file.to_string(), stem.to_string(), s.clone(), String::new())
                        } else {
                            (file.to_string(), stem.to_string(), String::new(), s.clone())
                        };
                        out.push(entry);
                    }
                }
            }
            _ => {}
        }
    }
}

/// 解析全部 locales 文件 → `(file, key, zh, en)`。
fn locale_entries() -> Vec<(String, String, String, String)> {
    let mut out = Vec::new();
    for (file, text) in locale_files() {
        let doc: toml::Table =
            toml::from_str(&text).unwrap_or_else(|e| panic!("{file} 不是合法 TOML: {e}"));
        collect(&file, "", &doc, &mut out);
    }
    out
}

// 每条形如 `[ns.key]` 的文案都必须同时有 zh-CN 与 en——
// 缺一边的后果是「切语言后那一条永远是另一种语言」（静默部分翻译）。
#[test]
fn every_declared_key_has_both_locales() {
    let entries = locale_entries();
    assert!(entries.len() > 50, "文案表规模异常：{} 条", entries.len());
    let mut bad = Vec::new();
    for (file, key, zh, en) in &entries {
        if zh.is_empty() {
            bad.push(format!("{file}: {key} 缺 zh-CN"));
        }
        if en.is_empty() {
            bad.push(format!("{file}: {key} 缺 en"));
        }
    }
    assert!(bad.is_empty(), "文案表缺项：\n{}", bad.join("\n"));
    // 两边 key 集合一致（上面的 per-key 检查已覆盖，这里再挡「只有单语言的表」）
    let keys: std::collections::HashSet<&str> =
        entries.iter().map(|(_, k, _, _)| k.as_str()).collect();
    assert_eq!(keys.len(), entries.len(), "存在重复 key（跨文件同名）");
}

// 声明的 key 必须真的能查到（rust-i18n 缺 key 时返回 key 本身 →
// 界面直接把 `menu.file` 画出来，且 zh/en 都不生效）。
#[test]
fn every_declared_key_resolves_in_both_locales() {
    let _guard = crate::viewmodel::testutil::locale_lock();
    let entries = locale_entries();
    let before = rust_i18n::locale().to_string();
    let mut bad = Vec::new();
    for loc in ["zh-CN", "en"] {
        crate::init_locale(loc);
        for (file, key, _, _) in &entries {
            let got = tr(key);
            if got == *key {
                bad.push(format!("{file}: {key} 在 {loc} 下未命中（返回 key 原文）"));
            }
        }
    }
    crate::init_locale(&before);
    assert!(bad.is_empty(), "文案查表失败：\n{}", bad.join("\n"));
}

// 中文值与英文值必须真的不同（防「双语表贴了同一份中文」的作品式翻译）。
// 允许少数专有名词同形（zip/GBK/English 等），故只统计占比。
#[test]
fn translations_are_actually_translated() {
    let entries = locale_entries();
    let same = entries.iter().filter(|(_, _, zh, en)| zh == en).count();
    let total = entries.len();
    assert!(
        same * 4 < total,
        "疑似未翻译（zh == en）占比过高：{same}/{total}"
    );
}

// 占位符替换（含「未传参保留原样」的守界行为）。
#[test]
fn substitute_replaces_known_and_keeps_unknown_placeholders() {
    assert_eq!(
        substitute(
            "提交 {kind} 任务 {id}: {src}",
            &[("kind", "解压"), ("id", "7"), ("src", "a.zip")]
        ),
        "提交 解压 任务 7: a.zip"
    );
    assert_eq!(
        substitute("甲 与 {other}", &[("name", "乙")]),
        "甲 与 {other}",
        "未传参的占位符原样保留（暴露漏传）"
    );
    // 同名占位符出现多次 → 全部替换
    assert_eq!(substitute("{x}-{x}", &[("x", "1")]), "1-1");
}
