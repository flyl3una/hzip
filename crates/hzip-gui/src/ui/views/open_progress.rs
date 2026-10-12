//! 「正在打开档案」进度窗（R-GUI2-06：打开大档案时给用户一个「界面没死」的证据）。
//!
//! # 形态
//!
//! 与 [`super::progress`]（压缩/解压进度窗）同族的**独立 OS 窗口**，但刻意更小更静：
//! 打开不是任务（任务中心没有条目、也没有「取消」语义——中止一次 browse 没有意义），
//! 用户要的只是三样东西：还在转的证据、打开的**是哪个档案**、**转了多久**。故窗口
//! 只有一行内容 + 一行状态 + 一个关闭按钮，约 180 px 高、不可缩放。
//!
//! ```text
//! ⟳  大档案.zip                      ← Spinner（打开结束即停）+ 档案展示名
//!    正在打开… · 1.2 s                ← 状态行（末尾附已耗时；语义色）
//!                          [ 关闭 ]   ← 与 ✕/Esc 同义
//! ```
//!
//! # 生命周期（调用方在 `shell::open_archive_task` 接线）
//!
//! 1. 打开耗时超过 `[jobs].open_show_delay_ms` 才 [`open`]——小档案秒开就不闪窗；
//! 2. 打开结束（成功 / 失败）→ [`finish`]：状态行转「已打开」/ 失败原因转错误色，
//!    并排定 `close_at = now + [jobs].open_close_delay_ms`（默认 100 ms）——停留
//!    一下让用户看见结果再自动关；
//! 3. 到点关窗由主线程泵**每 tick** 调 [`tick`] 执行（与 `progress::tick_pending_closes`
//!    同款：终局后不再有数据信号，必须靠兜底 tick 驱动；无排定项时零开销）；
//! 4. [`close_all`] 供退出 / 切档案时一次收口。
//!
//! **关窗 ≠ 中止打开**：✕ / Esc / 自动关都只关窗，后台 browse 照常跑完，结果照常
//! 落账并刷新主界面。
//!
//! # 已耗时怎么刷新（为什么是**有界**定时器）
//!
//! 状态行的耗时只能随渲染刷新，而渲染只在有事件时发生——没人驱动的话数字会停在开窗
//! 那一瞬（用户看到「正在打开 · 0.0 s」纹丝不动，反而更像卡死）。故 [`open`] 挂一个
//! **100 ms 的有界节拍**：每拍只做 `cx.notify()`，以下任一条件成立**立即退出**——
//! `done`（[`finish`] 已到）、窗口已被关（✕/Esc，句柄判活失败）、实体已释放
//! （注册表条目被清理）。既不做无限定时器，也不在没人看的时候空转刷屏。
//! Spinner 自己的动画帧只负责转圈，不承担「刷新耗时」的职责。
//!
//! # 数据面
//!
//! [`OpenProgressWindow`] 只有五个字段（档案名 / 起点 / 终点 / 成败），全部由
//! [`open`]/[`finish`] 写入；文案与颜色一律渲染侧派生（i18n、主题色不落数据面）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::component::button::Button;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Sizable, Size, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, Context, Entity, IntoElement, Render, SharedString, Styled, Window, div,
    size,
};

use crate::ui::constant::{
    CONTENT_MAX_W, OPEN_PROGRESS_WINDOW_H, OPEN_PROGRESS_WINDOW_MIN_H, OPEN_PROGRESS_WINDOW_MIN_W,
    OPEN_PROGRESS_WINDOW_W,
};
use crate::ui::spacing::{component_size, sp};

/// 重绘节拍（「已耗时」刷新粒度；10 Hz 与主线程泵同频，肉眼连续）。
const REPAINT_TICK: Duration = Duration::from_millis(100);

/// 长名零宽断点的插入间隔（字符数）。24 个字符一段在 180~420 px 宽的窗口里
/// 都能折得整齐，又不会把短名切碎。
const BREAK_EVERY: usize = 24;

