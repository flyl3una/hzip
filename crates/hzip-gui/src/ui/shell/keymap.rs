//! 可配置键位表（T-KEYS）：`[keys]` 配置节 ↔ gpui keymap 的**唯一事实源**。
//!
//! # 为什么要有这个模块
//!
//! 14 条菜单加速键原先内联在 `runtime.rs::run_ui` 的 `cx.bind_keys([...])` 里：
//! 只有「启动装配」一条路径——设置页既读不到它，也无从改它，改键位等于改代码。
//! 这里把它变成**数据**（[`ACTIONS`]）+ **三态配置语义**（[`Binding`]）+
//! 一组装配/查询函数，于是启动装配、设置页渲染、冲突检测、菜单快捷键提示
//! 全都读同一张表——不会再出现「表里改了、某处还留着一份写死的」。
//!
//! # 三态配置语义（与 `hzip_config::Keys` 的每个 `String` 字段一一对应）
//!
//! | 配置值 | [`Binding`] | 行为 |
//! |---|---|---|
//! | `""`（缺省） | [`Binding::Default`] | 绑该动作的 `default_spec` |
//! | `"none"` | [`Binding::Disabled`] | 不注册任何绑定（菜单仍可点，只是不显示提示） |
//! | 其它 | [`Binding::Custom`] | `Keystroke::parse` 通过才用；**非法回落默认并 `warn`** |
//!
//! 判定顺序**必须先查哨兵/空串，再进 parse**：`Keystroke::parse("none")` 不报错
//! （`none` 被当成普通键名），先 parse 会把「禁用」误判成合法自定义键位 ⇒
//! 用户写 `"none"` 却发现绑定还在。见 [`raw_kind`]。
//!
//! # 与 kit 自带绑定的关系（**最容易踩的坑**）
//!
//! `App::clear_key_bindings()` 是 **App 级**的：input/list/dialog/菜单等 kit
//! 组件自己绑的键也在同一个 keymap 里。因此 [`install`] 采取「留下非本模块的
//! 绑定，只换掉我们自己的 14 条」；裸清空会把整台应用的键盘交互打残。
//! 同一做法顺带保证**幂等**：连装两次不会让绑定数增长（泵在每次设置生效
//! 时都会调一次，非幂等就漏绑定）。
//!
//! # 安全约束
//!
//! `KeyBinding::new` 在**解析失败时会 panic**，所以 [`binding_for`] 只接受
//! 已经过 [`effective_spec`] 校验的串——绝不用未经校验的用户串构造绑定。

use std::any::TypeId;

use gpui_kit::{KeyBinding, Keystroke};

use crate::ui::shell as sh;

/// 绑定上下文：AppShell 主窗口的 `key_context`（与 `runtime.rs` 的 `Main`
/// 一致）。只有主窗口处于该上下文时这 14 条才参与匹配。
const CONTEXT: &str = "Main";

/// 「禁用」哨兵：配置写这个值 = 该动作不注册任何绑定。
const DISABLED: &str = "none";

/// 动作身份（[`ACTIONS`] 的键、[`binding_for`] 的分派依据、冲突检测的排除值）。
///
/// 加新动作的步骤固定为三步：这里加变体 → [`ACTIONS`] 加一行（含
/// `hzip_config::Keys` 的 `[keys]` 字段名与默认串）→ [`binding_for`] 加一条
/// arm。少任何一步都编译不过（`match` 穷尽 + 表项数测试），不会静默漏动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyActionId {
    /// 打开档案
    Open,
    /// 解压
    Extract,
    /// 新建窗口
    NewWindow,
    /// 全选
    SelectAll,
    /// 反选
    SelectInvert,
    /// 复制
    Copy,
    /// 粘贴
    Paste,
    /// 查找
    Find,
    /// 测试完整性
    TestIntegrity,
    /// 设置
    Settings,
    /// 任务中心
    ShowJobs,
    /// 刷新
    Refresh,
    /// 删除
    Delete,
    /// 退出
    Quit,
}

/// 可配置动作清单里的一项。
///
/// 这里**只放数据**、不放行为：动作本体（`sh::Open` 等）由 [`binding_for`]
/// 按 [`KeyActionId`] 现取。理由：本表要在 `const` 里构造、还要被设置页整表
/// 遍历，而 `Box<dyn Action>` 既不是 `Copy` 也不便 `Debug`/比较。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyActionDef {
    /// 动作身份。
    pub id: KeyActionId,
    /// 与 `hzip_config::Keys::field()` 的键名一一对应（= `[keys]` 的 TOML 字段名）。
    pub config_key: &'static str,
    /// i18n key：`keymap.action.<config_key>`（设置页用 `crate::tr` 取它）。
    pub label: &'static str,
    /// ASCII 规范串默认值。**必须能被 `Keystroke::parse` 解析**——否则启动
    /// 装配即 panic（`KeyBinding::new` 的硬要求），有测试守着。
    pub default_spec: &'static str,
}

