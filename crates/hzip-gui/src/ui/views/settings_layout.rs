//! 设置面板布局（R2-03 独占；R-GUI2 06 §4.8 → 七分类 Tabs）。
//!
//! # 结构
//!
//! [`SettingsPanel`] = gpui Entity（选中页内持，不外发）：
//!
//! ```text
//! Dialog Modal（遮罩/居中/Escape 内建，宽 680px——7 页签一行）
//! └ title「设置」+ 右上 ✕
//!   ├ 竖排页签轨道：常规 / 文件关联 / 上下文菜单 / 解压设置 / 压缩设置 / 查看器 / 语言
//!   ├ 当前分类页内容（views::settings_render 注册表驱动——页内容不手写）
//!   └ footer：[恢复默认] [关闭]
//! ```
//!
//! # 数据与刷新（T-SET3 核心）
//!
//! 面板**不持冻结快照**：`view: SettingsView` 为活读投影（AppData 镜像 +
//! `Config`），每次事件后 [`SettingsPanel::reload`] 重读重建 —— 修掉旧实现
//! 「控件回弹 / 面板显示陈旧值」（改完不刷新，只认打开那一刻的快照）。
//!
//! # 事件
//!
//! - 值控件（Switch/Checkbox/Select/列块/动作）一律上行
//!   `DialogEvent::SettingChanged { id, value }` → `AppData::apply_setting`
//!   五步流程（归一化 → 落库 → 落盘 → 生效分派 → 刷新），落地后端刷新；
//! - Reset/Close/Escape/✕：`SettingsReset` / `SettingsClose`（Escape 与遮罩
//!   点击经 Dialog 内建关闭路径触发 `on_close` → `SettingsClose`）。
//!
//! Select 的确认事件在装配期订阅（`SelectState` 窗口亲和需 `&mut Window`）。

use crate::ui::constant::{HAIRLINE, TAB_RAIL_MARK_H, TAB_RAIL_MARK_W};
use crate::ui::spacing::sp;
use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::IndexPath;
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, IntoElement, Styled, WeakEntity, Window, div};

use crate::app::settings_registry::{self, SettingValue, SettingsTab, SettingsView, WidgetKind};
use crate::ui::contracts::DialogEvent;
use crate::ui::shell::AppDataHandle;
use crate::ui::views::settings_render::{self, PageCtx, SelectEntity};
use crate::viewmodel::SettingId;

thread_local! {
    /// 当前打开的面板弱句柄（**不**延长生命周期；面板关闭/释放后自动失效）。
    ///
    /// 面板是 `window.open_dialog` 里的实体，外部没有持有者——而「应用到系统 /
    /// 从系统清除」是**后台线程**干的活（winreg / PowerShell 会阻塞数秒），它完成
    /// 后无法触碰 gpui 实体。故在此登记一份弱引用，由主线程泵消费
    /// `VmSignal::SettingApplied` 时调用 [`reload_open_panel`] 重读系统实况。
    ///
    /// 缺这一步的症状（实测）：点「从系统清除」后注册表已清干净，面板状态行却仍是
    /// 「已注册 N 项」——直到用户去点别的东西才刷新。
    static OPEN_PANEL: std::cell::RefCell<Option<WeakEntity<SettingsPanel>>> =
        const { std::cell::RefCell::new(None) };
}

/// 重读已打开面板的活读投影（系统实况重新查询）；未打开/已释放 → no-op。
///
/// 只允许在主线程（gpui 亲和）调用——调用点为主线程泵。
pub fn reload_open_panel(cx: &mut App) {
    OPEN_PANEL.with(|slot| {
        let weak = slot.borrow().clone();
        if let Some(weak) = weak {
            let _ = weak.update(cx, |panel, cx| panel.reload(cx));
        }
    });
}