/// 给档案名插入**零宽空格**断点（每 [`BREAK_EVERY`] 个字符一个）。
///
/// 为什么需要：gpui 的文本布局只在合法断点折行，而档案名常是无空格的一整串
/// （`aaaaaaaa….cpio.gz`）——那种名字即使开了 `whitespace_normal` 也会整行溢出
/// 窗口。零宽空格（U+200B）提供断点、**不显示、不改变字符本身**，故「名称完整
/// 显示」这一诉求与「窗口不被撑破」不再互斥。
fn breakable(name: &str, every: usize) -> String {
    let every = every.max(1);
    let mut out = String::with_capacity(name.len() + name.len() / every + 1);
    for (i, ch) in name.chars().enumerate() {
        if i > 0 && i % every == 0 {
            out.push('\u{200b}');
        }
        out.push(ch);
    }
    out
}

/// 窗口键自增源（每开一窗取一值；同 label 复用已开窗口时**不**取新值）。
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// 「进行中操作」进度窗的**文案参数**（打开 / 复制 / 外部打开 / … 共用同一窗口
/// 形态；用户口径 2026-09-24：耗时操作都要有进度窗，动词随操作而变）。
///
/// `record_kind` 是任务中心留痕用的任务种类（`job_kind_label` 查表）。
#[derive(Clone)]
pub struct OpLabels {
    /// 窗口标题栏文案 key（如 `openprogress.title`）
    pub title_key: &'static str,
    /// 进行中文案 key（如 `openprogress.opening`）
    pub running_key: &'static str,
    /// 完成文案 key（如 `openprogress.done`）
    pub done_key: &'static str,
    /// 任务中心记录的种类（`open` / `copy` / …）
    pub record_kind: &'static str,
}

impl Default for OpLabels {
    fn default() -> Self {
        Self {
            title_key: "openprogress.title",
            running_key: "openprogress.opening",
            done_key: "openprogress.done",
            record_kind: "open",
        }
    }
}

/// 「正在打开」窗的过程明细（工作者线程写 → 渲染侧每 [`REPAINT_TICK`] 读）。
///
/// 打开档案/进入容器此前只有「转圈 + 已耗时」——用户口径 2026-09-24：这类操作要
/// 「增强可观测性」，后台到底在干什么得看得见。故加一条明细线，承载**阶段文案**
/// （解出内层档案 / 索引内层档案 / 展开内层容器）+ **已处理字节**（总量已知时
/// 显示 `已处理 x / y`）。
///
/// 线程安全：工作者线程 `set_*`、UI 线程读，各字段独立（原子 / 短锁）——读到上一拍
/// 的值无妨，[`REPAINT_TICK`] 后还有下一拍。仅 `Send + Sync` 数据，不含 gpui 句柄，
/// 故可自由跨线程传递。
#[derive(Clone, Default)]
pub struct OpenDetail(std::sync::Arc<DetailInner>);

#[derive(Default)]
struct DetailInner {
    /// 阶段文言（已 `tr` 的人读串；`None` = 无阶段，不占行）。
    phase: std::sync::Mutex<Option<String>>,
    /// 已处理字节。
    done: AtomicU64,
    /// 已知总量（0 = 未知，只报已处理量）。
    total: AtomicU64,
}

impl OpenDetail {
    /// 记阶段（工作者线程调；文案取 [`crate::tr`]，调用处给最终串）。
    pub fn set_phase(&self, phase: String) {
        if let Ok(mut slot) = self.0.phase.lock() {
            *slot = Some(phase);
        }
    }

    /// 记总量（未知传 0）。
    pub fn set_total(&self, total: u64) {
        self.0.total.store(total, Ordering::Relaxed);
    }

    /// 累加已处理字节（每读到一块调一次：调用点在读循环里，必须是原子自增）。
    pub fn add_done(&self, n: u64) {
        self.0.done.fetch_add(n, Ordering::Relaxed);
    }

    /// 当前阶段（副本）。
    pub fn phase(&self) -> Option<String> {
        self.0.phase.lock().ok().and_then(|p| p.clone())
    }