/// 全部可配置动作（**顺序 = 设置页展示顺序**，与 `[keys]` 的字段顺序无关）。
///
/// 这张表是唯一事实源：启动装配（[`install`]）、设置页（`keys_section`）、
/// 冲突检测（[`conflict_with`]）、菜单快捷键提示（kit 反查 keymap）都读它。
pub const ACTIONS: &[KeyActionDef] = &[
    KeyActionDef {
        id: KeyActionId::Open,
        config_key: "open",
        label: "keymap.action.open",
        default_spec: "ctrl-o",
    },
    KeyActionDef {
        id: KeyActionId::Extract,
        config_key: "extract",
        label: "keymap.action.extract",
        default_spec: "ctrl-e",
    },
    KeyActionDef {
        id: KeyActionId::NewWindow,
        config_key: "new_window",
        label: "keymap.action.new_window",
        default_spec: "ctrl-shift-n",
    },
    KeyActionDef {
        id: KeyActionId::SelectAll,
        config_key: "select_all",
        label: "keymap.action.select_all",
        default_spec: "ctrl-a",
    },
    KeyActionDef {
        id: KeyActionId::SelectInvert,
        config_key: "select_invert",
        label: "keymap.action.select_invert",
        default_spec: "ctrl-i",
    },
    KeyActionDef {
        id: KeyActionId::Copy,
        config_key: "copy",
        label: "keymap.action.copy",
        default_spec: "ctrl-c",
    },
    KeyActionDef {
        id: KeyActionId::Paste,
        config_key: "paste",
        label: "keymap.action.paste",
        default_spec: "ctrl-v",
    },
    KeyActionDef {
        id: KeyActionId::Find,
        config_key: "find",
        label: "keymap.action.find",
        default_spec: "ctrl-f",
    },
    KeyActionDef {
        id: KeyActionId::TestIntegrity,
        config_key: "test_integrity",
        label: "keymap.action.test_integrity",
        default_spec: "ctrl-t",
    },
    KeyActionDef {
        id: KeyActionId::Settings,
        config_key: "settings",
        label: "keymap.action.settings",
        default_spec: "ctrl-,",
    },
    KeyActionDef {
        id: KeyActionId::ShowJobs,
        config_key: "show_jobs",
        label: "keymap.action.show_jobs",
        default_spec: "ctrl-j",
    },
    KeyActionDef {
        id: KeyActionId::Refresh,
        config_key: "refresh",
        label: "keymap.action.refresh",
        default_spec: "f5",
    },
    KeyActionDef {
        id: KeyActionId::Delete,
        config_key: "delete",
        label: "keymap.action.delete",
        default_spec: "delete",
    },
    KeyActionDef {
        id: KeyActionId::Quit,
        config_key: "quit",
        label: "keymap.action.quit",
        default_spec: "ctrl-q",
    },
];

/// 配置值解析出的三态语义（见模块文档的表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// 未配置（空串）：用内置默认键位。
    Default,
    /// `"none"`：禁用该动作的键位。
    Disabled,
    /// 自定义键位（**尚未校验**，是否合法问 [`spec_is_valid`]）。
    Custom(String),
}

/// 私有：原始串的分类（不分配、可比较），[`binding_of`] 与 [`effective_spec`]
/// 共用它，免得哨兵值在两处各写一份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawKind {
    Default,
    Disabled,
    Custom,
}

/// 原始配置串 → 三态（不校验合法性）。
///
/// 判定顺序即契约：先空串、再哨兵，最后才是「剩下的都算自定义串」。
/// **先判哨兵**是必须的——`none` 本身也能被 `Keystroke::parse` 解析成一个
/// 名叫 `none` 的键，先 parse 就永远走不到「禁用」分支。
fn raw_kind(raw: &str) -> RawKind {
    if raw.is_empty() {
        RawKind::Default
    } else if raw == DISABLED {
        RawKind::Disabled
    } else {
        RawKind::Custom
    }
}