/// 设置面板（Modal 容器 + Tabs 状态机；`tab` 选中态内持不外发）。
pub struct SettingsPanel {
    /// 当前分类页
    tab: SettingsTab,
    /// 活读投影（每次事件后由 [`SettingsPanel::reload`] 重建）
    view: SettingsView,
    /// AppData 句柄（活读数据面）
    app: AppDataHandle,
    /// 文件关联候选扩展名（打开期 `supported_open_extensions` 现算，面板生命周期内不变）
    candidates: Vec<String>,
    /// 事件上行（UI → AppData 五步流程）
    on: Rc<dyn Fn(DialogEvent)>,
    /// Select 实体（窗口亲和，构建期装配；与 SettingId 配对）
    selects: Vec<(SettingId, SelectEntity)>,
    /// 系统字体族列表（打开期枚举一次）：下标 0 = 空串 = 系统默认，
    /// 其余 = 字体名（排序）。`WidgetKind::FontSelect` 行的候选项与服务端
    /// 下标→值映射共用它（候选项是构建期快照，故留一份在面板上）
    fonts: Vec<String>,
    /// ColorPicker 实体（窗口亲和，构建期装配；T-SEL 走查 #3——
    /// 与 SettingId 配对，值经 render 帧幂等回写同步）
    pickers: Vec<(
        SettingId,
        Entity<gpui_kit::component::color_picker::ColorPickerState>,
    )>,
    /// Select 候选项最近一次装配时的 locale（与当前不同 → 需换文案）
    built_locale: String,
    /// 系统实况缓存（打开期查一次；仅「上下文菜单」页停留时交互重查——
    /// T-PERF 走查 #2：见 [`read_view`] 文档）
    ctx_status: Option<crate::app::state::CtxMenuStatus>,
}

/// 活读投影：`Config::load()` 在锁外读一次（不把磁盘 I/O 放进 AppData 锁内）。
///
/// `ctx_status` 由调用方决定：打开期与「上下文菜单」页停留时现查（注册表/
/// 文件系统读取，macOS 还会起 pluginkit 子进程——不得进入 AppData 锁内），
/// 其余页交互复用面板缓存的最近一次结果（T-PERF 走查 #2：下拉确认触发
/// 双重复活，每次都撞注册表 I/O 且恰落在弹层收起动画窗口内 → 掉帧；
/// 状态行只在「上下文菜单」页被消费，离开该页的交互无需刷新它）。
fn read_view(
    app: &AppDataHandle,
    candidates: &[String],
    ctx_status: Option<crate::app::state::CtxMenuStatus>,
    assoc_states: Vec<(String, hzip_platform::assoc::ExtAssocState)>,
) -> SettingsView {
    let cfg = hzip_config::Config::load();
    match app.lock() {
        Ok(data) => SettingsView::load(&data, &cfg, candidates.to_vec(), ctx_status, assoc_states),
        // 锁中毒（他处 panic）：退回出厂默认，不 panic 二次
        Err(_) => SettingsView::defaults(candidates.to_vec()),
    }
}

/// 已勾选格式的双击实况（关联页状态行）。
///
/// **在 AppData 锁外**查（注册表 I/O，不进渲染帧、不占锁——同 `ctx_status`
/// 的取舍）。
///
/// 扩展名集合取 **AppData 镜像**（`settings.assoc`）而非磁盘配置：镜像与勾选框
/// 同源（同一次 `apply_setting` 同时写二者，镜像还更早生效），这样才能保证状态行
/// 与用户眼前的勾选**逐项一致**。曾按磁盘 `[ui].assoc_formats` 取值，磁盘落后一步
/// 时状态行会列出「上一组」格式（用户报障 2026-09-29：「我勾了 zip，下面却说已由
/// hzip 打开 .tar/.tgz/…」）。
fn assoc_states_now(exts: &[String]) -> Vec<(String, hzip_platform::assoc::ExtAssocState)> {
    // 走缓存（审计 B7/B10）：本函数在面板装配与 `reload`（每次设置生效/切语言）
    // 里被调用，而每次真查都是 N 次注册表读（Windows）或子进程（macOS）。
    if let Some(hit) = crate::ui::contracts::cached_assoc_ext_states(exts) {
        return hit;
    }
    let states = crate::ui::contracts::assoc_ext_states(exts);
    crate::ui::contracts::warm_assoc_status_cache(exts.to_vec());
    states
}

