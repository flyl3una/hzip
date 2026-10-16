//! `ui::shell::keymap` 的用例（T-KEYS）。
//!
//! 这一层守两件事，且都不是「实现细节」：
//! 1. **三态配置语义**（默认 / 禁用 / 自定义 + 非法回落）——写错了用户改的
//!    键位就不是他以为的那个（`"none"` 被当成键名、坏串导致启动 panic 都属此类）；
//! 2. **与 kit 共用一个 keymap 的纪律**——`install` 必须幂等、必须留下 kit
//!    自己绑的键，否则「改一次快捷键，输入框里的键全废」。
//!
//! 涉及真实 keymap 的用例走 `TestAppContext`（要真的 `cx.key_bindings()`），
//! 其余是纯函数用例。

use std::any::TypeId;

use gpui_kit::{App, AsKeystroke, KeyBinding, Keystroke, Modifiers, TestAppContext};

use crate::ui::shell as sh;
use crate::ui::shell::keymap::{
    ACTIONS, Binding, KeyActionId, binding_of, conflict_with, def_of, display_spec, effective_spec,
    install, installed_bindings, is_ours, spec_from_keystroke, spec_is_valid,
};

/// 把 `config_key` 那格写成 `raw`（`""`/`"none"`/自定义串），其余保持默认。
fn with(mut keys: hzip_config::Keys, config_key: &str, raw: &str) -> hzip_config::Keys {
    *keys
        .field_mut(config_key)
        .unwrap_or_else(|| panic!("Keys 缺字段 {config_key}")) = raw.to_owned();
    keys
}

/// 造一个物理按键（`split_modifiers` 之类的平台路径与录制无关，直接构造）。
fn ks(modifiers: Modifiers, key: &str) -> Keystroke {
    Keystroke {
        modifiers,
        key: key.to_owned(),
        key_char: None,
    }
}

/// keymap 里属于**本模块**的绑定条数。
fn ours(cx: &App) -> usize {
    let map = cx.key_bindings();
    map.borrow()
        .bindings()
        .filter(|binding| is_ours(binding.action()))
        .count()
}

/// keymap 里**不属于**本模块的绑定条数（kit 组件/其它来源）。
fn foreign(cx: &App) -> usize {
    let map = cx.key_bindings();
    map.borrow()
        .bindings()
        .filter(|binding| !is_ours(binding.action()))
        .count()
}

/// keymap 里是否存在某个动作的绑定（按 `TypeId` 认动作）。
fn has_action(cx: &App, action: TypeId) -> bool {
    let map = cx.key_bindings();
    map.borrow()
        .bindings()
        .any(|binding| binding.action().as_any().type_id() == action)
}

/// 某动作当前绑的第一个键位。
fn spec_of(cx: &App, action: TypeId) -> Option<Keystroke> {
    let map = cx.key_bindings();
    map.borrow()
        .bindings()
        .find(|binding| binding.action().as_any().type_id() == action)
        .and_then(|binding| binding.keystrokes().first())
        .map(|keystroke| keystroke.as_keystroke().clone())
}

/// 表自身的完整性：14 条、id 唯一、label 由 config_key 拼出、默认串都能解析。
///
/// 「默认串能解析」是**硬约束**：`binding_for` 拿到的是过校验的串，而
/// `KeyBinding::new` 解析失败会 panic——默认串拼错 = 空配置启动即崩。
#[test]
fn actions_table_is_complete_and_defaults_are_parsable() {
    assert_eq!(ACTIONS.len(), 14, "14 条动作一一对应 [keys] 的 14 个字段");
    let mut ids: Vec<KeyActionId> = ACTIONS.iter().map(|def| def.id).collect();
    let total = ids.len();
    ids.sort_unstable_by_key(|id| *id as usize);
    ids.dedup();
    assert_eq!(ids.len(), total, "action id 必须两两不同");
    for def in ACTIONS {
        assert_eq!(
            def.label,
            format!("keymap.action.{}", def.config_key),
            "label 契约 = keymap.action.<config_key>"
        );
        assert!(
            spec_is_valid(def.default_spec),
            "{} 的默认键位 {:?} 不可解析",
            def.config_key,
            def.default_spec
        );
        assert_eq!(
            def_of(def.id).config_key,
            def.config_key,
            "def_of 必须回指同一表项"
        );
    }
}