/// 按动作身份取表项。
///
/// 表与枚举是同一份维护物（见 [`KeyActionId`] 的说明），找不到即代码错，
/// 故此处直接 `unreachable!`——让错误在启动/测试里立刻炸出来，而不是让某个
/// 动作悄悄没有键位。（不用 `expect`：`clippy::expect_used` 在本 workspace
/// 是 deny 级门禁。）
pub fn def_of(id: KeyActionId) -> &'static KeyActionDef {
    match ACTIONS.iter().find(|def| def.id == id) {
        Some(def) => def,
        None => unreachable!("ACTIONS 必须覆盖全部 KeyActionId"),
    }
}

/// 原始配置串 → 三态语义（`""` → 默认、`"none"` → 禁用、其余 → 自定义）。
pub fn binding_of(raw: &str) -> Binding {
    match raw_kind(raw) {
        RawKind::Default => Binding::Default,
        RawKind::Disabled => Binding::Disabled,
        RawKind::Custom => Binding::Custom(raw.to_owned()),
    }
}

/// 配置串是否是一段可用的键位（`Keystroke::parse` 成功，**不 panic**）。
///
/// 两个易踩的语义（调用方必读）：
/// - **空串解析成功**（`parse` 把空键名当合法）——但空串的语义是「用默认」
///   （[`binding_of`]），所以别拿空串来问合法性，先判哨兵；
/// - 纯修饰键名（如 `"control"`）也能解析成功，但 Windows 上纯修饰键不产生
///   keystroke，永远匹配不到——[`spec_from_keystroke`] 因此对它们返回 `None`，
///   正常路径不会把这种串写进配置。
pub fn spec_is_valid(spec: &str) -> bool {
    Keystroke::parse(spec).is_ok()
}

/// 借用版「有效键位」：禁用 → `None`；非法/空 → `default_spec`；合法 → 自定义串。
///
/// 返回的 `&'a str` 只与 `raw` 绑定：要么指向 `raw` 本身（自定义合法串），
/// 要么指向 `def.default_spec`（`'static`，自然满足 `'a`）。
///
/// 与 [`effective_spec`] 判定完全相同，但**不记日志**：冲突检测每按一个键都要
/// 遍历 14 条，坏配置在那条路径上会刷屏（它已在装配时 warn 过一次了）。
fn effective_spec_quiet<'a>(def: &KeyActionDef, raw: &'a str) -> Option<&'a str> {
    match raw_kind(raw) {
        RawKind::Default => Some(def.default_spec),
        RawKind::Disabled => None,
        RawKind::Custom => {
            if spec_is_valid(raw) {
                Some(raw)
            } else {
                Some(def.default_spec)
            }
        }
    }
}

/// 该动作**实际生效**的键位串；[`Binding::Disabled`] → `None`。
///
/// 非法自定义串回落 `default_spec` 并留一条 `warn`（配置是手写的，静默吞掉
/// 只会让用户以为「我的键位没生效是别的原因」）。返回的 `&'a str` 只与 `raw`
/// 绑定：要么指向 `raw`（自定义合法串），要么指向 `def.default_spec`。
pub fn effective_spec<'a>(def: &KeyActionDef, raw: &'a str) -> Option<&'a str> {
    let spec = effective_spec_quiet(def, raw);
    if raw_kind(raw) == RawKind::Custom && !spec_is_valid(raw) {
        tracing::warn!(
            config_key = def.config_key,
            raw,
            fallback = def.default_spec,
            "快捷键配置串非法，回落内置默认键位"
        );
    }
    spec
}

/// 键位串 → 给人看的显示串（`Keystroke::parse` → `Display`）。
///
/// **只用于显示，不可回写配置**：macOS 上是 `^⌥⌘⇧` 之类的字形，Windows 上
/// 单字符键会被大写（`ctrl-a` → `ctrl-A`），回写就变成了别名/别的键位。
/// 解析失败的串原样返回（宁可见者见其丑陋，也不显示成空）。
pub fn display_spec(spec: &str) -> String {
    Keystroke::parse(spec)
        .map(|keystroke| keystroke.to_string())
        .unwrap_or_else(|_| spec.to_owned())
}