/// AppData 镜像里的文件关联勾选集（状态行的扩展名来源）。
fn assoc_exts_from_app(app: &AppDataHandle) -> Vec<String> {
    app.lock()
        .map(|d| d.settings.assoc.clone())
        .unwrap_or_default()
}

/// 状态行的系统实况来源（走查 #13）。
///
/// 后台安装/清除**失败**时 `run_context_menu` 会把 `Degraded { reason }`
/// 写进 `AppData.ctx_menu_status`——此处优先消费它：失败原因必须直达
/// 状态行（否则用户面对的是「点了应用、状态纹丝不动地停在『尚未注册』」，
/// 而原因只在日志里）。其余情况（成功/未装/需重启/无记录）以系统实况为准；
/// 下一次成功的应用/清除会覆盖该字段，失败态不会陈旧滞留。
fn effective_ctx_status(app: &AppDataHandle) -> crate::app::state::CtxMenuStatus {
    use crate::app::state::CtxMenuStatus;
    let stored = app.lock().ok().and_then(|d| d.ctx_menu_status.clone());
    match stored {
        Some(degraded @ CtxMenuStatus::Degraded { .. }) => degraded,
        // 命中缓存即返回（审计 B7/B10：`reload` 每次都调本函数，不该每次都问系统）；
        // 未命中才真查，并顺手后台预热（下一次 `reload` 命中）
        _ => match crate::ui::contracts::cached_ctx_menu_status() {
            Some(hit) => hit,
            None => {
                crate::ui::contracts::warm_assoc_status_cache(assoc_exts_from_app(app));
                crate::ui::contracts::query_ctx_menu_status()
            }
        },
    }
}

impl SettingsPanel {
    /// 构建面板实体（需 window——SelectState 装配 + 订阅；由 shell 装配设置页签时调用）。
    ///
    /// * `app`：AppData 句柄（活读）。
    /// * `candidates`：文件关联候选扩展名（打开期现算）。
    /// * `on`：事件上行（UI → AppData；即时保存语义）。
    pub fn new_entity(
        window: &mut Window,
        cx: &mut App,
        app: AppDataHandle,
        candidates: Vec<String>,
        on: Rc<dyn Fn(DialogEvent)>,
    ) -> Entity<Self> {
        let ctx_status = Some(effective_ctx_status(&app));
        let assoc_states = assoc_states_now(&assoc_exts_from_app(&app));
        let view = read_view(&app, &candidates, ctx_status.clone(), assoc_states);
        let fonts = system_fonts(cx);
        let entity = cx.new(|cx| {
            let selects = build_selects(window, cx, &view, &on, &fonts);
            let pickers = build_pickers(window, cx, &view, &on);
            Self {
                tab: SettingsTab::default(),
                view,
                app,
                candidates,
                on,
                selects,
                fonts,
                pickers,
                built_locale: crate::current_locale(),
                ctx_status,
            }
        });
        // 登记弱句柄：后台效果完成后由主线程泵触发 reload（见 OPEN_PANEL 说明）
        OPEN_PANEL.with(|slot| *slot.borrow_mut() = Some(entity.downgrade()));
        entity
    }

    /// 事件后刷新：重读活读投影 + 重渲（修掉「面板显示陈旧值 / 控件回弹」）。
    ///
    /// 行文案（label/desc/按钮）每帧经 `crate::tr` 求值，切语言后自动跟随；
    /// 但 **Select 的候选项是构建期快照**（`SelectState` 持 `Vec<String>`），
    /// 语言变了必须换掉候选项，否则下拉仍是旧语言直到面板重开——该换文动作
    /// 在 [`Self::render`] 帧内完成（帧里有 `&mut Window`）。
    fn reload(&mut self, cx: &mut Context<Self>) {
        // 仅「上下文菜单」页停留时重查系统实况（状态行只在那一页被消费；
        // 其余页交互复用缓存——注册表/文件系统 I/O 不再撞弹层收起动画）
        if self.tab == SettingsTab::ContextMenu {
            self.ctx_status = Some(effective_ctx_status(&self.app));
        }
        // 「文件关联」页同理：双击实况只在那一页的状态行被消费
        let assoc_states = if self.tab == SettingsTab::Assoc {
            assoc_states_now(&assoc_exts_from_app(&self.app))
        } else {
            Vec::new()
        };
        let ctx_status = self.ctx_status.clone();
        self.view = read_view(&self.app, &self.candidates, ctx_status, assoc_states);
        cx.notify();
    }

