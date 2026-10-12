//! 主题 token → gpui-kit ActiveTheme 覆盖（06 §4.9；T-G00 独占，W2 起只读）。
//!
//! # 设计（06 §4.9）
//!
//! - gpui-kit 自带语义主题（`Theme`/`ActiveTheme`）挂这套浅色值；
//! - 深色主题用 gpui-kit 内置暗色（`Theme::change(ThemeMode::Dark, …)`），不另造 token；
//! - 表格列出的 token 集为 06 原型值；`ActiveTheme` 的各语义槽位映射见 [`apply_light_theme`]。
//!
//! # 红线（T-G10 §4）
//!
//! UI 各区渲染全程引用 `ActiveTheme`（`cx.theme().xxx`），**禁止裸色值**。

use crate::ui::constant::SELECTION_ROW_BG_ALPHA;
use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::component::theme::{Theme, ThemeConfig, ThemeMode};
use gpui_kit::{App, Hsla, Rgba};
use std::rc::Rc;

/// 浅色 token（06 §4.9 表；hex 原值）。
pub mod light {
    /// 窗口底 / 内容区底色 —— **与菜单/工具栏同色**（用户口径 2026-10-08：
    /// 「内容界面的背景颜色也应该和菜单/工具栏的背景颜色一致（根据浅色深色
    /// 模式决定白/黑）」）。旧值 `#f7f7f5`（比栏色暗一档的灰底）会让内容区与
    /// 菜单/工具栏并排时出现可见色差。
    pub const BG: &str = SURFACE;
    /// 栏/卡片 `#ffffff`
    pub const SURFACE: &str = "#ffffff";
    /// 悬停 `#eef0f2`
    pub const SURFACE_HOVER: &str = "#eef0f2";
    /// 分隔线 `#d8dade`
    pub const BORDER: &str = "#d8dade";
    /// 主文字 `#1c1f23`
    pub const TEXT: &str = "#1c1f23";
    /// 次文字 `#5c6670`
    pub const MUTED: &str = "#5c6670";
    /// 主色 `#2f6fed`
    pub const ACCENT: &str = "#2f6fed";
    /// 选中底 `#e3ecfd`
    pub const ACCENT_SOFT: &str = "#e3ecfd";
    /// 警告 `#b8860b`
    pub const WARN: &str = "#b8860b";
    /// 危险 `#c62828`
    pub const DANGER: &str = "#c62828";
    /// 成功 `#1a7f37`
    pub const SUCCESS: &str = "#1a7f37";
    /// 主色 hover `#2563d9`（T-GUI T01：ACCENT 加深一档）
    pub const ACCENT_HOVER: &str = "#2563d9";
    /// 主色按下 `#1e56bd`（T-GUI T01：ACCENT 加深两档）
    pub const ACCENT_ACTIVE: &str = "#1e56bd";
}

/// 深色 token（T-GUI T25：同一批槽位的深色值——浅深两套视觉语言一致；
/// 交互高亮/选中/聚焦语义仍由 ACCENT 驱动）。
pub mod dark {
    /// 窗口底 / 内容区底色 —— **与菜单/工具栏同色**（用户口径 2026-10-08；
    /// 旧值 `#17191c` 近黑，比栏色 `#1f2226` 暗一档 ⇒ 内容区看起来「是黑的」
    /// 而菜单/工具栏不是）。
    pub const BG: &str = SURFACE;
    /// 栏/卡片 `#1f2226`
    pub const SURFACE: &str = "#1f2226";
    /// 悬停 `#262a2f`
    pub const SURFACE_HOVER: &str = "#262a2f";
    /// 分隔线 `#33383f`
    pub const BORDER: &str = "#33383f";
    /// 主文字 `#e6e8ea`
    pub const TEXT: &str = "#e6e8ea";
    /// 次文字 `#9aa3ad`
    pub const MUTED: &str = "#9aa3ad";
    /// 主色 `#4f83f2`（深色底上提高亮度保证对比度）
    pub const ACCENT: &str = "#4f83f2";
    /// 选中底 `#1c2b47`
    pub const ACCENT_SOFT: &str = "#1c2b47";
    /// 警告 `#d9a441`
    pub const WARN: &str = "#d9a441";
    /// 危险 `#e05252`
    pub const DANGER: &str = "#e05252";
    /// 成功 `#3fb45f`
    pub const SUCCESS: &str = "#3fb45f";
    /// 主色 hover `#6593f5`
    pub const ACCENT_HOVER: &str = "#6593f5";
    /// 主色按下 `#7ba4f7`
    pub const ACCENT_ACTIVE: &str = "#7ba4f7";
}