    /// 已处理字节。
    pub fn done(&self) -> u64 {
        self.0.done.load(Ordering::Relaxed)
    }

    /// 已知总量（0 = 未知）。
    pub fn total(&self) -> u64 {
        self.0.total.load(Ordering::Relaxed)
    }

    /// **明细行文案**（`None` = 什么都没有，渲染侧不占行）。
    ///
    /// 纯派生（含 i18n 查表）：数据面只存阶段串与字节数，句子怎么拼在渲染侧决定。
    pub fn line(&self) -> Option<String> {
        match (self.phase(), self.done()) {
            (None, 0) => None,
            (p, 0) => p,
            (p, done) => {
                let bytes = if self.total() > 0 {
                    crate::tr_with(
                        "openprogress.detail_known",
                        &[
                            ("done", &crate::ui::binding::fmt_bytes(done)),
                            ("total", &crate::ui::binding::fmt_bytes(self.total())),
                        ],
                    )
                } else {
                    crate::tr_with(
                        "openprogress.detail_unknown",
                        &[("done", &crate::ui::binding::fmt_bytes(done))],
                    )
                };
                Some(match p {
                    Some(phase) => format!("{phase} · {bytes}"),
                    None => bytes,
                })
            }
        }
    }
}

/// 计数读包装（`OpenDetail::add_done` 的数据来源）。
///
/// 包在**已经拿到的条目流**外侧，不改 reader 语义：`Read::read` 原样转发，只把
/// 返回的字节数累加进明细。故「解出内层档案」这类**只有流、没有回调**的长操作
/// 也能报进度。
pub struct CountingReader<R> {
    inner: R,
    detail: OpenDetail,
}

impl<R> CountingReader<R> {
    /// 包装（明细随读推进）。
    pub fn new(inner: R, detail: OpenDetail) -> Self {
        Self { inner, detail }
    }
}

impl<R: std::io::Read> std::io::Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.detail.add_done(n as u64);
        }
        Ok(n)
    }
}

/// 终局后是否**排定自动关窗**（纯判定；与 `progress::is_close_on_terminal` 同款）。
///
/// 成功 → 排（百毫秒后自动退场，无需用户处置）；**失败 → 不排**——窗口常驻，
/// 原因可见，由用户点「关闭」/ ✕ / Esc 收场（用户口径 2026-09-24：「这些弹窗
/// 如果在出现执行失败时都不要自动关闭，否则影响用户感知」）。
pub fn is_close_on_finish(failed: bool) -> bool {
    !failed
}

/// 「正在打开」窗口实体（数据面 = 档案展示名 + 起止时刻 + 成败）。
pub struct OpenProgressWindow {
    /// 档案展示名（开窗时静态；同 label 去重复用已开窗口）
    label: String,
    /// 计时起点（开窗时刻）
    started: Instant,
    /// 计时终点（[`finish`] 写入；`None` = 仍在打开）。写它让耗时在完成后**冻结**——
    /// 停留期内数字继续走会让人以为还没打开完
    finished_at: Option<Instant>,
    /// 打开是否已收尾（成功或失败）：渲染据此换文案，重绘节拍据此退出
    done: bool,
    /// 失败原因（`Some` = 失败，状态行转错误色并显示原因；`None` = 成功或进行中）
    error: Option<String>,
    /// 过程明细（阶段 + 已处理字节；工作者线程写、渲染读）
    detail: OpenDetail,
    /// 文案参数（标题/进行中/完成/任务种类；打开与复制共用一个窗体）
    labels: OpLabels,
}

impl OpenProgressWindow {
    /// 已耗时（秒，渲染侧现算；1 位小数由渲染格式化）。
    fn elapsed_secs(&self) -> f32 {
        let end = self.finished_at.unwrap_or_else(Instant::now);
        end.saturating_duration_since(self.started).as_secs_f32()
    }
}

/// Global 注册表（窗口键 → 窗口；同 label 去重 + 判活/关窗句柄 + 延迟关窗截止时刻）。
#[derive(Default)]
struct OpenProgressGlobal(HashMap<u64, OpenSlot>);