    /// locale 变化 → 就地替换各 Select 的候选项文案。
    ///
    /// 用 [`SelectState::set_items`] 换 delegate 数据而非重建实体：实体身份不变，
    /// 装配期订阅与选中态都保留（重建会丢订阅，且需要重新 `subscribe`）。
    fn sync_select_locale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let locale = crate::current_locale();
        if locale == self.built_locale {
            return;
        }
        for (id, entity) in &self.selects {
            let def = settings_registry::def(*id);
            let items: SearchableVec<String> = match def.widget {
                WidgetKind::Select { label_keys } => {
                    SearchableVec::new(label_keys.iter().map(|k| crate::tr(k)).collect::<Vec<_>>())
                }
                // 字体下拉的候选是**运行期枚举**的系统字体（只有首项「系统默认」
                // 随语言变化；字体名本身不翻译——它是契约值）
                WidgetKind::FontSelect => font_select_items(&self.fonts),
                _ => continue,
            };
            entity.update(cx, |state, cx| state.set_items(items, window, cx));
        }
        self.built_locale = locale;
    }
}

/// Select 白名单订阅装配（确认 → `SettingChanged { id, Index(ix) }` 上行 + 面板刷新）。
///
/// `SelectEvent::Confirm` 载荷是选中**文案**（非下标）→ 按 label 序列反查下标
/// （下标即白名单 value 下标，与落盘同源）。
fn wire_select(
    cx: &mut Context<SettingsPanel>,
    entity: &SelectEntity,
    id: SettingId,
    on: Rc<dyn Fn(DialogEvent)>,
) {
    cx.subscribe(
        entity,
        move |this, e, ev: &SelectEvent<SearchableVec<String>>, cx| {
            if matches!(ev, SelectEvent::Confirm(Some(_))) {
                // **按下标匹配**：`Confirm` 的载荷是选中**文案**，而文案随语言
                // 变化——用文案反查下标会在切语言后错位（`SelectState` 的候选项
                // 是构建期快照，可能与当前语言的 key 序列不一致）。
                // 选中下标 (`IndexPath.row`) 与语言无关，是稳定身份。
                let ix = e.read(cx).selected_index(cx).map(|p| p.row);
                if let Some(ix) = ix {
                    on(DialogEvent::SettingChanged {
                        id,
                        value: SettingValue::Index(ix),
                    });
                    this.reload(cx);
                }
            }
        },
    )
    .detach();
}

/// 装配全部 Select 实体（构建期与「切语言重建」共用）。
fn build_selects(
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
    view: &SettingsView,
    on: &Rc<dyn Fn(DialogEvent)>,
    fonts: &[String],
) -> Vec<(SettingId, SelectEntity)> {
    let mut selects: Vec<(SettingId, SelectEntity)> = Vec::new();
    for def in settings_registry::REGISTRY {
        let id = def.id;
        let entity = match def.widget {
            WidgetKind::Select { label_keys } => {
                let entity = string_select(label_keys, view.index_of(id), window, cx);
                wire_select(cx, &entity, id, on.clone());
                entity
            }
            WidgetKind::FontSelect => {
                // 选中下标由**当前值**反查（值 = 字体名；未知/空 → 0 = 系统默认）
                let cur = view.text_of(id);
                let ix = fonts.iter().position(|f| f == cur).unwrap_or(0);
                let entity = item_select(font_select_items(fonts), ix, window, cx);
                wire_font_select(cx, &entity, id, Rc::new(fonts.to_vec()), on.clone());
                entity
            }
            _ => continue,
        };
        selects.push((id, entity));
    }
    selects
}