/// 初始化主题（浅色实值 + 默认 Light 模式；`run_ui` 装配时调用一次）。
pub fn init(cx: &mut gpui_kit::App) {
    apply_light_theme(cx);
    Theme::change(ThemeMode::Light, None, cx);
}

/// 将 06 §4.9 浅色 token 挂到 gpui-kit `Theme`（`ThemeConfig` 语义槽位覆写）。
/// - `warn/danger/success` → `warning.background` / `danger.background` / `success.background`
///
/// T-GUI T01 修正：JSON key 必须与 `ThemeConfigColors` 字段名严格一致
///（serde 不拒绝未知 key，拼错即静默失效）——`"primary"` →
/// `"primary.background"`；`"surface"`/`"surface.foreground"` →
/// `"popover.background"`/`"popover.foreground"`。key 合法性由
/// [`tests::theme_json_keys_are_known`] 门禁。
pub fn theme_json(mode: ThemeModeChoice) -> serde_json::Value {
    // T-GUI T25：浅/深两套 token 同一批槽位；交互高亮语义由两套各自的
    // ACCENT 驱动（深色底用提亮蓝 #4f83f2 保证对比度）。
    let t = mode;
    let name = match t {
        ThemeModeChoice::Light => "hzip-light",
        ThemeModeChoice::Dark => "hzip-dark",
        ThemeModeChoice::System => "hzip-light",
    };
    let mode_str = match t {
        ThemeModeChoice::Dark => "dark",
        _ => "light",
    };
    let (
        bg,
        fg,
        border,
        surface,
        text,
        muted,
        accent,
        accent_soft,
        warn,
        danger,
        success,
        accent_hover,
        accent_active,
    ) = match t {
        ThemeModeChoice::Dark => (
            dark::BG,
            dark::TEXT,
            dark::BORDER,
            dark::SURFACE,
            dark::TEXT,
            dark::MUTED,
            dark::ACCENT,
            dark::ACCENT_SOFT,
            dark::WARN,
            dark::DANGER,
            dark::SUCCESS,
            dark::ACCENT_HOVER,
            dark::ACCENT_ACTIVE,
        ),
        _ => (
            light::BG,
            light::TEXT,
            light::BORDER,
            light::SURFACE,
            light::TEXT,
            light::MUTED,
            light::ACCENT,
            light::ACCENT_SOFT,
            light::WARN,
            light::DANGER,
            light::SUCCESS,
            light::ACCENT_HOVER,
            light::ACCENT_ACTIVE,
        ),
    };
    serde_json::json!({
        "name": name,
        "mode": mode_str,
        "colors": {
            "background": bg,
            "foreground": fg,
            "border": border,
            // primary 语义槽（T-GUI T01 修正：原 `"primary"` 是无效 key）
            "primary.background": accent,
            "primary.foreground": surface,
            "primary.hover.background": accent_hover,
            "primary.active.background": accent_active,
            // popover（T-GUI T01 修正）
            "popover.background": surface,
            "popover.foreground": text,
            // 输入框/下拉聚焦环
            "ring": accent,
            // 树/列表/表格选中态
            "list.active.background": accent,
            "list.active.border": accent,
            "table.active.background": accent,
            "table.active.border": accent,
            // 输入框默认边框与全局 border 一致
            "input.border": border,
            // 滑块（T-GUI 09 压缩级别滑块）：显式声明，不靠 fallback。
            // 注意真实 key 是 `slider.background`（gpui-component schema.rs:534），
            // 不是 default-theme.json 里写的 `slider.bar.background`——后者是无效 key，
            // 会被 serde 静默丢弃（本文件 `theme_json_keys_are_known` 门禁正是防这个）。
            "slider.background": accent,
            "slider.thumb.background": surface,
            "accent.background": accent,
            "accent.foreground": surface,
            "muted.foreground": muted,
            "selection.background": accent_soft,
            "list.hover.background": match t { ThemeModeChoice::Dark => dark::SURFACE_HOVER, _ => light::SURFACE_HOVER },
            "secondary.background": match t { ThemeModeChoice::Dark => dark::SURFACE_HOVER, _ => light::SURFACE_HOVER },
            "warning.background": warn,
            "danger.background": danger,
            "success.background": success,
            // 滚动条：**固定中性灰**（浅/深两套主题同值）。kit 的
            // `scrollbar.thumb.background` 语义槽 fallback = `accent`（主题色）⇒
            // 不声明就随主题色染色（用户实测不想要）；轨道透明 = 只留滑块。
            // 键名是**带点路径**（kit schema `#[serde(rename = "scrollbar.thumb.background")]`），
            // 写错会被 serde 静默丢弃——`theme_json_keys_are_known` 门禁守着这条。
            "scrollbar.background": "#00000000",
            "scrollbar.thumb.background": SCROLLBAR_THUMB,
            "scrollbar.thumb.hover.background": SCROLLBAR_THUMB_HOVER,
        }
    })
}