/// 表里的 `config_key` 必须真的存在于 `hzip_config::Keys`（两侧防漂移）。
///
/// 只在两处都改动的情况下才可能出错，而 `Keys::field` 对认不出的键名返回
/// `None`（= 静默用默认键位）⇒ 没有这条用例，漂移会以「设置页改了没反应」
/// 的形式潜伏。
#[test]
fn every_config_key_exists_in_the_config_struct() {
    let mut keys = hzip_config::Keys::default();
    for def in ACTIONS {
        assert!(
            keys.field(def.config_key).is_some(),
            "hzip_config::Keys 缺字段 {}",
            def.config_key
        );
        assert!(
            keys.field_mut(def.config_key).is_some(),
            "hzip_config::Keys 缺可变字段 {}",
            def.config_key
        );
    }
    *keys.field_mut("open").expect("open 字段") = "f9".to_owned();
    assert_eq!(keys.field("open"), Some("f9"), "写进去必须读得出来");
    assert!(keys.field("no_such_key").is_none(), "未知键名 → None");
    assert!(keys.field_mut("no_such_key").is_none(), "未知键名 → None");
}

/// `spec_is_valid` 认真串、拒假串，且**任何输入都不 panic**（它是配置的第一道门）。
#[test]
fn spec_is_valid_accepts_real_specs_and_never_panics() {
    for spec in ["ctrl-a", "f5", "delete", "ctrl-,", "ctrl-shift-n", "alt-f4"] {
        assert!(spec_is_valid(spec), "{spec} 应合法");
    }
    for spec in ["ctrl--a", "kbd-nonsense-", "ctrl-a-b"] {
        assert!(!spec_is_valid(spec), "{spec} 应非法");
    }
    // 不 panic 本身就是断言：坏串只允许以 false 收场
    for spec in ["", " ", "-", "->", "ctrl->x", "😀", &"x".repeat(4096)] {
        let _ = spec_is_valid(spec);
    }
}

/// 三态映射（判定顺序：先空串/哨兵，再算自定义）。
#[test]
fn binding_of_maps_the_three_states() {
    assert_eq!(binding_of(""), Binding::Default);
    assert_eq!(binding_of("none"), Binding::Disabled);
    assert_eq!(binding_of("f9"), Binding::Custom("f9".to_owned()));
    // 哨兵只认小写；其余一律按自定义串处理（含首尾空格——不替用户猜）
    assert_eq!(binding_of("NONE"), Binding::Custom("NONE".to_owned()));
    assert_eq!(binding_of(" none"), Binding::Custom(" none".to_owned()));
}

/// `effective_spec`：空 → 默认、`none` → 无绑定、合法自定义 → 原样、非法 → 回落默认。
#[test]
fn effective_spec_covers_default_disabled_custom_and_garbage() {
    let open = def_of(KeyActionId::Open);
    assert_eq!(effective_spec(open, ""), Some("ctrl-o"));
    assert_eq!(effective_spec(open, "none"), None);
    assert_eq!(effective_spec(open, "f9"), Some("f9"));
    assert_eq!(
        effective_spec(open, "ctrl--a"),
        Some("ctrl-o"),
        "非法串回落内置默认（并留一条 warn）"
    );
    // 每条默认串都必须能被自己这一路走通
    for def in ACTIONS {
        assert_eq!(effective_spec(def, ""), Some(def.default_spec));
    }
}