impl gpui_kit::Global for OpenProgressGlobal {}

/// 单窗口槽：展示名（去重判据）+ 实体（写入成败）+ 句柄（判活/关窗）+ 关窗截止时刻。
#[derive(Clone)]
struct OpenSlot {
    /// 档案展示名（`open` 的同 label 去重按它比对）
    label: String,
    /// 窗口内容实体（[`finish`] 往这里写终局态）
    entity: Entity<OpenProgressWindow>,
    /// 窗口句柄（判活 + 聚焦 + 关窗；根视图是 `component::Root`，故为类型擦除句柄）
    handle: AnyWindowHandle,
    /// 自动关窗截止时刻（[`finish`] 首次排定；`None` = 未排定）。到点由 [`tick`]
    /// 关窗——打开结束后不再有数据信号，必须靠泵的兜底 tick 驱动。
    close_at: Option<Instant>,
}

/// 开「正在打开」窗口；返回窗口键。
///
/// 同一 `label` 已开且存活 → **只聚焦**已开窗口并返回同一 key（不重复开——同一个
/// 档案被连点两次不该出现两个窗）；`label` 取档案展示名（文件名）。
///
/// 开窗失败**不**拖垮打开本身：日志留痕、注册表不登记（对未知 key 的 [`finish`]
/// 与 [`tick`] 均为 no-op），后台 browse 照常完成。
pub fn open(cx: &mut App, label: String, detail: OpenDetail, labels: OpLabels) -> u64 {
    // 判活：窗口已被用户关掉的条目先清掉（注册表不随打开次数无界增长）
    for (key, _, handle, _) in snapshot(cx) {
        if !is_alive(cx, handle) {
            remove_entry(cx, key);
        }
    }
    // 同 label 已开且存活 → 仅聚焦（把已开的窗口提到前台，返回它的 key）
    if let Some((key, _, handle, _)) = snapshot(cx)
        .into_iter()
        .find(|(_, label0, _, _)| *label0 == label)
    {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        return key;
    }
    let entity = cx.new(|_| OpenProgressWindow {
        label: label.clone(),
        started: Instant::now(),
        finished_at: None,
        done: false,
        error: None,
        detail: detail.clone(),
        labels: labels.clone(),
    });
    let options = gpui_kit::WindowOptions {
        window_bounds: Some(gpui_kit::WindowBounds::Windowed(
            gpui_kit::Bounds::centered(
                None,
                size(OPEN_PROGRESS_WINDOW_W, OPEN_PROGRESS_WINDOW_H),
                cx,
            ),
        )),
        window_min_size: Some(size(OPEN_PROGRESS_WINDOW_MIN_W, OPEN_PROGRESS_WINDOW_MIN_H)),
        // 可缩放（用户口径 2026-09-24：长档案名要能看全 → 窗口自己拉大）。
        // 最小尺寸仍由 `window_min_size` 守（拉不到看不出内容的大小）。
        is_resizable: true,
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some(SharedString::from(crate::tr(labels.title_key))),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        // 基座取 kit 约定（与主窗口/参数窗同款），只覆盖上面这几个字段
        ..gpui_kit::component::TitleBar::window_options()
    };
    let view = entity.clone();
    let opened = cx.open_window(options, move |window, cx| {
        // 根视图包 `component::Root`（kit 约定；本窗无对话框/悬浮层需求，Root 只托底）
        let any: gpui_kit::AnyView = view.into();
        cx.new(|cx| gpui_kit::component::Root::new(any, window, cx))
    });
    let handle: AnyWindowHandle = match opened {
        Ok(handle) => handle.into(),
        Err(e) => {
            tracing::error!("「正在打开」窗口创建失败: {e}");
            return NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        }
    };
    // 新开窗口必须**主动激活**：`--open` 由资源管理器拉起时新窗口默认落后台
    // （用户看不到，以为「双击没反应」——与进度窗同款处理）。
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    cx.default_global::<OpenProgressGlobal>().0.insert(
        key,
        OpenSlot {
            label,
            entity: entity.clone(),
            handle,
            close_at: None,
        },
    );
    spawn_repaint(cx, &entity, handle);
    key
}