/// 滚动条滑块**常态灰**（固定值，不随主题色/主题模式变——用户口径 2026-09-24：
/// 「滚动条默认都使用一般滚动条用的灰色，不需要跟随主题色调整」）。
///
/// 为什么必须显式声明：kit 的 `scrollbar_thumb` 语义槽 **fallback = `accent`**
/// （主题色），不声明就会随主题色染色。
const SCROLLBAR_THUMB: &str = "#A3A3A3E6";
/// 滚动条滑块**悬停灰**（比常态深一档；同样固定值）。
const SCROLLBAR_THUMB_HOVER: &str = "#A3A3A3";

/// 兼容名（T-GUI T01 引入；T25 重构后为 [`theme_json`] 特例）。
pub fn light_theme_json() -> serde_json::Value {
    theme_json(ThemeModeChoice::Light)
}

/// 主题色派生家族（T-SEL 走查 #3：base + hover/active 两档 + soft 选底）。
///
/// 槽位覆盖面 = `theme_json` 中所有 ACCENT 驱动槽（primary 按钮/ring 聚焦/
/// list·table 选中/slider 轨道/accent 背景/selection 软底）——自定义后
/// 全部交互高亮语义跟随新色。
pub(crate) struct ThemeColorFamily {
    /// 主色（hex）
    base: String,
    /// hover（提亮一档）
    hover: String,
    /// active（压暗一档）
    active: String,
    /// 软底（15% alpha 的选底/`selection.background`）
    soft: String,
}

/// hex（`#RRGGBB`）→ Hsla（解析失败 → None）——T-SEL 走查 #3 公开工具
///（设置面板 ColorPicker 装配/回写用）。
pub fn parse_theme_hex(hex: &str) -> Option<Hsla> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    let [_, r, g, b] = v.to_be_bytes().map(|b| (b as f32) / 255.0);
    Some(Hsla::from(Rgba { r, g, b, a: 1.0 }))
}

/// Hsla → hex `#RRGGBB`（alpha 丢弃——主题色不支持半透明主色）。
/// T-SEL 走查 #3 公开工具（picker 上行 Text 载荷用）。
pub fn theme_hex_of(c: Hsla) -> String {
    let Rgba { r, g, b, .. } = Rgba::from(c);
    format!(
        "#{:02X}{:02X}{:02X}",
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8
    )
}

/// 亮度偏移（clamp 0..1；浅色主色提亮一档 = hover、压暗 = active）。
fn shift_l(c: Hsla, delta: f32) -> Hsla {
    Hsla {
        l: (c.l + delta).clamp(0.0, 1.0),
        ..c
    }
}

/// 主题色家族派生（hex 解析失败 → None = 不覆盖）。
pub(crate) fn theme_color_family(hex: &str) -> Option<ThemeColorFamily> {
    let base = parse_theme_hex(hex)?;
    Some(ThemeColorFamily {
        base: theme_hex_of(base),
        hover: theme_hex_of(shift_l(base, 0.08)),
        active: theme_hex_of(shift_l(base, -0.08)),
        soft: format!("{}26", theme_hex_of(base)), // 0x26 ≈ 15% alpha（RGBA hex）
    })
}