/// 录制 → 回写：纯修饰键不产出键位，其余按固定顺序拼 ASCII 规范串。
#[test]
fn spec_from_keystroke_builds_canonical_ascii_specs() {
    let none = Modifiers::default();
    // 纯修饰键：Windows 上它们只产生 ModifiersChanged，不可能是键位
    for key in ["control", "alt", "shift", "platform", "function"] {
        assert_eq!(
            spec_from_keystroke(&ks(none, key)),
            None,
            "{key} 是纯修饰键"
        );
    }
    // 带着修饰位一起按的「纯 Ctrl」（模拟/其它平台可能这么上报）
    assert_eq!(
        spec_from_keystroke(&ks(
            Modifiers {
                control: true,
                ..Default::default()
            },
            "control"
        )),
        None
    );
    assert_eq!(spec_from_keystroke(&ks(none, "")), None, "空键名不产出键位");

    let ctrl_shift = Modifiers {
        control: true,
        shift: true,
        ..Default::default()
    };
    assert_eq!(
        spec_from_keystroke(&ks(ctrl_shift, "A")).as_deref(),
        Some("ctrl-shift-a")
    );
    assert_eq!(
        spec_from_keystroke(&ks(ctrl_shift, "a")).as_deref(),
        Some("ctrl-shift-a"),
        "大小写归一：键名一律小写"
    );
    assert_eq!(spec_from_keystroke(&ks(none, "F5")).as_deref(), Some("f5"));
    let all = Modifiers {
        control: true,
        alt: true,
        shift: true,
        ..Default::default()
    };
    assert_eq!(
        spec_from_keystroke(&ks(all, "delete")).as_deref(),
        Some("ctrl-alt-shift-delete"),
        "顺序固定 ctrl-alt-shift"
    );
    // platform（Win 键）无对应 token：忽略并留痕，其余修饰键照常拼
    assert_eq!(
        spec_from_keystroke(&ks(
            Modifiers {
                control: true,
                platform: true,
                ..Default::default()
            },
            "a"
        ))
        .as_deref(),
        Some("ctrl-a")
    );
    // 回写链的底线：录出来的串必须是「能落盘、能装配」的合法串
    for key in ["a", "f5", "delete", ",", "-", "escape"] {
        let spec = spec_from_keystroke(&ks(ctrl_shift, key)).expect("应产出键位");
        assert!(spec_is_valid(&spec), "录出的 {spec:?} 必须可解析");
    }
}

/// `display_spec` 是纯函数、坏串原样透传（显示串**不回写**配置）。
#[test]
fn display_spec_is_stable_and_passes_garbage_through() {
    assert_eq!(display_spec("f5"), "f5");
    assert_eq!(display_spec("ctrl--a"), "ctrl--a", "解析失败原样返回");
    let shown = display_spec("ctrl-a");
    assert_eq!(shown, display_spec("ctrl-a"), "同一输入必须同一显示");
    // 非 macOS 走 ASCII，单字符键被大写；macOS 出字形（这里不锁字形细节）
    let expected = if cfg!(target_os = "macos") {
        "^A"
    } else {
        "ctrl-A"
    };
    assert_eq!(shown, expected);
}

/// `is_ours` 只认本模块的 14 个动作（kit 动作必须判 false，否则 install 会吃掉它们）。
#[test]
fn is_ours_matches_only_our_own_actions() {
    assert!(is_ours(&sh::Open));
    assert!(is_ours(&sh::Quit));
    // 本模块产出的每一条（14 条互不相同的动作）都必须被认作我们的
    let defaults = installed_bindings(&hzip_config::Keys::default());
    assert_eq!(defaults.len(), 14, "默认配置应产出 14 条");
    assert!(defaults.iter().all(|binding| is_ours(binding.action())));
    // 反例：kit/gpui 自己的动作
    assert!(!is_ours(&gpui_kit::base::actions::Cancel));
    assert!(!is_ours(&gpui_kit::base::actions::SelectUp));
}

/// 禁用项不产出绑定（14 → 13），且自定义串按配置走。
#[test]
fn installed_bindings_skip_disabled_actions() {
    assert_eq!(installed_bindings(&hzip_config::Keys::default()).len(), 14);
    let keys = with(
        with(hzip_config::Keys::default(), "select_all", "none"),
        "open",
        "f9",
    );
    let bindings = installed_bindings(&keys);
    assert_eq!(bindings.len(), 13, "被禁用的一项不产出绑定");
    assert!(bindings.iter().all(|binding| is_ours(binding.action())));
    let has = |action: TypeId| {
        bindings
            .iter()
            .any(|binding| binding.action().as_any().type_id() == action)
    };
    assert!(
        !has(TypeId::of::<sh::SelectAll>()),
        "被禁用的动作不得产出绑定"
    );
    assert!(has(TypeId::of::<sh::Open>()));
    let open_spec = bindings
        .iter()
        .find(|binding| binding.action().as_any().type_id() == TypeId::of::<sh::Open>())
        .and_then(|binding| binding.keystrokes().first())
        .map(|keystroke| keystroke.as_keystroke().clone());
    assert_eq!(open_spec, Some(Keystroke::parse("f9").expect("f9")));
}