/// 物理按键 → ASCII 规范串（录制回写配置用）。
///
/// 规则（顺序**固定**，与 gpui 内部 `unparse` 的 ctrl-alt-shift 口径一致）：
/// - 纯修饰键（key 就是 control/alt/shift/platform/function）→ `None`：
///   Windows 上纯修饰键只产生 `ModifiersChanged`，本就不该成为键位；
/// - 其余 → `ctrl-alt-shift-<key>`（缺失的修饰键段直接省略），key 统一小写。
///
/// `platform`（Win 键）与 `function`（Fn）修饰键**忽略并各留一条 `warn`**：
/// 项目键位串统一走 `ctrl-*` 口径，`win-` 在别的平台不可用，静默丢弃会让
/// 用户觉得「按了没反应」。
pub fn spec_from_keystroke(keystroke: &gpui_kit::Keystroke) -> Option<String> {
    let key = keystroke.key.to_ascii_lowercase();
    if key.is_empty()
        || matches!(
            key.as_str(),
            "control" | "alt" | "shift" | "platform" | "function"
        )
    {
        return None;
    }
    if keystroke.modifiers.platform {
        tracing::warn!(
            key = %keystroke.key,
            "platform（Win/Command）修饰键无对应键位 token，录制时忽略"
        );
    }
    if keystroke.modifiers.function {
        tracing::warn!(
            key = %keystroke.key,
            "function（Fn）修饰键无对应键位 token，录制时忽略"
        );
    }
    let mut spec = String::new();
    if keystroke.modifiers.control {
        spec.push_str("ctrl-");
    }
    if keystroke.modifiers.alt {
        spec.push_str("alt-");
    }
    if keystroke.modifiers.shift {
        spec.push_str("shift-");
    }
    spec.push_str(&key);
    Some(spec)
}

/// 某个动作是不是**本模块管辖**的 14 个动作之一（按 `TypeId` 判定）。
///
/// [`install`] 用它把「我们自己的绑定」从既有 keymap 里筛掉：kit 组件
/// （input/list/dialog/菜单）绑的键必须原样留下，否则整台应用键盘失灵。
pub fn is_ours(action: &dyn gpui_kit::Action) -> bool {
    own_action_type_ids().contains(&action.as_any().type_id())
}

/// 本模块 14 个动作的 `TypeId`（顺序与 [`ACTIONS`] 一致）。
///
/// 放函数里而非 `const`：`TypeId::of` 是否可 const 求值不该成为本模块的
/// 约束，而这里每次调用只是一次 14 项 `TypeId`（`u128`）比较，无分配。
fn own_action_type_ids() -> [TypeId; 14] {
    [
        TypeId::of::<sh::Open>(),
        TypeId::of::<sh::Extract>(),
        TypeId::of::<sh::NewWindow>(),
        TypeId::of::<sh::SelectAll>(),
        TypeId::of::<sh::SelectInvert>(),
        TypeId::of::<sh::CopyFiles>(),
        TypeId::of::<sh::Paste>(),
        TypeId::of::<sh::FindFile>(),
        TypeId::of::<sh::TestIntegrity>(),
        TypeId::of::<sh::Settings>(),
        TypeId::of::<sh::ShowJobs>(),
        TypeId::of::<sh::RefreshView>(),
        TypeId::of::<sh::DeleteFiles>(),
        TypeId::of::<sh::Quit>(),
    ]
}

/// 按 id + **已校验的**键位串造一条绑定。
///
/// 14 条 arm 是本模块与 `sh::*` 动作之间的唯一映射点。`spec` 必须来自
/// [`effective_spec`]（它已保证非空且可解析）——`KeyBinding::new` 遇到解析
/// 失败会 **panic**，而 panic 点会在 UI 线程上（启动或设置生效时）。
fn binding_for(id: KeyActionId, spec: &str) -> KeyBinding {
    match id {
        KeyActionId::Open => KeyBinding::new(spec, sh::Open, Some(CONTEXT)),
        KeyActionId::Extract => KeyBinding::new(spec, sh::Extract, Some(CONTEXT)),
        KeyActionId::NewWindow => KeyBinding::new(spec, sh::NewWindow, Some(CONTEXT)),
        KeyActionId::SelectAll => KeyBinding::new(spec, sh::SelectAll, Some(CONTEXT)),
        KeyActionId::SelectInvert => KeyBinding::new(spec, sh::SelectInvert, Some(CONTEXT)),
        KeyActionId::Copy => KeyBinding::new(spec, sh::CopyFiles, Some(CONTEXT)),
        KeyActionId::Paste => KeyBinding::new(spec, sh::Paste, Some(CONTEXT)),
        KeyActionId::Find => KeyBinding::new(spec, sh::FindFile, Some(CONTEXT)),
        KeyActionId::TestIntegrity => KeyBinding::new(spec, sh::TestIntegrity, Some(CONTEXT)),
        KeyActionId::Settings => KeyBinding::new(spec, sh::Settings, Some(CONTEXT)),
        KeyActionId::ShowJobs => KeyBinding::new(spec, sh::ShowJobs, Some(CONTEXT)),
        KeyActionId::Refresh => KeyBinding::new(spec, sh::RefreshView, Some(CONTEXT)),
        KeyActionId::Delete => KeyBinding::new(spec, sh::DeleteFiles, Some(CONTEXT)),
        KeyActionId::Quit => KeyBinding::new(spec, sh::Quit, Some(CONTEXT)),
    }
}