/// 标记打开结束（成功 `error = None` / 失败 `error = Some(原因)`）。
///
/// **成功**：窗口进入「停留展示」期——`[jobs].open_close_delay_ms`（默认 100 ms）
/// 后由 [`tick`] 关窗（成功无需用户处置，闪一下即走）。
///
/// **失败：不自动关窗**（用户口径 2026-09-24：「这些弹窗如果在出现执行失败时
/// 都不要自动关闭，否则影响用户感知」）。原因转错误色**常驻**，直到用户点
/// 「关闭」/ ✕ / Esc——此前失败与成功一样百毫秒后自动关，用户只来得及看到
/// 窗口一闪，连失败原因都读不到（任务中心也还没有记录，见 `jobs_vm::record_open`）。
/// 窗口已被用户关掉（未知 key 或判活失败）→ 静默返回，打开照常收尾。
pub fn finish(cx: &mut App, key: u64, error: Option<String>) {
    let Some(slot) = slot(cx, key) else {
        return; // 未知 key（含开窗失败）/ 条目已被清理：无事可做
    };
    if !is_alive(cx, slot.handle) {
        remove_entry(cx, key); // 用户已关窗 → 条目退场（不留孤儿）
        return;
    }
    let failed = error.is_some();
    slot.entity.update(cx, |w, cx| {
        w.done = true;
        w.finished_at = Some(Instant::now());
        w.error = error;
        cx.notify();
    });
    // 排定延迟关窗（幂等：已排定不重置——连调两次不会把截止时刻往后顺延）。
    // **失败不排**：窗口留在屏幕上等用户处置（见函数文档）。
    if is_close_on_finish(failed)
        && let Some(s) = cx.global_mut::<OpenProgressGlobal>().0.get_mut(&key)
        && s.close_at.is_none()
    {
        let delay = Duration::from_millis(hzip_config::Config::load().jobs.open_close_delay_ms);
        s.close_at = Some(Instant::now() + delay);
    }
}

/// 泵每 tick 调用（与 `progress::tick_pending_closes` 同款，含空闲 tick）：
/// 到点关窗 + 清理已关条目。
///
/// 与 [`finish`] 分离的原因：排定关窗时打开已经结束，不再有数据信号驱动本模块——
/// 到点执行必须挂在泵的兜底 tick 上。10 Hz 精度对百毫秒级停留足够；无活窗口时
/// 零开销（空快照遍历）。
pub fn tick(cx: &mut App) {
    if !cx.has_global::<OpenProgressGlobal>() {
        return;
    }
    let now = Instant::now();
    for (key, _, handle, close_at) in snapshot(cx) {
        // 判活失败 = 用户 ✕/Esc 关过窗 → 条目退场
        if !is_alive(cx, handle) {
            remove_entry(cx, key);
            continue;
        }
        if close_at.is_some_and(|at| now >= at) {
            close_slot(cx, key);
        }
    }
}

/// **丢弃**某个「进行中操作」窗（被顶替的流程静默收场：不复用 `finish` 的
/// 成败文案——那条路会把失败原因留在屏幕上，而被顶替不是失败）。
pub fn discard(cx: &mut App, key: u64) {
    close_slot(cx, key);
}

/// 按**展示名**关闭窗口（用户实测 2026-09-24：打开 A 后再打开 B，A 的
/// 「正在打开」窗还在屏幕上转 ⇒ 换会话时按旧目标名一次性关掉它的窗）。
///
/// 名字匹配用「去掉零宽断点后的相等」，与 [`open`] 的去重判据同源。
pub fn close_by_label(cx: &mut App, label: &str) {
    let target = label.to_string();
    let keys: Vec<u64> = snapshot(cx)
        .into_iter()
        .filter(|(_, l, _, _)| l.replace('\u{200b}', "") == target)
        .map(|(k, ..)| k)
        .collect();
    for key in keys {
        close_slot(cx, key);
    }
}