/// `install` 幂等，且**保留** kit 绑定的（真实 App）。
///
/// 这条用例守的是最坏的一类故障：`clear_key_bindings()` 是 App 级的，忘了
/// 回灌既有绑定 ⇒ 输入框/列表/对话框的键全部失灵；忘了幂等 ⇒ 泵每次设置
/// 生效都再叠一层绑定。
#[gpui_kit::test]
fn install_is_idempotent_and_keeps_foreign_bindings(cx: &mut TestAppContext) {
    let keys = hzip_config::Keys::default();
    cx.update(|cx: &mut App| {
        let foreign_action = TypeId::of::<gpui_kit::base::actions::Cancel>();
        cx.bind_keys([KeyBinding::new(
            "ctrl-shift-f9",
            gpui_kit::base::actions::Cancel,
            Some("Main"),
        )]);
        assert_eq!(ours(cx), 0, "装配前不该有我们的绑定");
        let foreign_before = foreign(cx);
        assert_eq!(foreign_before, 1, "只有刚绑的那条 kit 绑定");

        install(cx, &keys);
        assert_eq!(ours(cx), 14);
        assert_eq!(foreign(cx), foreign_before, "kit 绑定必须留下");
        assert!(has_action(cx, foreign_action));

        install(cx, &keys);
        assert_eq!(ours(cx), 14, "install 必须幂等（绑定数不增长）");
        assert_eq!(foreign(cx), foreign_before, "重复装配不得吃掉 kit 绑定");
        assert!(has_action(cx, foreign_action));
    });
}

/// `install` 落地三态：禁用不绑、自定义生效、非法回落默认（真实 App）。
#[gpui_kit::test]
fn install_applies_disabled_custom_and_fallback_states(cx: &mut TestAppContext) {
    let keys = with(
        with(
            with(hzip_config::Keys::default(), "select_all", "none"),
            "open",
            "f9",
        ),
        "copy",
        "ctrl--a",
    );
    cx.update(|cx: &mut App| {
        install(cx, &keys);
        assert_eq!(ours(cx), 13, "禁用一项 = 14 - 1");
        assert!(
            !has_action(cx, TypeId::of::<sh::SelectAll>()),
            "禁用项不得留任何绑定"
        );
        assert_eq!(
            spec_of(cx, TypeId::of::<sh::Open>()),
            Some(Keystroke::parse("f9").expect("f9")),
            "自定义键位必须生效"
        );
        assert_eq!(
            spec_of(cx, TypeId::of::<sh::CopyFiles>()),
            Some(Keystroke::parse("ctrl-c").expect("ctrl-c")),
            "非法串回落内置默认"
        );
    });
}

/// 冲突检测：命中占用者、排除自身、跳过禁用项、闲置键不误报。
#[test]
fn conflict_with_reports_the_occupant_only() {
    let keys = hzip_config::Keys::default();
    assert_eq!(
        conflict_with(&keys, "ctrl-a", KeyActionId::Quit).map(|def| def.config_key),
        Some("select_all")
    );
    assert_eq!(
        conflict_with(&keys, "ctrl-a", KeyActionId::SelectAll),
        None,
        "自身不算占用者"
    );
    assert_eq!(
        conflict_with(&keys, "ctrl-shift-f9", KeyActionId::Quit),
        None,
        "空闲键无冲突"
    );
    assert_eq!(
        conflict_with(&keys, "ctrl--a", KeyActionId::Quit),
        None,
        "非法串谈不上冲突"
    );
    // 具名键的大小写差异仍判为同一个键（parse 归一化）
    assert_eq!(
        conflict_with(&keys, "Delete", KeyActionId::Quit).map(|def| def.config_key),
        Some("delete")
    );
    // 自定义键位同样占位
    assert_eq!(
        conflict_with(
            &with(hzip_config::Keys::default(), "open", "f9"),
            "f9",
            KeyActionId::Quit
        )
        .map(|def| def.config_key),
        Some("open")
    );
    // 禁用项把键空出来
    assert_eq!(
        conflict_with(
            &with(hzip_config::Keys::default(), "select_all", "none"),
            "ctrl-a",
            KeyActionId::Quit
        ),
        None,
        "禁用项不占键位"
    );
    // 自定义非法串回落默认 ⇒ 仍占默认键位（否则改完反而能抢默认键）
    assert_eq!(
        conflict_with(
            &with(hzip_config::Keys::default(), "open", "ctrl--a"),
            "ctrl-o",
            KeyActionId::Quit
        )
        .map(|def| def.config_key),
        Some("open")
    );
}