/// 主题色槽位覆盖 merge（`theme_json` 的 colors 之上——ACCENT 驱动槽
/// 全部换新色家族）。
fn merge_theme_color(json: &mut serde_json::Value, family: &ThemeColorFamily) {
    let Some(colors) = json.get_mut("colors").and_then(|c| c.as_object_mut()) else {
        return; // theme_json 恒含 colors——防御分支（不 panic）
    };
    for key in [
        "primary.background",
        "primary.hover.background",
        "primary.active.background",
        "accent.background",
        "ring",
        "list.active.background",
        "list.active.border",
        "table.active.background",
        "table.active.border",
        "slider.background",
        "link",
    ] {
        colors.insert(key.into(), serde_json::json!(family.base));
    }
    colors.insert(
        "primary.hover.background".into(),
        serde_json::json!(family.hover),
    );
    colors.insert(
        "primary.active.background".into(),
        serde_json::json!(family.active),
    );
    colors.insert(
        "selection.background".into(),
        serde_json::json!(family.soft),
    );
}

/// 注入界面字体（`ThemeConfig` 的 `font.family` / `font.size`——注意 serde
/// `rename` 是**带点**的键名，写成 `font_family` 会被静默丢弃）。
///
/// 单独成函数是为了可测：serde 不拒绝未知 key，路径写错只会**静默失效**
/// （门禁 `font_config_lands_in_theme_config` 正是防这个）。
///
/// `family` 为空 = 保持主题默认（`.SystemUIFont` = 系统界面字体）；
/// `size_px` 是**全界面 rem 基准**——0.7.1 起由 kit 的 `WindowState` 插件在
/// `gpui_base::Root::render` 流程内 `set_rem_size(cx.theme().font_size)` 落定
/// （插件 `prepare` 钩子，gpui-component-0.7.1/src/root.rs:434-435）。
fn merge_font_config(json: &mut serde_json::Value, family: &str, size_px: u32) {
    let Some(root) = json.as_object_mut() else {
        return;
    };
    if !family.is_empty() {
        root.insert("font.family".into(), serde_json::json!(family));
    }
    root.insert("font.size".into(), serde_json::json!(size_px as f32));
}

/// 将 hzip token 挂到 gpui-kit `Theme`（`ThemeConfig` 语义槽位覆写）。
///
/// T-GUI T25：浅/深两套覆写（此前 Dark 分支直接用组件库内置暗色，
/// hzip 语义色全部失效）。
///
/// T-SEL 走查 #3：`[ui].theme_color` 非空时主色家族（primary/accent/
/// ring/选中/滑块）整体覆盖——设置页 ColorPicker 写入，本函数为唯一
/// 生效点（冷启动 `init` / 主题切换 / 主题色变更共用）。读盘仅在设置
/// 变更路径（低频信号，非渲染帧——T-PERF3 纪律不破）。
pub fn apply_theme(choice: ThemeModeChoice, cx: &mut gpui_kit::App) {
    let cfg = hzip_config::Config::load();
    apply_theme_cfg(choice, &cfg, cx);
}

/// 主题重放的**内存取值**（L1：重放不读盘）。
///
/// 为什么需要（用户实报 2026-10-09「界面字号/圆角点一次不生效、再点一次才
/// 生效」）：设置落盘是**异步**的（`hzip_config::Config::save_async`），而旧实现的
/// 重放内部 `Config::load()` 读盘——信号到达（微秒级）时盘上还是**上一拍**的值
/// （实测 200/200 陈旧）⇒ 每次点击都差一拍。现在值由泵在**同一把锁内**从内存
/// 镜像（`AppData.settings`）取出后传入（与 `theme_ix` / `theme_color` 同源），
/// 与快照同拍。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeReplay {
    /// `[ui].theme` 标签下标（[`ThemeModeChoice::from_index`]）
    pub mode_ix: usize,
    /// `[ui].theme_color`（空 = 主题默认）
    pub theme_color: String,
    /// `[ui].font_family`（空 = 系统默认）
    pub family: String,
    /// `[ui].font_size`（px）
    pub size_px: u32,
    /// `[ui].corner_radius`（px；0 = 直角）
    pub radius_px: u32,
}