/// 由配置算出的**待安装**绑定（禁用项不产出条目）。
///
/// `Keys::field` 返回 `None`（键名对不上）按空串处理 = 用默认：让「配置节里
/// 加了字段但表还没跟上」退化成本来的默认键位，而不是启动 panic。
pub fn installed_bindings(keys: &hzip_config::Keys) -> Vec<KeyBinding> {
    ACTIONS
        .iter()
        .filter_map(|def| {
            let raw = keys.field(def.config_key).unwrap_or("");
            effective_spec(def, raw).map(|spec| binding_for(def.id, spec))
        })
        .collect()
}

/// 把 `spec` 是否已被**别的**动作占用算出来（占用者 = 返回的表项）。
///
/// - 只比「有效键位」：禁用项不占键（用户刚把某动作禁用，就应该能拿走那个键）；
/// - `exclude` 用于排除「正在改的这个动作」——否则它自己永远是最先命中的占用者；
/// - 比较按解析后的 `Keystroke`（大小写/写法差异不影响），任一侧解析失败才
///   退回字符串比较（见 [`same_keystroke`]）。
pub fn conflict_with(
    keys: &hzip_config::Keys,
    spec: &str,
    exclude: KeyActionId,
) -> Option<&'static KeyActionDef> {
    if !spec_is_valid(spec) {
        // 非法串根本进不了表，谈不上冲突（调用方应先据 spec_is_valid 拒掉）
        return None;
    }
    ACTIONS.iter().find(|def| {
        if def.id == exclude {
            return false;
        }
        let raw = keys.field(def.config_key).unwrap_or("");
        effective_spec_quiet(def, raw).is_some_and(|taken| same_keystroke(taken, spec))
    })
}

/// 两个键位串是否指同一个物理按键。
///
/// `Keystroke::parse` 会归一化修饰键顺序与大小写（`CTRL-A` 与 `ctrl-shift-a`
/// 其实都指向 ctrl+shift+a）⇒ 直接比字符串会把「同一个键的两种写法」当成不冲突。
/// 解析失败（配置里的坏值）才退回字面比较。
fn same_keystroke(a: &str, b: &str) -> bool {
    match (Keystroke::parse(a), Keystroke::parse(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// 按配置装配全部键位（**幂等**，且不碰 kit 自带绑定）。仅 UI 线程可调。
///
/// 步骤即 [`is_ours`] 的用途说明：先快照既有绑定 → 清空 → 回灌「非本模块的」
/// 绑定并接上我们这批。两个后果都是刻意的：
/// 1. kit 组件（input/list/dialog/菜单）的键位原样保留；
/// 2. 重复调用不会累积绑定（清空后回灌的是同一批快照 + 同一批计算值）。
///
/// 装配后 kit 会重绘窗口（`bind_keys`/`clear_key_bindings` 都会推
/// `Effect::RefreshWindows`），菜单项的快捷键提示因此自动跟随。
///
/// # 调用点的取值口径（**别读盘**）
///
/// 启动装配传 `config.keys`（`run_ui` 早已 `Config::load()` 过一次）。
/// 设置生效时由泵装配，传的是 **AppData 内存镜像** `settings.keys`——与
/// `theme_replay`/`density`/`file_icons` 同款口径：`store_keys` 同拍就更新镜像，
/// 而落盘是异步的（读盘会拿到上一拍的值，`cfg!(test)` 下 `persist` 根本不落盘
/// ⇒ 读盘会命中开发机的真实 `config.toml`）。键位没有「只读配置就能重装」的
/// 第二入口——**泵不读盘**，免得又开一条与异步落盘赛跑的路。
pub fn install(cx: &mut gpui_kit::App, keys: &hzip_config::Keys) {
    let existing: Vec<KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
    cx.clear_key_bindings();
    cx.bind_keys(
        existing
            .into_iter()
            .filter(|binding| !is_ours(binding.action()))
            .chain(installed_bindings(keys)),
    );
}