/// 系统字体族列表（下拉数据源）：下标 0 = 空串 = **系统默认**，其余按名排序。
///
/// 数据源 = [`gpui_kit::TextSystem::all_font_names`]（平台字体枚举；Windows 走
/// DirectWrite 全系统字体）。**只列已安装字体**：gpui 对不存在的族静默回落
/// 默认族，与其让用户手打一个不可能生效的名字，不如只给真正能用的。
///
/// 代价：枚举一次约毫秒级——只在**面板打开期**做一次（`new_entity`），
/// 不进入渲染帧（T-PERF3 纪律：渲染帧零系统调用）。
fn system_fonts(cx: &App) -> Vec<String> {
    let mut names = cx.text_system().all_font_names();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    let mut out = Vec::with_capacity(names.len() + 1);
    out.push(String::new());
    out.extend(names);
    out
}

/// 字体下拉候选项文案（首项 = 「系统默认」；其余是字体名原文）。
fn font_select_items(fonts: &[String]) -> SearchableVec<String> {
    SearchableVec::new(
        fonts
            .iter()
            .enumerate()
            .map(|(ix, name)| {
                if ix == 0 {
                    crate::tr("setting.text.system_default")
                } else {
                    name.clone()
                }
            })
            .collect::<Vec<_>>(),
    )
}

/// 字体下拉实体（候选项为运行期数据；与 `string_select` 同款，只差数据源）。
fn item_select(
    items: SearchableVec<String>,
    selected_ix: usize,
    window: &mut Window,
    cx: &mut App,
) -> SelectEntity {
    // **开搜索**（`.searchable(true)`）：系统字体动辄数百项，靠滚动找不现实
    // （用户要求）。搜索开关挂在 `SelectState` 上（渲染期读它开关下拉里的搜索
    // 框），故只能在这里设——`Select::new(entity).with_size(component_size())` 只是消费句柄。
    cx.new(|cx| {
        SelectState::new(items, Some(IndexPath::new(selected_ix)), window, cx).searchable(true)
    })
}

/// 字体下拉订阅（确认 → `SettingChanged { id, Text(字体名) }` 上行 + 面板刷新）。
///
/// 与 [`wire_select`] 的差别只在**取值**：白名单下拉上行下标（下标即白名单
/// value 下标），字体下拉上行**字体名本身**——候选集随机器变化，下标不是契约。
fn wire_font_select(
    cx: &mut Context<SettingsPanel>,
    entity: &SelectEntity,
    id: SettingId,
    fonts: Rc<Vec<String>>,
    on: Rc<dyn Fn(DialogEvent)>,
) {
    cx.subscribe(
        entity,
        move |this, e, ev: &SelectEvent<SearchableVec<String>>, cx| {
            if !matches!(ev, SelectEvent::Confirm(Some(_))) {
                return;
            }
            let ix = e.read(cx).selected_index(cx).map(|p| p.row);
            let Some(name) = ix.and_then(|ix| fonts.get(ix)) else {
                return;
            };
            on(DialogEvent::SettingChanged {
                id,
                value: SettingValue::Text(name.clone()),
            });
            this.reload(cx);
        },
    )
    .detach();
}

/// ColorPicker 实体装配（窗口亲和；T-SEL 走查 #3——遍历注册表 `Color` 行）。
fn build_pickers(
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
    view: &SettingsView,
    on: &Rc<dyn Fn(DialogEvent)>,
) -> Vec<(
    SettingId,
    Entity<gpui_kit::component::color_picker::ColorPickerState>,
)> {
    use gpui_kit::component::color_picker::ColorPickerState;
    let mut pickers = Vec::new();
    for def in settings_registry::REGISTRY {
        if !matches!(def.widget, WidgetKind::Color) {
            continue; // 非 Color 行不装配
        }
        let id = def.id;
        let hex = view.text_of(id).trim().to_string();
        let entity = cx.new(|cx| {
            let mut state = ColorPickerState::new(window, cx);
            if let Some(color) = crate::ui::theme::parse_theme_hex(&hex) {
                state = state.default_value(color);
            }
            state
        });
        wire_picker(cx, &entity, id, on.clone());
        pickers.push((id, entity));
    }
    pickers
}

