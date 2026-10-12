use super::*;

// 双向映射 roundtrip：每个下标 label/value 往返一致。
#[test]
fn label_value_roundtrip_all_tables() {
    let tables = [
        (&FORMAT[..], "zip"),
        (&LEVEL[..], "store"),
        (&CONFLICT[..], "ask"),
        (&CODEPAGE[..], "auto"),
        (&LANG[..], "system"),
        (&THEME[..], "system"),
        (&METHOD[..], "deflate"),
        (&SPLIT[..], "none"),
        (&THREADS[..], "0"),
        (&ENC_METHOD[..], "none"),
        (&DICT[..], "auto"),
        (&UPDATE_MODE[..], "new"),
    ];
    for (table, first_value) in tables {
        for ix in 0..table.len() {
            let v = value_of(table, ix);
            assert_eq!(index_of_value(table, v), ix, "{v} 下标往返");
            assert_ne!(table[ix].0, "", "label key 非空");
        }
        assert_eq!(value_of(table, 0), first_value);
    }
}

// 未知值回落 0（向后兼容旧配置语义）。
#[test]
fn unknown_value_falls_back_to_zero() {
    assert_eq!(index_of_value(&THEME, "sepia"), 0);
    assert_eq!(index_of_value(&LANG, "fr"), 0);
}

// 越界下标回落 0 项（防守白名单漂移）。
#[test]
fn out_of_range_index_falls_back() {
    assert_eq!(value_of(&THEME, 9), "system");
    assert_eq!(value_of(&LEVEL, 99), "store");
}

// label key 顺序 = table 顺序；本地化后下标语义不变（下标 = value 下标）。
#[test]
fn label_keys_preserve_order() {
    let _g = crate::viewmodel::testutil::locale_lock();
    assert_eq!(rust_i18n::locale().to_string(), "zh-CN");
    assert_eq!(
        label_keys(THEME),
        ["opt.theme.system", "opt.theme.light", "opt.theme.dark"]
    );
    // 7 单格式 + 4 组合格式（tar.gz / tar.bz2 / tar.xz / tar.zst）
    assert_eq!(label_keys(FORMAT).len(), 11);
    assert!(label_keys(FORMAT).contains(&"opt.format.tar.gz"));
    // 界面子表：单格式 7 项，无组合格式。
    assert_eq!(label_keys(UI_FORMAT).len(), 7);
    assert!(label_keys(UI_FORMAT).iter().all(|k| !k.contains("tar.")));
    // UI 子表是 FORMAT 的**单格式子集**：同 value 的 key 两表一致
    // （防拼写漂移；下标域则**独立**——UI 表自身重排无需迁移 FORMAT）。
    for (k, v) in FORMAT.iter() {
        if let Some(u) = UI_FORMAT.iter().position(|(_, uv)| uv == v) {
            assert_eq!(UI_FORMAT[u].0, *k, "{k} 两表 key 必须一致");
        }
    }
    assert_eq!(
        UI_FORMAT.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        FORMAT
            .iter()
            .filter(|(_, v)| !v.contains('.'))
            .map(|(_, v)| *v)
            .collect::<Vec<_>>(),
        "UI_FORMAT = FORMAT 的单格式（无点）子集"
    );
}

// 本地化展示文案：切语言后同一 key 给出不同文案（下标→value 映射不受影响）。
#[test]
fn label_of_follows_locale_but_value_stays() {
    let _g = crate::viewmodel::testutil::locale_lock();
    let before = rust_i18n::locale().to_string();
    crate::init_locale("zh-CN");
    assert_eq!(label_of(&LEVEL, 0), "存储");
    assert_eq!(label_of(&THEME, 2), "深色");
    crate::init_locale("en");
    assert_eq!(label_of(&LEVEL, 0), "Store");
    assert_eq!(label_of(&THEME, 2), "Dark");
    // 落盘值不随语言变化（config 契约）
    assert_eq!(value_of(&LEVEL, 0), "store");
    assert_eq!(value_of(&THEME, 2), "dark");
    // 越界仍回落 0 项文案
    assert_eq!(label_of(&LEVEL, 99), "Store");
    crate::init_locale(&before);
}