/// 关闭全部「正在打开」窗口（退出 / 切档案时一次收口）。
///
/// 只关窗与清表：调用时正在跑的打开任务不受影响（关窗从不等于中止）。
pub fn close_all(cx: &mut App) {
    if !cx.has_global::<OpenProgressGlobal>() {
        return;
    }
    for (key, ..) in snapshot(cx) {
        close_slot(cx, key);
    }
}

/// 注册表快照（`key, label, 句柄, 截止时刻`）。
///
/// 先整体取出来再逐个动 `cx`：判活/关窗都要 `&mut App`，不能边持注册表借用边改它
/// （`progress::live_slots` 同款两段式）。
fn snapshot(cx: &App) -> Vec<(u64, String, AnyWindowHandle, Option<Instant>)> {
    cx.try_global::<OpenProgressGlobal>()
        .map(|g| {
            g.0.iter()
                .map(|(key, slot)| (*key, slot.label.clone(), slot.handle, slot.close_at))
                .collect()
        })
        .unwrap_or_default()
}

/// 取槽（未登记时 `None`）。
fn slot(cx: &App, key: u64) -> Option<OpenSlot> {
    cx.try_global::<OpenProgressGlobal>()
        .and_then(|g| g.0.get(&key).cloned())
}

/// 窗口是否存活（用户点 ✕ / 按 Esc 关掉 → `Err`）。
///
/// 泛型化是为了一处定义两处用：主线程侧（`&mut App`）与重绘节拍的异步侧
/// （`&mut AsyncApp`）判活语义必须一致，写成两份就会漂移。
fn is_alive<C: gpui_kit::AppContext>(cx: &mut C, handle: AnyWindowHandle) -> bool {
    handle.update(cx, |_, _, _| {}).is_ok()
}

/// 移除条目（返回被移除的槽；窗口关不关由调用方决定）。
fn remove_entry(cx: &mut App, key: u64) -> Option<OpenSlot> {
    if !cx.has_global::<OpenProgressGlobal>() {
        return None;
    }
    cx.global_mut::<OpenProgressGlobal>().0.remove(&key)
}

/// 注册表收口：移除条目 + 关窗（窗口存活时）。
fn close_slot(cx: &mut App, key: u64) {
    let Some(slot) = remove_entry(cx, key) else {
        return;
    };
    let _ = slot
        .handle
        .update(cx, |_, window, _| window.remove_window());
}

/// 挂「已耗时」重绘节拍（**有界**，见模块文档末节）。
///
/// 退出条件（任一成立即停）：`done` 已置位 / 窗口已被关（`handle` 判活失败）/
/// 实体已释放（注册表条目被清理）。故不存在「没人看的窗口还在定时刷屏」。
fn spawn_repaint(cx: &mut App, entity: &Entity<OpenProgressWindow>, handle: AnyWindowHandle) {
    let weak = entity.downgrade();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(REPAINT_TICK).await;
            let keep_going = weak.update(cx, |w, cx| {
                if w.done {
                    return false;
                }
                cx.notify(); // 只有「还在打开」才需要刷新耗时
                true
            });
            if !matches!(keep_going, Ok(true)) || !is_alive(cx, handle) {
                break;
            }
        }
    })
    .detach();
}

