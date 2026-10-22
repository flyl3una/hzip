//! 卷层文案表完整性（T-IMG）：**每个 `vol.*` 键在两种语言里都必须有非空文案**。
//!
//! # 为什么要有这条测试
//!
//! 卷层的「不能改」理由（只读原因/不支持/警告/错误）是**用户最常看到的一句话**
//! （脏卷、加密、布局不支持、空间不足…）。键散落在 `hzip-vol` 与四个驱动 crate 里，
//! 少登记一条的后果不是编译错误，而是界面上直接显示出 `vol.unsupported.ext-l2`
//! 这种内部标识——静默降级成「说不出理由」，正是本仓库反复强调要禁止的形态。
//!
//! # 键清单怎么维护
//!
//! 下面 `VOL_KEYS` 是从代码里**枚举出来的**（不是手抄）：
//!
//! ```sh
//! grep -rhoE '"vol\.[a-z0-9.-]+"' crates | sort -u
//! ```
//!
//! 新增键时重跑这条、补进本清单、再往 `locales/vol.toml` 补文案——测试会立刻告诉你
//! 漏了哪一条（而不是等用户看到生键名）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

/// 卷层全部稳定键（与 `locales/vol.toml` 一一对应）。
const VOL_KEYS: &[&str] = &[
    // —— 只读理由（`ReadOnlyReason::key()`；变体加一个这里要加一个）——
    "vol.readonly.by-setting",
    "vol.readonly.device",
    "vol.readonly.dirty",
    "vol.readonly.encrypted",
    "vol.readonly.fs-unknown",
    "vol.readonly.hibernated",
    "vol.readonly.journal",
    "vol.readonly.layout",
    "vol.readonly.outer-not-editable",
    // —— 拒绝（来源/形态层面）——
    "vol.reject.commit-spooled",
    "vol.reject.nested-depth",
    "vol.reject.multi-volume",
    "vol.reject.stdin-source",
    // —— 不支持 ——
    "vol.unsupported.backing-file",
    "vol.unsupported.change",
    "vol.unsupported.change-route",
    "vol.unsupported.compressed",
    "vol.unsupported.create-image",
    "vol.unsupported.descriptor",
    "vol.unsupported.dir-too-large",
    "vol.unsupported.ext-l2",
    "vol.unsupported.external-data",
    "vol.unsupported.layout-flat",
    "vol.unsupported.layout-unknown",
    "vol.unsupported.layout-vmfs",
    "vol.unsupported.multi-extent",
    "vol.unsupported.open",
    "vol.unsupported.qcow2-feature",
    "vol.unsupported.snapshot-delta",
    "vol.unsupported.split-image",
    // —— 错误（文件系统层）——
    "vol.err.audit",
    "vol.err.audit-skipped",
    "vol.err.bad-type",
    "vol.err.dir-move",
    "vol.err.dir-not-empty",
    "vol.err.entry-gone",
    "vol.err.exists",
    "vol.err.fs-feature",
    "vol.err.invalid-name",
    "vol.err.name-too-long",
    "vol.err.no-space",
    "vol.err.partition-not-mounted",
    "vol.err.read-entry",
    "vol.err.refused",
    "vol.err.source-not-file",
    "vol.err.spool-limit",
    "vol.err.too-large",
    "vol.err.unknown-change",
    "vol.err.unknown-entry",
    "vol.err.unknown-selection",
    "vol.err.unsupported-change",
    // —— 警告（提示代价，不阻止操作）——
    "vol.warn.backing-copyup",
    "vol.warn.inplace-fallback",
    "vol.warn.no-journal",
    "vol.warn.table-corrupt",
];

/// 文案表（编译期嵌入：漏改文件不会让测试悄悄跳过）。
const VOL_TOML_ZH: &str = include_str!("../../locales/vol.toml");

fn lookup(table: &toml::Value, key: &str, lang: &str) -> Option<String> {
    let mut cur = table;
    for seg in key.split('.') {
        cur = cur.get(seg)?;
    }
    cur.get(lang)?.as_str().map(str::to_string)
}

#[test]
fn every_volume_key_has_both_locales() {
    let table: toml::Value = toml::from_str(VOL_TOML_ZH).expect("vol.toml 应可解析");
    let mut missing = Vec::new();
    for key in VOL_KEYS {
        for lang in ["en", "zh-CN"] {
            match lookup(&table, key, lang) {
                Some(text) if !text.trim().is_empty() => {}
                _ => missing.push(format!("{key} [{lang}]")),
            }
        }
    }
    assert!(
        missing.is_empty(),
        "卷层文案缺失（界面上会直接显示内部键名）：{missing:#?}"
    );
}

#[test]
fn single_locale_file_covers_every_key_exactly_once() {
    // 反向校验：`vol.toml` 里不该有清单外的键（改名/删键后忘删文案）
    let table: toml::Value = toml::from_str(VOL_TOML_ZH).expect("vol.toml 应可解析");
    let mut extra = Vec::new();
    collect(&table, String::new(), &mut extra);
    let known: std::collections::HashSet<&&str> = VOL_KEYS.iter().collect();
    let unknown: Vec<String> = extra
        .into_iter()
        .filter(|k| !known.contains(&k.as_str()))
        .collect();
    assert!(
        unknown.is_empty(),
        "文案表里有清单外的键（清单没跟上或键已改名）：{unknown:#?}"
    );
}

/// 递归收集 `[a.b.c]` 形态的表键（叶 = 带 `en`/`zh-CN` 的表）。
fn collect(value: &toml::Value, prefix: String, out: &mut Vec<String>) {
    let Some(map) = value.as_table() else {
        return;
    };
    if map.contains_key("en") || map.contains_key("zh-CN") {
        if !prefix.is_empty() {
            out.push(prefix);
        }
        return;
    }
    for (k, v) in map {
        if k.starts_with('_') {
            continue; // `_meta`/`_version` 是 rust-i18n 的元数据
        }
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        collect(v, path, out);
    }
}
