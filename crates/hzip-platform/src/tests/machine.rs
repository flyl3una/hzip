use super::*;

// 机型判定是**纯函数**（无平台调用）——这些用例在三平台都跑，Windows 上同样有效。

/// 正例：触控栏世代（2018–2019 的 15,x、2019–2020 的 16,x）。
#[test]
fn touch_bar_generations_are_recognized() {
    for model in [
        "MacBookPro15,1",
        "MacBookPro15,2",
        "MacBookPro15,3",
        "MacBookPro15,4",
        "MacBookPro16,1",
        "MacBookPro16,2",
        "MacBookPro16,3",
    ] {
        assert!(model_has_touch_bar(model), "{model} 应判为带触控栏");
    }
}

/// Apple Silicon 里仍带触控栏的两款——**必须**判真（漏判 = 逃生开关在真机静默失效）。
///
/// - `MacBookPro17,1`：13" MacBook Pro (M1, 2020)，Touch Bar 为标配。
/// - `Mac14,7`：13" MacBook Pro (M2, 2022)，标识**不在** `MacBookPro` 前缀族里
///   （Apple 支持文档 108052 明列 `Mac14,7`），是该族里唯一带触控栏的标识。
#[test]
fn apple_silicon_touch_bar_identifiers() {
    assert!(model_has_touch_bar("MacBookPro17,1"));
    assert!(model_has_touch_bar("Mac14,7"));
}

/// `Mac*` 标识族（`Mac14,x` / `Mac15,x`）：除 `Mac14,7` 外**不得**放宽（Air / 14"·16" Pro / mini /
/// Studio / M3 全系都在这一族，`Mac*` 前缀级判定会把它们全判真）。
#[test]
fn mac_identifier_family_is_not_blanket_true() {
    for model in [
        "Mac14,2",  // 13" MacBook Air (M2, 2022)
        "Mac14,3",  // Mac mini (M2, 2023)
        "Mac14,5",  // 14"/16" MacBook Pro (M2 Pro/Max, 2023)
        "Mac14,9",  // 同上（16"）
        "Mac14,13", // Mac Studio (2023)
        "Mac14,14", // Mac Studio (2023)
        "Mac15,3",  // M3 世代（14"/16" Pro 与 Air 均无 Touch Bar）
        "Mac15,12",
    ] {
        assert!(!model_has_touch_bar(model), "{model} 不应判为带触控栏");
    }
}

/// 13,x / 14,x：`,1` 是功能键机型（无触控栏），其余小号带触控栏。
///
/// `13,1`（Late 2016 13" 两口）与 `14,1`（Mid 2017 13" 两口）都没有 Touch Bar
/// ——这是两代里仅有的两款；`13,2`/`13,3`/`14,2`/`14,3` 均为四口 TB3 触控栏机型。
#[test]
fn early_generations_split_on_minor_one() {
    for (model, expected) in [
        ("MacBookPro13,1", false),
        ("MacBookPro13,2", true),
        ("MacBookPro13,3", true),
        ("MacBookPro14,1", false),
        ("MacBookPro14,2", true),
        ("MacBookPro14,3", true),
        // 未知小号按「带触控栏」处理（误报无害、漏报有害，见模块文档）。
        ("MacBookPro13,4", true),
    ] {
        assert_eq!(model_has_touch_bar(model), expected, "{model} 判定不符预期");
    }
}

/// 反例：非触控栏世代、非触控栏机型、空串、垃圾串一律否。
#[test]
fn non_touch_bar_models_are_rejected() {
    for model in [
        "MacBookPro18,1", // M1 Pro 14"（无 Touch Bar）
        "MacBookPro18,3", // M1 Pro/Max 14"/16"（无 Touch Bar）
        "MacBookPro18,4",
        "MacBookPro11,5", // 2015 世代（更早，从未有 Touch Bar）
        "MacBookAir10,1",
        "MacBookAir7,2",
        "iMac21,1",
        "Macmini9,1",
        "MacPro7,1",
        "",
        "   ",
        "垃圾串",
        "macbookpro15,1",   // 前缀大小写敏感：hw.model 只会给标准写法
        "MacBookPro",       // 缺小号
        "MacBookPro15",     // 缺逗号
        "MacBookPro15,",    // 小号为空
        "MacBookPro15,x",   // 小号非数字
        "MacBookPro15,1x",  // 小号带尾巴
        "MacBookPro 15,1",  // 世代带空格
        "MacBookPro0,1",    // 世代越界
        "MacBookPro200,1",  // 世代超出 u8
        "MacBook Pro 15,1", // 带空格的产品名
    ] {
        assert!(!model_has_touch_bar(model), "{model:?} 不应判为带触控栏");
    }
}

/// 串首尾空白忽略（`sysctl` 输出清洁，但调用方可能带进来）。
#[test]
fn surrounding_whitespace_is_ignored() {
    assert!(model_has_touch_bar(" MacBookPro15,1 "));
    assert!(model_has_touch_bar("MacBookPro16,2\n"));
    assert!(!model_has_touch_bar("\tMacBookPro18,3 "));
}

/// `is_touch_bar_machine`：`None`（非 macOS / 读不到）⇒ false（安全值）。
#[test]
fn missing_model_is_not_touch_bar() {
    assert!(!is_touch_bar_machine(None));
    assert!(!is_touch_bar_machine(Some("")));
    assert!(is_touch_bar_machine(Some("MacBookPro16,2")));
}

/// 本机（当前目标平台）的真值探测：非 macOS 必须恒 `None` / `false`。
#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_reports_no_model() {
    let model = machine_model();
    assert!(model.is_none(), "非 macOS 不应读到 hw.model：{model:?}");
    assert!(!is_touch_bar_machine(model.as_deref()));
}

/// macOS 上 `hw.model` 必须真的读到（`sysctlbyname` 两趟式取值的回归位）。
///
/// 只钉形态不钉具体机型（CI 可能是真机也可能是虚拟机，`hw.model` 在 VMware/
/// Parallels 上并非 `Mac*`）：非空、无空白、可打印 ASCII——这正是
/// [`model_has_touch_bar`] 的解析前提（不做任何平台调用，故这里也不 panic）。
#[cfg(target_os = "macos")]
#[test]
fn macos_reads_hw_model() {
    let model = machine_model().expect("macOS 应能读到 hw.model");
    assert!(!model.is_empty(), "hw.model 不应为空");
    assert!(
        model.is_ascii() && !model.contains(char::is_whitespace),
        "hw.model 形态异常：{model:?}"
    );
    // 拿到的机型串必须能被纯函数消费（不 panic）。
    let _ = is_touch_bar_machine(Some(&model));
}

/// 逃生开关：只认 `off`（大小写不敏感、两侧空白忽略），其余一律「不关辅助功能」。
#[test]
fn a11y_env_only_accepts_off() {
    for value in ["off", "OFF", "Off", "oFf", " off ", "\toff\n"] {
        assert!(
            a11y_fallback_env_value(Some(value)),
            "{value:?} 应要求关闭辅助功能"
        );
    }
    for value in ["", " ", "on", "ON", "0", "false", "no", "offf", "1"] {
        assert!(
            !a11y_fallback_env_value(Some(value)),
            "{value:?} 不应要求关闭辅助功能"
        );
    }
    assert!(!a11y_fallback_env_value(None), "未设置应走正常 a11y 路径");
}

/// 环境变量名是对外契约（文档与现场操作都按它来），改名即破坏逃生路径。
#[test]
fn a11y_env_name_is_frozen() {
    assert_eq!(A11Y_ENV, "HZIP_MACOS_A11Y");
}