impl Render for OpenProgressWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let done = self.done;
        let failed = self.error.is_some();
        // 状态行文案：失败原因 > 已打开 > 正在打开（i18n 与语义色都在渲染侧派生——
        // 切语言后重渲染即换文案，数据面不存给人看的字符串）
        let status = match &self.error {
            Some(reason) => crate::tr_with("openprogress.failed", &[("reason", reason.as_str())]),
            None if done => crate::tr(self.labels.done_key),
            None => crate::tr(self.labels.running_key),
        };
        let status_color = if failed {
            cx.theme().danger
        } else if done {
            cx.theme().success
        } else {
            cx.theme().muted_foreground
        };
        let label = self.label.clone();
        let elapsed = self.elapsed_secs();
        // 明细行（阶段 · 已处理字节）：只有**进行中**才显示——终局后阶段串已无意义，
        // 失败原因/成功状态才是用户要读的东西
        let detail = if done { None } else { self.detail.line() };

        v_flex()
            .id("op-root")
            .debug_selector(|| "op-root".into())
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // Esc = 仅关窗（后台打开继续；与进度窗同款）
            .on_key_down(|ev: &gpui_kit::KeyDownEvent, window, _| {
                if ev.keystroke.key == "escape" {
                    window.remove_window();
                }
            })
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .w_full()
                    // 内容列 max 720 居中（与进度窗同款软约束）；本窗**不**吃满高度——
                    // 窗口只有 180 px，内容按自然高由根节点垂直居中，底部不留大片空白
                    .max_w(CONTENT_MAX_W)
                    .p(sp(4.))
                    .gap(sp(2.))
                    // 名字折成多行后内容可能高于窗口：可纵向滚动，任何长度都读得到
                    .max_h_full()
                    .overflow_y_scrollbar()
                    .debug_selector(|| "op-content".into())
                    // Spinner + 档案展示名（长名**换行**显示，不省略——用户口径
                    // 2026-09-24：截断后认不出是哪个档案；窗口可缩放，行数随宽度走）
                    .child(
                        h_flex()
                            .w_full()
                            .flex_shrink_0()
                            .gap(sp(2.))
                            .items_center()
                            .child(
                                // 转圈只在**未完成**时出现：打开结束后窗口进入停留展示期，
                                // 还在转等于告诉用户「没打开完」；顺带让停留期彻底静止
                                // （不再有动画帧需求）。
                                if done {
                                    div().flex_shrink_0().into_any_element()
                                } else {
                                    Spinner::new().with_size(Size::Small).into_any_element()
                                },
                            )
                            .child(
                                div()
                                    .id("op-label")
                                    .debug_selector(|| "op-label".into())
                                    .flex_1()
                                    .min_w_0()
                                    .text_base()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    // 换行优先（用户口径 2026-09-24：截断后认不出是
                                    // 哪个档案）。gpui 只在合法断点折行，故长名先插
                                    // 零宽断点（`breakable`）——无空格长名也能折。
                                    .whitespace_normal()
                                    .child(SharedString::from(breakable(&label, BREAK_EVERY))),
                            ),
                    )
                    // 状态行：状态文案 · 已耗时（失败时整行转错误色，原因即在其内）
                    .child(
                        div()
                            .id("op-status")
                            .debug_selector(|| "op-status".into())
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(status_color)
                            .child(SharedString::from(format!("{status} · {elapsed:.1} s"))),
                    )
                    // 明细行（用户口径 2026-09-24「增强可观测性」）：后台在干什么、
                    // 处理到哪了。无明细时不占行（小档案秒开连窗都不会弹）
                    .children(detail.map(|line| {
                        div()
                            .id("op-detail")
                            .debug_selector(|| "op-detail".into())
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(line))
                    }))
                    // 关闭按钮（与 ✕/Esc 同义：只关窗，不中止打开）
                    .child(
                        h_flex()
                            .w_full()
                            .flex_shrink_0()
                            .justify_end()
                            // kit `Button` 的 id 只当 keyed state 键（不注册可观测元素）
                            // → 外包 debug_selector 容器供测试定位（progress 同款约束）
                            .child(
                                div()
                                    .id("op-close-cell")
                                    .debug_selector(|| "op-close".into())
                                    .child(
                                        // 标准尺寸（用户口径 2026-09-24）：与参数窗/
                                        // 进度窗的按钮同一档，不再单独压到 Small
                                        Button::new("op-close")
                                            .with_size(component_size())
                                            .label(crate::tr("dlg.close"))
                                            .on_click(|_, window, _| window.remove_window()),
                                    ),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
#[path = "tests/open_progress.rs"]
mod tests;