/// 主题取值（[`apply_theme_cfg`] 从**注入配置**取；[`apply_theme_replay`] 从
/// **内存镜像**取）——内核只认值，不认识来源，故两条路径共用一份应用逻辑。
struct ThemeValues<'a> {
    choice: ThemeModeChoice,
    theme_color: &'a str,
    family: &'a str,
    size_px: u32,
    radius_px: u32,
}

/// 按**显式取值**重放主题（不读盘）。
///
/// 泵的 `SettingApplied(Theme | ThemeColor | FontFamily | FontSize | CornerRadius)`
/// 生效半场调用；`&mut App` 是因为主题是全局态（快照不携带）。
pub fn apply_theme_replay(r: &ThemeReplay, cx: &mut gpui_kit::App) {
    let choice = ThemeModeChoice::from_index(r.mode_ix).unwrap_or(ThemeModeChoice::Light);
    // 与旧 `ThemeModeChoice::apply` 同序：先切 kit 的配色模式，再挂 hzip 覆写。
    let kit_mode = match choice {
        ThemeModeChoice::Dark => ThemeMode::Dark,
        ThemeModeChoice::Light | ThemeModeChoice::System => ThemeMode::Light,
    };
    Theme::change(kit_mode, None, cx);
    apply_theme_values(
        ThemeValues {
            choice,
            theme_color: r.theme_color.trim(),
            family: r.family.trim(),
            size_px: r.size_px,
            radius_px: r.radius_px,
        },
        cx,
    );
}

/// [`apply_theme`] 的**可注入配置**版本（测试用；生产路径一律
/// [`apply_theme`] / [`apply_theme_replay`]）。
///
/// 拆出来的原因：圆角/字体/主题色都从 `Config` 读，而 `Config::load()` 走
/// `HZIP_CONFIG`/用户配置目录（进程级全局态）——测试要验「配置值 → 主题」的
/// 映射，只能靠注入，不能去改全局配置文件。
pub fn apply_theme_cfg(choice: ThemeModeChoice, cfg: &hzip_config::Config, cx: &mut gpui_kit::App) {
    apply_theme_values(
        ThemeValues {
            choice,
            theme_color: cfg.ui.theme_color.trim(),
            family: cfg.ui.font_family.trim(),
            size_px: cfg.ui.font_size_px(),
            radius_px: cfg.ui.corner_radius_px(),
        },
        cx,
    );
}

/// 主题应用内核（取值即用；唯一应用点）。
fn apply_theme_values(v: ThemeValues<'_>, cx: &mut gpui_kit::App) {
    let mut json = theme_json(v.choice);
    let theme_color = v.theme_color;
    if !theme_color.is_empty()
        && let Some(family) = theme_color_family(theme_color)
    {
        merge_theme_color(&mut json, &family);
    }
    // 界面字体（`[ui].font_family` / `[ui].font_size`）：一次注入全局 Theme。
    //
    // 为什么这两个设置都挂在 Theme 上就够：窗口根的 gpui-component `Root`
    // 每帧渲染时做两件事——`window.set_rem_size(cx.theme().font_size)` 与根 div
    // 的 `.font_family(cx.theme().font_family)`（root.rs）。rem 是**窗口级**基准
    // ⇒ 字号一处改、每个窗口（主窗/进度窗/参数窗/预览窗）各自生效，文字与
    // rem 间距等比缩放；字体族随树继承 ⇒ 连组件库自绘的按钮/下拉文案一起换。
    merge_font_config(&mut json, v.family, v.size_px);
    match serde_json::from_value::<ThemeConfig>(json) {
        Ok(config) => Theme::global_mut(cx).apply_config(&Rc::new(config)),
        Err(e) => {
            tracing::error!("hzip {:?} 主题配置解析失败（用组件库默认）: {e}", v.choice);
        }
    }
    // T-SEL 走查 #2：关闭 kit 的 active_highlight（选中行的边框+高亮
    // 覆盖子层）——选中视觉统一为各视图自绘的纯色背景（多选行为
    // render_tr/mode 行的 selection_row_bg tint；无叠加边框）
    Theme::global_mut(cx).list.active_highlight = false;
    // 滚动条**默认常显**（用户实测：设置弹窗右侧内容可以滚轮翻页，却看不到
    // 滚动条，不知道还能往下滚）。kit 的 `Scrollbar` 在未显式设 mode 时取
    // **主题**的 `scrollbar_mode`（gpui-base `Scrollbar::mode` 文档："If unset,
    // the current application theme projection is used"），kit 的 `Scrollable`
    // 包装（`overflow_y_scrollbar()`）正是走这条 ⇒ 改主题一处，全应用所有
    // 滚动区（设置弹窗正文、文件列表、目录树、预览…）的滚动条都常显。
    // 注意：本行在 `apply_config` **之后**——`apply_config` 会用配置里的
    // `scrollbar_mode`（缺省 `Scrolling` = 滚动时才出现、随后淡出）覆盖掉。
    Theme::global_mut(cx).scrollbar_mode = ScrollbarMode::Always;
    // **圆角（用户可配：`[ui].corner_radius`，0 = 直角）**：kit 的 `PopupMenu`
    // 圆角取全局 `theme.radius`（容器走 `popover_style`、菜单项走 `radius.min(8px)`），
    // 组件级**没有**样式钩子（`PopupMenu` 未实现 `Styled`，`RenderOptions` 私有）
    // ⇒ 唯一旋钮在此，工具按钮/输入框/页签关闭钮等也都读它。
    //
    // `0` 是 kit 的**受支持方形模式**（`styled.rs` 文档：`Theme::radius` 为零即
    // 「a UI that is square everywhere else」，`radius_full()` 随之归零，徽标/
    // 头像不残留圆形）。
    //
    // `radius_lg`（Dialog/Notification 专用档）按 kit 的默认档比 6:8 派生：
    // 0 → 0（连对话框一起方）、6 → 8（= kit 原默认，观感不变）、20 → 26。
    //
    // 必须在 `apply_config` **之后**（配置里带 radius 会被它覆盖）。
    {
        let r = v.radius_px as f32;
        let theme = Theme::global_mut(cx);
        theme.radius = gpui_kit::px(r);
        theme.radius_lg = gpui_kit::px(r * 4.0 / 3.0);
    }
}