/// picker 确认上行（`ColorPickerEvent::Change` → `SettingChanged` Text(hex)；
/// 清空 → 空串 = 主题默认色）。
fn wire_picker(
    cx: &mut Context<SettingsPanel>,
    entity: &Entity<gpui_kit::component::color_picker::ColorPickerState>,
    id: SettingId,
    on: Rc<dyn Fn(DialogEvent)>,
) {
    use gpui_kit::component::color_picker::ColorPickerEvent;
    cx.subscribe(entity, move |this, _e, ev: &ColorPickerEvent, cx| {
        // Change 是 ColorPickerEvent 唯一变体（irrefutable 由 matches 收口）
        if matches!(ev, ColorPickerEvent::Change(_)) {
            let hex = match ev {
                ColorPickerEvent::Change(Some(c)) => crate::ui::theme::theme_hex_of(*c),
                _ => String::new(),
            };
            on(DialogEvent::SettingChanged {
                id,
                value: SettingValue::Text(hex),
            });
            // 行内控件标准模式（on + refresh）：面板重读活读投影
            //（描述行/回写同步下一帧即正确）
            this.reload(cx);
        }
    })
    .detach();
}

/// picker 值幂等回写（view 文本 ↔ 实体 hex 不一致才 set_value；不 emit）。
fn _sync_pickers_placeholder() {}

impl SettingsPanel {
    /// render 帧同步 picker 值（store/reload/重置后 UI 不回弹）。
    fn sync_pickers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 幂等基准 = **实体当前值**（T-SEL 后走查 #7：曾用 last 追踪——
        // 选色上行不更新追踪位，恢复默认后「旧色实体 vs 空配置」被误判
        // 无变化，picker 永不回写=「恢复默认没生效」的观感根源）
        let hex = self.view.text_of(SettingId::ThemeColor).trim().to_string();
        let want = crate::ui::theme::parse_theme_hex(&hex);
        for (_, entity) in &self.pickers {
            let cur = entity.read(cx).value();
            let cur_hex = cur.map(crate::ui::theme::theme_hex_of);
            let want_hex = want.map(crate::ui::theme::theme_hex_of);
            if cur_hex == want_hex {
                continue;
            }
            entity.update(cx, |state, cx| match want {
                Some(color) => state.set_value(color, window, cx),
                None => state.clear_value(window, cx),
            });
        }
    }
}

/// 白名单 label **key** 序列 + 当前下标 → Select 实体（窗口亲和；dialogs.rs 同款模式）。
///
/// key 在此处（构建期）本地化：`SelectState` 只认展示文案。语言变化后由
/// [`SettingsPanel::reload`] 重建整批实体。
fn string_select(
    label_keys: &'static [&'static str],
    selected_ix: usize,
    window: &mut Window,
    cx: &mut App,
) -> SelectEntity {
    let vec: SearchableVec<String> =
        SearchableVec::new(label_keys.iter().map(|k| crate::tr(k)).collect::<Vec<_>>());
    cx.new(|cx| SelectState::new(vec, Some(IndexPath::new(selected_ix)), window, cx))
}