/// Select 回查下标靠**文案**（`select_string_state`：Confirm 载荷 = 显示文案，
/// 用构建期同一份 label 序列反查）。前提：同一张表内本地化文案互不相同——
/// 重复文案会让「选了 B 落到 A」静默发生，故在此把前提钉住。
#[test]
fn select_table_labels_are_unique() {
    let _g = crate::viewmodel::testutil::locale_lock();
    for (name, table) in [
        ("FORMAT", &FORMAT[..]),
        ("METHOD", &METHOD[..]),
        ("SPLIT", &SPLIT[..]),
        ("THREADS", &THREADS[..]),
        ("DICT", &DICT[..]),
        ("UPDATE_MODE", &UPDATE_MODE[..]),
        ("ENC_METHOD", &ENC_METHOD[..]),
        ("CONFLICT", &CONFLICT[..]),
        ("CODEPAGE", &CODEPAGE[..]),
        ("LEVEL", &LEVEL[..]),
    ] {
        let labels = labels_of(table);
        let mut sorted = labels.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            labels.len(),
            "{name} 的候选文案必须互不相同（否则 Select 反查下标会错位）：{labels:?}"
        );
    }
}

/// Select 候选项**必须已本地化**：rust-i18n 缺译时 `tr(key)` 原样返回 key，
/// 于是下拉里就出现 `opt.format.zip` 这种内部键（2026-09 用户报障）。
/// 本测试把「每个候选表都有译文」钉住（表的增项忘了写 locale 会立刻红）。
#[test]
fn select_table_labels_are_translated() {
    let _g = crate::viewmodel::testutil::locale_lock();
    for (name, table) in [
        ("FORMAT", &FORMAT[..]),
        ("METHOD", &METHOD[..]),
        ("SPLIT", &SPLIT[..]),
        ("THREADS", &THREADS[..]),
        ("DICT", &DICT[..]),
        ("UPDATE_MODE", &UPDATE_MODE[..]),
        ("ENC_METHOD", &ENC_METHOD[..]),
        ("CONFLICT", &CONFLICT[..]),
        ("CODEPAGE", &CODEPAGE[..]),
        ("LEVEL", &LEVEL[..]),
    ] {
        for (key, _) in table {
            assert_ne!(
                crate::tr(key),
                *key,
                "{name} 的候选 `{key}` 缺译文（下拉会显示 i18n 内部键）"
            );
        }
    }
    // 格式候选项是 glob 形态：用户看的是「能压成什么后缀」
    assert_eq!(crate::tr("opt.format.zip"), "*.zip");
    assert_eq!(crate::tr("opt.format.7z"), "*.7z");
}

// UI 侧互锁锚点在 ui/theme.rs（theme_mode_choice_labels_match）——
// 本模块保持零 ui/ 依赖（含 cfg(test)）。
#[test]
fn interlocks_with_existing_semantics() {
    let _g = crate::viewmodel::testutil::locale_lock();
    assert_eq!(rust_i18n::locale().to_string(), "zh-CN");
    assert_eq!(
        labels_of(&FORMAT),
        [
            "*.zip",
            "*.7z",
            "*.tar",
            "*.tar.gz",
            "*.tar.bz2",
            "*.tar.xz",
            "*.tar.zst",
            "*.gz",
            "*.bz2",
            "*.xz",
            "*.zst",
        ],
        "格式候选显示为 glob 模式（用户看的是「能压成什么后缀」；落盘值仍是不带点的 value）"
    );
    assert_eq!(label_of(&LEVEL, 0), "存储");
    assert_eq!(label_of(&LEVEL, 5), "极限");
    assert_eq!(label_of(&CONFLICT, 0), "询问");
    assert_eq!(label_of(&CODEPAGE, 0), "自动");
    assert_eq!(label_of(&LANG, 1), "简体中文");
}