/// 统一选中色（T-SEL 走查 #4 收口：**当前主题背景 × 主题色 30% 混合**
/// 的不透明色——比主题色浅、明显可见；浅/深主题自动适配（各自取
/// `background`/`accent` 合成）。主题色自定义经 `accent.background`
/// 槽自动跟随。三种浏览模式（表格/图标/紧凑列表）与最近点击行
/// （盖掉 kit 单选 accent 背景）共用同一来源。
pub fn selection_row_bg(cx: &App) -> Hsla {
    use gpui_kit::component::ActiveTheme as _;
    cx.theme()
        .background
        .blend(cx.theme().accent.opacity(SELECTION_ROW_BG_ALPHA))
}

/// 浅色覆写（`init` 装配路径；T-GUI T25 起为 [`apply_theme`] 特例）。
pub fn apply_light_theme(cx: &mut gpui_kit::App) {
    apply_theme(ThemeModeChoice::Light, cx);
}

#[cfg(test)]
#[path = "tests/theme.rs"]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeModeChoice {
    /// 跟随系统
    System,
    /// 浅色
    Light,
    /// 深色
    Dark,
}

impl ThemeModeChoice {
    /// 白名单 label **key**（顺序 = DialogEvent::SettingsTheme 下标）。
    ///
    /// **派生自** [`crate::app::labels::THEME`]（单一白名单来源，勿手抄）——
    /// 手写副本会与设置注册表漂移（下标语义 = 同一张表）。
    pub const LABEL_KEYS: [&'static str; 3] =
        crate::app::labels::label_keys(crate::app::labels::THEME);

    /// 本地化展示名。
    pub fn label(self) -> String {
        crate::tr(self.key())
    }

    /// 本模式的 i18n key。
    pub fn key(self) -> &'static str {
        Self::LABEL_KEYS[match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }]
    }

    /// 本地化白名单序列（下标 = [`Self::from_index`] 输入）。
    pub fn labels() -> Vec<String> {
        Self::LABEL_KEYS.iter().map(|k| crate::tr(k)).collect()
    }

    /// 下标 → 枚举。
    pub fn from_index(i: usize) -> Option<Self> {
        match i {
            0 => Some(Self::System),
            1 => Some(Self::Light),
            2 => Some(Self::Dark),
            _ => None,
        }
    }
}