impl gpui_kit::Render for SettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 语言变化 → 先换下拉候选项文案，本帧即用新文案渲染
        self.sync_select_locale(window, cx);
        // picker 值回写（T-SEL 走查 #3：store 落库 → reload 重读 → 此处把
        // view 值幂等写回实体——恢复默认/外部变更后 UI 不回弹）
        self.sync_pickers(window, cx);
        let tab = self.tab;
        // 刷新手柄用 Weak（强句柄会被 Select 订阅/元素回调长期持有 → 面板泄漏）
        let this = cx.weak_entity();
        let ctx = PageCtx {
            on: self.on.clone(),
            refresh: Rc::new(move |cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.reload(cx));
            }),
        };
        let content =
            settings_render::page(tab, &self.view, &self.selects, &self.pickers, &ctx, cx);
        let m = crate::ui::spacing::metrics();
        let selected_bg = crate::ui::theme::selection_row_bg(cx);

        // 竖排页签轨道（用户要求「Tabs 样式改为垂直 tab」）：七个分类一列排开，
        // 选中项用与目录树/文件列表**同一来源**的选中底色（`selection_row_bg`）。
        let mut rail = v_flex()
            .w(crate::ui::spacing::settings_tab_rail_w())
            .flex_none()
            .gap(m.space(0.5))
            // 边界锚点（回归用：页签栏整条高度随密度）
            .debug_selector(|| "settings-tabs-wrap".into());
        for (ix, key) in SettingsTab::LABEL_KEYS.iter().enumerate() {
            let is_cur = ix == tab.index();
            let mut item = h_flex()
                .id(("settings-tab", ix))
                .w_full()
                .items_center()
                .gap(m.gap_sm())
                .px(m.pad_cell())
                .py(m.space(1.5))
                .rounded(cx.theme().radius)
                .cursor_pointer()
                .when(is_cur, |d| d.bg(selected_bg))
                .when(!is_cur, |d| d.hover(|h| h.bg(cx.theme().secondary)))
                // 选中项左侧一道主题色竖条：只靠底色时深浅主题下对比太弱
                // （浅色主题里 secondary 与背景几乎同色）
                .when(is_cur, |d| {
                    d.child(
                        div()
                            .w(TAB_RAIL_MARK_W)
                            .h(TAB_RAIL_MARK_H)
                            .rounded_full()
                            .bg(cx.theme().primary),
                    )
                })
                .child(div().text_sm().child(crate::tr(key)));
            if cfg!(test) {
                let sel = format!("settings-tab-{ix}");
                item = item.debug_selector(move || sel.clone());
            }
            rail = rail.child(item.on_click(cx.listener(move |this, _, _, cx| {
                this.tab = SettingsTab::from_index(ix);
                cx.notify();
            })));
        }

        h_flex()
            .w_full()
            // 撑满正文视口（用户四轮：中间内容区占满除顶部/底部的全部空间，
            // 左侧页签的竖线要撑满整个内容区）——`min_h_full` 让面板至少等于
            // 滚动视口高，分隔线的 `self_stretch()` 随之顶到上下沿。
            .min_h_full()
            .items_start()
            .gap(m.gap_lg())
            // 测试锚点（`cfg!(test)` 下可查边界；发布构建无开销）：面板根
            // 布局盒——「对话框内控件点不动」这类布局/命中区缺陷靠它定位。
            .debug_selector(|| "settings-panel-root".into())
            .child(rail)
            // 用户要求：竖排页签与右侧内容之间用**一道竖线**分隔。
            // `self_stretch()`：面板是内容高（无 `h_full`），分隔线要跟着内容
            // 列一起长到面板满高，否则只有 1px 高、看不见。
            .child(
                div()
                    .w(HAIRLINE)
                    .self_stretch()
                    .flex_none()
                    .when(cfg!(test), |d| {
                        d.debug_selector(|| "settings-tab-divider".into())
                    })
                    .bg(cx.theme().border),
            )
            // 内容列收窄（用户要求「各个设置页的宽度可以稍微调窄一点点」）：
            // 弹窗总宽已调窄 + 竖排页签占一列 ⇒ 页内容比原先窄一截。
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    // 边界锚点（回归用：分组内边距用「相对内容列左边」的差值量，
                    // 绝对 x 会随页签轨道宽度/弹窗位置一起漂）
                    .when(cfg!(test), |d| {
                        d.debug_selector(|| "settings-content".into())
                    })
                    // 内容列的底部留白（原本挂在滚动内容上）
                    .pb(sp(3.))
                    .child(content),
            )
    }
}

#[cfg(test)]
#[path = "tests/settings_layout.rs"]
mod tests;
