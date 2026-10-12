//! 参数窗（右键「添加到压缩文件…」/「解压选项…」的目标窗口）。
//!
//! # 形态（与 7-Zip / Bandizip 一致的两段式）
//!
//! ```text
//! 右键「…」→ 参数窗（本模块）→ 点「开始」→ **关掉参数窗** → 输出（进度）窗
//! ```
//!
//! 参数窗是独立窗口，全程不出现主界面；主界面在跑也不牵进来（直接模式是独立
//! 进程，见 `runtime` 模块文档）。
//!
//! # 界面承载（2026-09 返工：窗口内容视图直渲面板）
//!
//! 界面 = [`ParamsWindow`]（窗口内容视图）直接渲染
//! `dialogs::compress_panel` / `extract_panel`——**窗口级三段式布局**：
//!
//! ```text
//! 头部（固定）  标题 · 对象摘要/统计
//! 正文（滚动）  基本 / 高级折叠 / 加密折叠 / 参数摘要卡
//! 底部（固定）  提示·错误 + [取消][开始]
//! ```
//!
//! 旧实现走 `open_dialog` + `Dialog` 弹层（与设置面板同款链路），但 `Dialog`
//! 弹层是内容高度驱动的（无 `max_h`）：高级/加密区一展开就长出窗口底部，
//! **正文滚不动、底部「开始」按钮点不到**。窗口级布局把滚动区钉在
//! `flex_1 + min_h_0` 上，窗口多小都能滚（同 `views::progress::ProgressWindow`）。
//!
//! [`Root`] 仍是窗口根：Select 下拉/native menu 等悬浮层，以及 sheet/dialog/
//! notification 三层弹层，都是 `component::init` 注册的 `WindowState` 插件在
//! `gpui_base::Root::render` 内自动挂载的（0.7.1 起；内容视图不必、也不得手工
//! 挂层）。参数界面**不是**对话框层内容——它是窗口内容视图直渲的面板。
//!
//! 状态变化（下拉选中、输入框改动、浏览选路径）必须让面板重渲染——面板是**本
//! 视图**画出来的，故每次状态落定 `notify()` 本视图实体（见 [`refresh`]）。
//!
//! # 为什么参数状态是 `Rc<RefCell<_>>` 而不是 AppData
//!
//! 参数窗是**一次性**的（提交即关）。状态留在窗口作用域内，既避免「上一次没提交完
//! 的参数泄漏到下一次」，也不必引入全局态；`AppData::compress/extract` 那两个 VM
//! 仍归主界面（设置面板/工具栏）使用。
//!
//! # 与后端的唯一契约（红线）
//!
//! 点「开始」只做一件事：把状态装配成 VM，交给
//! [`super::dispatch::submit_compress`] / [`super::dispatch::submit_extract`]——
//! 与主界面工具栏、右键快捷动词**同一份**提交代码，两种工作模式的效果因此不可能
//! 漂移。
//!
//! # 事件通道
//!
//! 控件回调（Select `Confirm`/`Input` `Change`/`Switch`/`Checkbox`/折叠头）拿不到
//! `cx`/`window`，无法直接改状态并重绘；故一律发 [`DialogEvent`] 到
//! [`ParamsWindow::ev_tx`]，由消费者任务落表单、重绘、提交——`on_pick` 之类的
//! 装配期闭包因此只需一条 `try_send`。

use crate::ui::constant::{
    MESSAGE_WINDOW_H, MESSAGE_WINDOW_MIN_H, MESSAGE_WINDOW_MIN_W, MESSAGE_WINDOW_W,
    PARAMS_WINDOW_H, PARAMS_WINDOW_MIN_H, PARAMS_WINDOW_MIN_W, PARAMS_WINDOW_W,
};
use crate::ui::spacing::{component_size, sp};
use gpui_kit::component::Sizable as _;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::IndexPath;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::SelectState;
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::component::{ActiveTheme as _, Icon, Root, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, SharedString, Window, div, px, size};

use crate::app::labels;
use crate::ui::contracts::DialogEvent;
use crate::ui::shell::{AppDataHandle, open_progress_window_with, submit_compress, submit_extract};
use crate::ui::widgets::icon::AppIcon;
use crate::viewmodel::dialog_vm::CompressDialogVm;

/// 参数窗种类（决定渲染哪个面板、点「开始」调哪个提交点）。
enum OpKind {
    /// 压缩：整批输入 → 一个档案（`%SELS` 语义）。
    Compress { inputs: Vec<PathBuf> },
    /// 解压：一批档案，参数共用（每档案一条任务）。
    Extract { archives: Vec<PathBuf> },
}

/// 参数窗状态共享句柄（`Rc<RefCell>`——单窗口单线程，无跨线程需求）。
type Shared = Rc<RefCell<OpState>>;

/// 参数窗的窗口内容视图：直接渲染参数面板（见模块文档「界面承载」）。
///
/// 面板由 `state` 现场投影（props 每次渲染重算——能力门控/派生文案/校验都跟着
/// 最新表单走），事件一律经 [`ParamsWindow::ev_tx`] 出到事件消费者。
struct ParamsWindow {
    state: Shared,
    /// 控件事件出口：面板内每个控件（输入/下拉/滑块/开关）的 `DialogEvent`
    /// 都从这里进消费者任务，再由消费者落表单 + 重绘 + 提交。
    ev_tx: smol::channel::Sender<DialogEvent>,
}

impl Render for ParamsWindow {
    // `_window`：层已改由 kit 插件自动挂载，本视图不再碰窗口（避免 unused）。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (panel, cancel_ev, esc_tx) = {
            let st = self.state.borrow();
            let tx = self.ev_tx.clone();
            let on = move |e: DialogEvent| {
                let _ = tx.try_send(e);
            };
            let (panel, cancel_ev) = match &st.kind {
                OpKind::Compress { .. } => (
                    crate::ui::dialogs::compress_panel(cx, &st.compress_props(), on)
                        .into_any_element(),
                    DialogEvent::CompressCancel,
                ),
                OpKind::Extract { .. } => (
                    crate::ui::dialogs::extract_panel(cx, &st.extract_props(), on)
                        .into_any_element(),
                    DialogEvent::ExtractCancel,
                ),
            };
            (panel, cancel_ev, self.ev_tx.clone())
        };
        // Esc = 取消（只发取消事件，关窗归事件消费者——保持单一关闭路径）
        //
        // 覆盖确认是**真 Dialog**（用户要求「已存在要弹窗确认」，不是面板里的
        // 一行提示）。对话框层不必（也不可能）在本视图手工挂：gpui-kit 0.7.1
        // 已把 `Root` 的手工挂层函数整族删除（sheet / dialog / notification 三层，
        // 整树 0 命中、无同名替代），改由 `component::init` 注册的 `WindowState`
        // 插件随 `gpui_base::Root::render` 自动挂载
        // （gpui-component-0.7.1/src/root.rs:21 注册、:483 挂三层）。
        // 本窗口根即 `Root::new(...)`（见 `open_params_window`），层自动出现。
        v_flex()
            .size_full()
            .on_key_down(move |ev: &gpui_kit::KeyDownEvent, _, _| {
                if ev.keystroke.key == "escape" {
                    let _ = esc_tx.try_send(cancel_ev.clone());
                }
            })
            .child(panel)
    }
}

/// 参数窗的控件实体（窗口亲和；建一次复用）。
///
/// 09 扩面：压缩侧新增文件名/保存到双输入、压缩方式/分卷/线程/加密方式四路
/// 下拉、级别滑块、自定义分卷、确认密码。
struct Controls {
    // —— 压缩 ——
    filename_input: Entity<InputState>,
    out_dir_input: Entity<InputState>,
    format_select: Entity<SelectState<SearchableVec<String>>>,
    method_select: Entity<SelectState<SearchableVec<String>>>,
    level_slider: Entity<SliderState>,
    split_select: Entity<SelectState<SearchableVec<String>>>,
    split_custom_input: Entity<InputState>,
    threads_select: Entity<SelectState<SearchableVec<String>>>,
    dict_select: Entity<SelectState<SearchableVec<String>>>,
    update_select: Entity<SelectState<SearchableVec<String>>>,
    excludes_input: Entity<InputState>,
    password_input: Entity<InputState>,
    password_confirm_input: Entity<InputState>,
    enc_select: Entity<SelectState<SearchableVec<String>>>,
    // —— 解压 ——
    conflict_select: Entity<SelectState<SearchableVec<String>>>,
    codepage_select: Entity<SelectState<SearchableVec<String>>>,
    dest_input: Entity<InputState>,
    /// 解压密码输入态（掩码；可选——填了就走 core 首个候选）
    extract_pw_input: Entity<InputState>,
}

/// 参数窗的控件实体（窗口亲和，建一次复用；窗口销毁即随之释放）。
///
/// 初始值一律取自 `form`——表单是唯一真值来源，控件只是它的投影；用户操作
/// 经实体事件（`InputEvent`/`SliderEvent`）或 `DialogEvent` 回灌（见
/// [`apply_form_event`] / [`resync_controls`]）。
fn build_controls(
    form: &ParamsForm,
    ev_tx: &smol::channel::Sender<DialogEvent>,
    window: &mut Window,
    cx: &mut App,
) -> Controls {
    // 格式下拉只露单格式（组合格式界面屏蔽，见 `labels::UI_FORMAT`）
    let fmt_items = labels::UI_FORMAT.iter().map(|(k, _)| *k).collect();
    let mth_items = labels::METHOD.iter().map(|(k, _)| *k).collect();
    let spl_items = labels::SPLIT.iter().map(|(k, _)| *k).collect();
    let thr_items = labels::THREADS.iter().map(|(k, _)| *k).collect();
    let dict_items = labels::DICT.iter().map(|(k, _)| *k).collect();
    let upd_items = labels::UPDATE_MODE.iter().map(|(k, _)| *k).collect();
    let enc_items = labels::ENC_METHOD.iter().map(|(k, _)| *k).collect();
    let cfl_items = labels::CONFLICT.iter().map(|(k, _)| *k).collect();
    let cpg_items = labels::CODEPAGE.iter().map(|(k, _)| *k).collect();
    // 选中事件不在这里落状态：`on_pick` 拿不到 `cx`（无法重绘面板），
    // 故统一经事件通道交给消费者任务处理（见模块文档「事件通道」）。
    //
    // 「压缩方式」候选给**全表**而非本格式子集：单候选格式的 Select 会被禁用
    // 并保持显示该格式的方法；将来某格式有多个编码器时，越集仍由 core 以
    // 5001 拒绝（不靠界面兜底），且切格式时装配方会把选择重置为该格式默认。
    Controls {
        filename_input: crate::ui::dialogs::input_state(&form.filename_text, false, window, cx),
        out_dir_input: crate::ui::dialogs::input_state(&form.out_dir_text, false, window, cx),
        format_select: crate::ui::dialogs::select_string_state(
            fmt_items,
            form.format_ix,
            window,
            cx,
            ev_tx,
            DialogEvent::CompressFormat,
        ),
        method_select: crate::ui::dialogs::select_string_state(
            mth_items,
            form.method_ix.unwrap_or(0),
            window,
            cx,
            ev_tx,
            DialogEvent::CompressMethod,
        ),
        level_slider: crate::ui::dialogs::level_slider_state(form.level_ix, window, cx),
        split_select: crate::ui::dialogs::select_string_state(
            spl_items,
            form.split_ix,
            window,
            cx,
            ev_tx,
            DialogEvent::CompressSplitPreset,
        ),
        split_custom_input: crate::ui::dialogs::number_input_state(
            &form.split_custom_text,
            window,
            cx,
        ),
        threads_select: crate::ui::dialogs::select_string_state(
            thr_items,
            form.threads_ix,
            window,
            cx,
            ev_tx,
            DialogEvent::CompressThreads,
        ),
        dict_select: crate::ui::dialogs::select_string_state(
            dict_items,
            form.dict_ix,
            window,
            cx,
            ev_tx,
            DialogEvent::CompressDict,
        ),
        update_select: crate::ui::dialogs::select_string_state(
            upd_items,
            form.update_ix,
            window,
            cx,
            ev_tx,
            DialogEvent::CompressUpdateMode,
        ),
        excludes_input: crate::ui::dialogs::input_state(&form.excludes_text, false, window, cx),
        password_input: crate::ui::dialogs::input_state("", true, window, cx),
        password_confirm_input: crate::ui::dialogs::input_state("", true, window, cx),
        extract_pw_input: crate::ui::dialogs::input_state("", true, window, cx),
        enc_select: crate::ui::dialogs::select_string_state(
            enc_items,
            form.enc_ix,
            window,
            cx,
            ev_tx,
            DialogEvent::CompressEncMethod,
        ),
        conflict_select: crate::ui::dialogs::select_string_state(
            cfl_items,
            form.conflict_ix,
            window,
            cx,
            ev_tx,
            DialogEvent::ExtractConflict,
        ),
        codepage_select: crate::ui::dialogs::select_string_state(
            cpg_items,
            form.codepage_ix,
            window,
            cx,
            ev_tx,
            DialogEvent::ExtractCodepage,
        ),
        dest_input: crate::ui::dialogs::input_state("", false, window, cx),
    }
}

/// 参数字段（**纯数据**：与控件、窗口解耦，便于「两种工作模式一致性」单测）。
///
/// 两模式一致性的锚点在这里：
/// - 参数窗：控件 → 本结构 → [`ParamsForm::to_compress_vm`] / [`ParamsForm::to_extract_vm`]；
/// - 快捷动词：`dispatch::quick_*_vm`（config 缺省）。
///
/// 二者对**共享参数子集**产出的 VM 必须逐字段相同——`params_form_matches_quick_path`
/// 把这条钉在测试里（否则「右键压缩」与「主界面压缩」会各写一套参数翻译而漂移）。
/// 09 起参数窗多出若干旋钮（方式/线程/固实/开关），该测试同时断言这些旋钮的
/// **未设置值 == `CompressDialogVm::default()`**，即参数窗不引入自己的隐式默认。
///
/// # 输出路径的表示（09 §3.1）
///
/// 界面拆成「文件名 + 保存到」两段（换格式时扩展名自动跟随，消掉「选了 7z 但
/// 输出仍叫 .zip」），故本结构存两段文本，**完整路径由 [`ParamsForm::out_path`]
/// 派生**——不另存一份完整路径，避免两处真值漂移。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParamsForm {
    // —— 压缩 ——
    pub filename_text: String,
    pub out_dir_text: String,
    pub format_ix: usize,
    /// 压缩方式在 `METHOD` 白名单中的下标。
    ///
    /// **`None` = 不显式指定**（交给格式默认编码器）。这是安全默认：像 tar 这种
    /// `caps.methods` 为空的格式，指定任何方式都会被 core 以 5001 拒绝——
    /// 装配方按格式能力把它设为 `None`，任何未经能力派生的表单也落在这里。
    pub method_ix: Option<usize>,
    pub level_ix: usize,
    pub split_ix: usize,
    /// 自定义分卷（**单位 MB**；仅 `split_ix` 指向 custom 时有意义）
    pub split_custom_text: String,
    pub threads_ix: usize,
    /// LZMA2 字典大小在 `DICT` 白名单中的下标（仅 7z 的 LZMA2 消费）
    pub dict_ix: usize,
    /// 更新方式在 `UPDATE_MODE` 白名单中的下标（`append` 仅 `caps.append` 格式可选）
    pub update_ix: usize,
    pub solid: bool,
    pub excludes_text: String,
    pub exclude_hidden: bool,
    pub follow_symlinks: bool,
    pub keep_metadata: bool,
    pub verify_after: bool,
    pub best_effort: bool,
    pub password_text: String,
    pub password_confirm_text: String,
    /// 解压密码（可选）：core 首个候选；空 = 走运行时询问链
    pub extract_pw_text: String,
    pub enc_ix: usize,
    pub enc_names: bool,
    /// 两个折叠区的展开态（09：kit `AccordionItem::open` 是纯 prop，须外部持有）
    pub advanced_open: bool,
    pub enc_open: bool,
    // —— 解压 ——
    pub conflict_ix: usize,
    pub codepage_ix: usize,
    pub dest_text: String,
    pub smart: bool,
}

impl ParamsForm {
    /// 压缩参数窗的初始态 = config `[defaults]` + 按源名派生的输出路径。
    pub(crate) fn defaults_compress(inputs: &[PathBuf], cfg: &hzip_config::Config) -> Self {
        use crate::viewmodel::dialog_vm::{default_out_path, level_from_str, level_index};
        // 下标域 = UI_FORMAT（组合格式屏蔽）；存量配置组合值回落 0=zip。
        let format_ix = labels::UI_FORMAT
            .iter()
            .position(|(_, v)| v.eq_ignore_ascii_case(&cfg.defaults.format))
            .unwrap_or(0);
        let suggested = default_out_path(inputs, labels::UI_FORMAT[format_ix].1);
        let filename_text = suggested
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let out_dir_text = suggested
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        Self {
            format_ix,
            level_ix: level_index(level_from_str(&cfg.defaults.level)),
            filename_text,
            out_dir_text,
            ..Self::empty()
        }
    }

    /// 解压参数窗的初始态 = config `[extract]` / `[security]`。
    pub(crate) fn defaults_extract(cfg: &hzip_config::Config) -> Self {
        Self {
            conflict_ix: labels::CONFLICT
                .iter()
                .position(|(_, v)| *v == cfg.extract.conflict)
                .unwrap_or(0),
            codepage_ix: cfg
                .extract
                .codepage
                .as_deref()
                .and_then(|c| labels::CODEPAGE.iter().position(|(_, v)| *v == c))
                .unwrap_or(0),
            smart: cfg.extract.smart,
            best_effort: cfg.extract.best_effort,
            ..Self::empty()
        }
    }

    fn empty() -> Self {
        Self {
            filename_text: String::new(),
            out_dir_text: String::new(),
            format_ix: 0,
            method_ix: None,
            level_ix: 0,
            split_ix: 0,
            split_custom_text: String::new(),
            threads_ix: 0,
            dict_ix: 0,
            update_ix: 0,
            solid: false,
            excludes_text: String::new(),
            exclude_hidden: false,
            follow_symlinks: false,
            // 与 CompressDialogVm::default() 一致（spec 侧默认开的项不能在这里变关）
            keep_metadata: true,
            verify_after: true,
            best_effort: false,
            password_text: String::new(),
            password_confirm_text: String::new(),
            extract_pw_text: String::new(),
            enc_ix: 0,
            // 「加密文件名」默认开（安全默认，设计稿 11 决策②；仅 7z 可见）
            enc_names: true,
            advanced_open: false,
            enc_open: false,
            conflict_ix: 0,
            codepage_ix: 0,
            dest_text: String::new(),
            smart: false,
        }
    }

    /// 当前格式 ID（**UI_FORMAT** value；`format_ix` 是界面下标域）。
    fn format_id(&self) -> &'static str {
        labels::UI_FORMAT[self.format_ix].1
    }

    /// 输出扩展名（随格式派生；不含点）。
    fn ext(&self) -> &'static str {
        crate::viewmodel::dialog_vm::default_ext(self.format_id())
    }

    /// 完整输出路径（**派生**：保存到 / 文件名.扩展名）。
    fn out_path(&self) -> Option<PathBuf> {
        let name = self.filename_text.trim();
        if name.is_empty() {
            return None;
        }
        let file = format!("{name}.{}", self.ext());
        let dir = self.out_dir_text.trim();
        Some(if dir.is_empty() {
            PathBuf::from(file)
        } else {
            std::path::Path::new(dir).join(file)
        })
    }

    /// 统一压缩级别（`LEVEL` 白名单 value → `Level`）。
    fn level_value(&self) -> hzip_core::Level {
        crate::viewmodel::dialog_vm::level_from_str(labels::LEVEL[self.level_ix].1)
    }

    /// 分卷字节（预设或自定义；「不分卷」= None）。
    fn split_bytes(&self) -> Option<u64> {
        use crate::viewmodel::dialog_vm::{split_custom_bytes, split_preset_bytes};
        let v = labels::SPLIT
            .get(self.split_ix)
            .map(|(_, v)| *v)
            .unwrap_or("none");
        if v == "custom" {
            split_custom_bytes(&self.split_custom_text)
        } else {
            split_preset_bytes(v)
        }
    }

    /// 线程数（`THREADS` 白名单 value；`0` = 自动）。
    fn threads_value(&self) -> u32 {
        labels::THREADS
            .get(self.threads_ix)
            .map(|(_, v)| *v)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0)
    }

    /// 压缩方式（`METHOD` 白名单 value → 枚举；未指定/未知 → `None` = 格式默认）。
    fn method_value(&self) -> Option<hzip_core::CodecMethod> {
        self.method_ix
            .and_then(|i| labels::METHOD.get(i))
            .and_then(|(_, v)| hzip_core::CodecMethod::from_id(v))
    }

    /// 字典大小字节（`DICT` 白名单 value → 字节；`auto`/未知 → `None` = 随级别）。
    fn dict_bytes(&self) -> Option<u64> {
        crate::viewmodel::dialog_vm::dict_from_str(
            labels::DICT
                .get(self.dict_ix)
                .map(|(_, v)| *v)
                .unwrap_or("auto"),
        )
    }

    /// 更新方式 → `append`（`UPDATE_MODE` 白名单 value；未知回落「新建」）。
    fn append_value(&self) -> bool {
        crate::viewmodel::dialog_vm::append_from_str(
            labels::UPDATE_MODE
                .get(self.update_ix)
                .map(|(_, v)| *v)
                .unwrap_or("new"),
        )
    }

    /// 加密请求（`ENC_METHOD` 白名单 value → `EncryptionRequest`）。
    ///
    /// 注意：写侧加密尚未实现（T45/T46），本值仅作 UI 态保留，
    /// [`CompressDialogVm::build_spec`] 不携带（携带即 Unsupported 失败）。
    fn enc_request(&self) -> Option<hzip_format::EncryptionRequest> {
        use hzip_format::{AesKey, EncryptionRequest};
        let picked = match labels::ENC_METHOD
            .get(self.enc_ix)
            .map(|(_, v)| *v)
            .unwrap_or("none")
        {
            "zipcrypto" => Some(EncryptionRequest::ZipCrypto),
            "aes128" => Some(EncryptionRequest::Aes(AesKey::Bits128)),
            "aes256" => Some(EncryptionRequest::Aes(AesKey::Bits256)),
            _ => None,
        };
        if picked.is_some() {
            return picked;
        }
        // 下拉停在「不加密」但用户填了密码 → 按格式缺省方案补齐（AES-256）。
        // 与 CLI `--password-env`/`HZIP_PASSWORD` 同语义；否则「有密码无方案」
        // 会撞 core 的 HZ-5003 单边校验，用户看到的是「不支持的操作」
        //（2026-09-23 实测：右键「添加到压缩文件」填密码即失败）。
        (!self.password_text.trim().is_empty()).then_some(EncryptionRequest::Aes(AesKey::Bits256))
    }

    /// 排除 glob（逗号/分号/空白分隔；空项剔除）。
    fn excludes(&self) -> Vec<String> {
        self.excludes_text
            .split([',', ';', ' ', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// 表单 → 压缩 VM（**唯一参数翻译处**：提交点只认 VM）。
    pub(crate) fn to_compress_vm(&self) -> CompressDialogVm {
        use crate::viewmodel::dialog_vm::level_from_str;
        CompressDialogVm {
            format: Some(hzip_format::FormatId::from(self.format_id())),
            method: self.method_value(),
            level: level_from_str(labels::LEVEL[self.level_ix].1),
            password: (!self.password_text.is_empty())
                .then(|| zeroize::Zeroizing::new(self.password_text.clone())),
            encryption: self.enc_request(),
            encrypt_names: self.enc_names,
            split: self.split_bytes(),
            threads: self.threads_value(),
            dict_size: self.dict_bytes(),
            solid: self.solid,
            append: self.append_value(),
            excludes: self.excludes(),
            exclude_hidden: self.exclude_hidden,
            follow_symlinks: self.follow_symlinks,
            keep_metadata: self.keep_metadata,
            verify_after: self.verify_after,
            best_effort: self.best_effort,
            out: self.out_path(),
        }
    }

    /// 表单 → 解压 VM。
    pub(crate) fn to_extract_vm(
        &self,
        cfg: &hzip_config::Config,
    ) -> crate::viewmodel::dialog_vm::ExtractDialogVm {
        use crate::viewmodel::dialog_vm::{ExtractDialogVm, codepage_from_str, conflict_from_str};
        ExtractDialogVm {
            // 留空 = 档案所在目录（core 语义：dest_root = None）
            dest: (!self.dest_text.trim().is_empty()).then(|| PathBuf::from(self.dest_text.trim())),
            smart: self.smart,
            conflict: conflict_from_str(labels::CONFLICT[self.conflict_ix].1),
            codepage: codepage_from_str(Some(labels::CODEPAGE[self.codepage_ix].1)),
            best_effort: self.best_effort,
            symlink: crate::viewmodel::dialog_vm::symlink_from_str(&cfg.security.symlink),
            // 解压密码（可选）：填了即进 spec 作为 core 首个候选；空 → 运行时询问链
            password: (!self.extract_pw_text.is_empty())
                .then(|| zeroize::Zeroizing::new(self.extract_pw_text.clone())),
            preview: Vec::new(),
        }
    }

    /// 参数摘要（预览行）：参数窗的「所见即所得」——提交前用户就该看到实际参数。
    fn preview(&self) -> String {
        let split = match self.split_bytes() {
            Some(n) => crate::ui::binding::fmt_bytes(n),
            None => crate::tr("dlg.params.no_split"),
        };
        let n_excl = self.excludes().len();
        let excl = if n_excl == 0 {
            crate::tr("dlg.params.no_excludes")
        } else {
            n_excl.to_string()
        };
        let level = labels::label_of(&labels::LEVEL, self.level_ix);
        // 摘要里的方式按实际生效值显示：无方式（tar）与未指定都报「不压缩」
        let method = match self.method_value() {
            Some(m) => labels::label_of(
                &labels::METHOD,
                labels::METHOD
                    .iter()
                    .position(|(_, v)| *v == m.as_str())
                    .unwrap_or(0),
            ),
            None => crate::tr("dlg.level.detail_none"),
        };
        let enc = if self.password_text.is_empty() {
            crate::tr("dlg.compress.enc_none")
        } else {
            crate::tr("dlg.compress.enc_set")
        };
        let dest = if self.dest_text.trim().is_empty() {
            crate::tr("dlg.params.source_dir")
        } else {
            self.dest_text.trim().to_string()
        };
        let on = crate::tr("dlg.params.on");
        let off = crate::tr("dlg.params.off");
        let conflict = labels::label_of(&labels::CONFLICT, self.conflict_ix);
        let codepage = labels::label_of(&labels::CODEPAGE, self.codepage_ix);
        // 压缩摘要由各白名单 label 拼装（09 §3.4）：槽位随参数集增长而增——
        // 单一模板字符串一旦漏了槽位，「改了参数但摘要不动」这类缺陷很难被发现。
        let compress_line = format!(
            "{} · {} · {} · {} · {} · {}",
            self.format_id(),
            method,
            level,
            split,
            excl,
            enc
        );
        // 两行摘要（压缩/解压各一行），窗口按种类取对应行
        format!(
            "{}\u{1}{}",
            compress_line,
            crate::tr_with(
                "dlg.extract.preview",
                &[
                    ("dest", dest.as_str()),
                    (
                        "smart",
                        if self.smart {
                            on.as_str()
                        } else {
                            off.as_str()
                        }
                    ),
                    ("conflict", conflict.as_str()),
                    ("codepage", codepage.as_str()),
                ],
            )
        )
    }

    /// 预览行按种类取（压缩 → 第 1 段；解压 → 第 2 段）。
    fn preview_for(&self, kind: &OpKind) -> String {
        let both = self.preview();
        let mut parts = both.split('\u{1}');
        match kind {
            OpKind::Compress { .. } => parts.next().unwrap_or_default().to_string(),
            OpKind::Extract { .. } => parts.nth(1).unwrap_or_default().to_string(),
        }
    }
}

/// 写侧加密是否可用（T45/T46 实现进度开关；设计稿 11 落地后置 true）。
///
/// 格式能力（`caps.encryption`）说「格式**能**加密」，本常量说「我们**会**写」——
/// 二者都为真才露出加密区。zip 写侧（ZipCrypto + WinZip AES 三档）与 7z 写侧
/// （AES-256 ± 头加密）均已接通，`build_spec` 成对携带密码/加密请求。
const WRITE_ENCRYPTION_READY: bool = true;

/// 格式能力（UI 门控的**唯一来源**）。
///
/// 经 core 注册表取 `handler.caps()`——与实际压缩执行时用的是**同一份**声明，
/// 界面不会与后端漂移（例如某格式的 `solid` feature 被裁剪，界面自动跟着关）。
/// 取不到（格式未注册 / 锁中毒）时回落 `FormatCaps::default()` = 全 false，
/// 即「一律不支持」——宁可界面保守，也不显示一个后端做不到的选项。
fn format_caps(data: &AppDataHandle, format: &str) -> hzip_format::FormatCaps {
    let Ok(d) = data.lock() else {
        return hzip_format::FormatCaps::default();
    };
    d.engine
        .registry()
        .handler(format)
        .map(|h| h.caps())
        .unwrap_or_default()
}

/// 格式默认压缩方式在 `labels::METHOD` 白名单中的下标。
///
/// **`None` = 该格式无可选压缩方式**（`caps.methods` 为空，如 tar 仅打包）——
/// 此时必须不显式指定方式，否则 core 以 5001 拒绝整个压缩任务。
fn default_method_ix(data: &AppDataHandle, format: &str) -> Option<usize> {
    let first = format_caps(data, format).methods.first().copied()?;
    labels::METHOD
        .iter()
        .position(|(_, v)| *v == first.as_str())
}

// （`renamed_target` 已删除：压缩目标已存在时不再预告自动改名，改为「开始时
// 询问覆盖」——见 `OpState::existing_target` 与覆盖确认态。）

/// 一次性参数窗状态（窗口视图与事件消费者共享）。
struct OpState {
    kind: OpKind,
    data: AppDataHandle,
    form: ParamsForm,
    error: String,
    ctl: Controls,
}

impl OpState {
    /// 对象摘要行（压缩：输入批；解压：档案批）。
    fn summary(&self) -> String {
        let (first, n) = match &self.kind {
            OpKind::Compress { inputs } => (inputs.first(), inputs.len()),
            OpKind::Extract { archives } => (archives.first(), archives.len()),
        };
        let Some(first) = first else {
            return String::new();
        };
        let name = first.display().to_string();
        if n <= 1 {
            name
        } else {
            crate::tr_with(
                "dlg.params.multi",
                &[("first", name.as_str()), ("n", &n.to_string())],
            )
        }
    }

    /// 对象统计行（3 个文件 · 2 个目录 · 48.2 MB）。
    ///
    /// 只读**单个**输入项的元数据：统计整个树要走目录遍历，不能在渲染帧里做
    /// （09 §3.1 的体量信息在单文件/单目录时足够；多选时退化为项数说明）。
    fn summary_meta(&self) -> String {
        let OpKind::Compress { inputs } = &self.kind else {
            return String::new();
        };
        match inputs.as_slice() {
            [] => String::new(),
            [one] => match std::fs::metadata(one) {
                Ok(m) if m.is_dir() => crate::tr_with(
                    "dlg.compress.source_dir_meta",
                    &[("path", &one.display().to_string())],
                ),
                Ok(m) => crate::ui::binding::fmt_bytes(m.len()),
                Err(_) => crate::tr("dlg.compress.source_unreadable"),
            },
            many => crate::tr_with(
                "dlg.params.multi",
                &[
                    ("first", &many[0].display().to_string()),
                    ("n", &many.len().to_string()),
                ],
            ),
        }
    }

    /// 预览行（按种类取对应那一段）。
    fn preview(&self) -> String {
        self.form.preview_for(&self.kind)
    }

    /// 当前格式能力。
    fn caps(&self) -> hzip_format::FormatCaps {
        format_caps(&self.data, self.form.format_id())
    }

    /// 对象是否「多文件 / 目录」（单流格式提示的触发条件，09 §7.2）。
    fn source_is_multi_or_dir(&self) -> bool {
        match &self.kind {
            OpKind::Compress { inputs } => {
                inputs.len() > 1 || inputs.first().map(|p| p.is_dir()).unwrap_or(false)
            }
            OpKind::Extract { .. } => false,
        }
    }

    /// 压缩面板 props：VM 投影 + **能力门控** + 派生文案。
    ///
    /// 门控一律取 `caps()`（与执行侧同一份声明）；派生文案（扩展名/实参/各原因行/
    /// 摘要）在此处一次性算好，让面板组件保持纯渲染。
    fn compress_props(&self) -> crate::ui::dialogs::CompressPanelProps {
        use crate::viewmodel::dialog_vm as vm;
        let caps = self.caps();
        let fmt = self.form.format_id();
        let level = self.form.level_value();
        let name = self.form.filename_text.trim();

        // —— 压缩方式：候选 ≤1 → 锁定（禁用 + 原因行） ——
        let method_selectable = caps.methods.len() > 1;
        let method_hint = if caps.methods.is_empty() {
            crate::tr("dlg.compress.hint_no_method")
        } else if !method_selectable {
            crate::tr_with(
                "dlg.compress.hint_method_locked",
                &[("method", caps.methods[0].display())],
            )
        } else {
            String::new()
        };

        // —— 级别实参（数值与执行侧同源） ——
        let level_detail =
            crate::ui::dialogs::compress_level_detail_text(&vm::level_detail(fmt, level));
        let level_speed = crate::ui::dialogs::compress_level_speed_text(level);

        // —— 高级区原因行 ——
        let split_hint = if !caps.split {
            crate::tr("dlg.compress.hint_split_unsupported")
        } else if self.form.split_bytes().is_some() {
            let base = if name.is_empty() { "archive" } else { name };
            crate::tr_with(
                "dlg.compress.hint_split_names",
                &[("name", &format!("{base}.{fmt}"))],
            )
        } else {
            String::new()
        };
        let threads_hint = if fmt == "7z" {
            crate::tr("dlg.compress.hint_threads_7z")
        } else {
            String::new()
        };
        let solid_hint = if caps.solid {
            crate::tr("field.solid_hint")
        } else {
            String::new()
        };

        // —— 字典大小（仅 7z 的 LZMA2 消费；其它格式禁用 + 原因行） ——
        let dict_supported = fmt == "7z";
        let dict_hint = if dict_supported {
            crate::tr("dlg.compress.hint_dict_7z")
        } else {
            crate::tr("dlg.compress.hint_dict_unsupported")
        };

        // —— 更新方式（`caps.append` 才可追加；不支持时禁用而非静默新建） ——
        let append_supported = caps.append;
        let update_hint = if append_supported {
            crate::tr("dlg.compress.hint_update_append")
        } else {
            crate::tr("dlg.compress.hint_update_unsupported")
        };

        // —— 加密区 ——
        let enc_supported = caps.encryption != hzip_format::EncryptionCaps::None;
        let enc_names_supported = caps.encryption == hzip_format::EncryptionCaps::AesHeader;
        // 当前加密方式是否为该格式写侧白名单所允许（HZ-5001 前移：
        // 组合不合法 → 提交前禁用 + 行内提示，而非提交后任务失败）
        let enc_scheme_bad = WRITE_ENCRYPTION_READY
            && match self.form.enc_request() {
                Some(req) => !caps.write_encryption.contains(&req),
                None => false,
            };
        let enc_hint = if !enc_supported {
            crate::tr("dlg.compress.hint_encrypt_fmt_unsupported")
        } else if enc_scheme_bad {
            crate::tr("dlg.compress.scheme_unsupported_format")
        } else {
            // 写侧已接通：无禁用提示（空串 = 不渲染 hint 行）
            String::new()
        };

        // —— 底部提示 ——
        let stream_hint = if vm::is_stream_format(fmt) && self.source_is_multi_or_dir() {
            crate::tr_with("dlg.compress.hint_stream_wrap", &[("format", fmt)])
        } else {
            String::new()
        };
        // 目标已存在：不再预告「自动改名」，改为预告「开始时会问是否覆盖」
        // （覆盖/取消由底部确认态承担，见 `OpState::existing_target`）。
        let exists_hint = match self.existing_target() {
            Some(path) => crate::tr_with("dlg.compress.exists_warn", &[("path", &path)]),
            None => String::new(),
        };

        // —— 校验 ——
        let filename_error = if name.is_empty() || vm::invalid_name_char(name).is_none() {
            String::new()
        } else {
            crate::tr("dlg.compress.name_invalid")
        };
        let pw_mismatch = !self.form.password_text.is_empty()
            && self.form.password_text != self.form.password_confirm_text;
        // 选了加密（方式 ≠ 无、格式支持）→ 密码必填（HZ-5003 前移到提交前）
        let pw_required = WRITE_ENCRYPTION_READY
            && enc_supported
            && self.form.enc_ix != 0
            && self.form.password_text.is_empty();
        let password_error = if pw_mismatch {
            crate::tr("dlg.compress.password_mismatch")
        } else if pw_required {
            crate::tr("dlg.compress.password_required")
        } else {
            String::new()
        };
        let start_enabled = !name.is_empty()
            && filename_error.is_empty()
            && !pw_mismatch
            && !pw_required
            && !enc_scheme_bad
            && self.form.out_path().is_some();

        // —— 折叠区就地摘要 ——
        let split_txt = match self.form.split_bytes() {
            Some(n) => crate::ui::binding::fmt_bytes(n),
            None => crate::tr("dlg.params.no_split"),
        };
        let threads_txt = match self.form.threads_value() {
            0 => crate::tr("dlg.params.auto"),
            n => n.to_string(),
        };
        let dict_txt = labels::label_of(&labels::DICT, self.form.dict_ix);
        let advanced_summary = crate::tr_with(
            "dlg.compress.sum_advanced",
            &[
                ("split", split_txt.as_str()),
                ("threads", threads_txt.as_str()),
                ("dict", dict_txt.as_str()),
            ],
        );
        let enc_summary = if self.form.enc_ix == 0 || self.form.password_text.is_empty() {
            crate::tr("dlg.compress.enc_none")
        } else {
            crate::tr_with(
                "dlg.compress.enc_set",
                &[(
                    "method",
                    labels::label_of(&labels::ENC_METHOD, self.form.enc_ix).as_str(),
                )],
            )
        };

        crate::ui::dialogs::CompressPanelProps {
            summary: self.summary(),
            summary_meta: self.summary_meta(),
            filename_input: self.ctl.filename_input.clone(),
            out_dir_input: self.ctl.out_dir_input.clone(),
            ext: vm::default_ext(fmt).to_string(),
            filename_error,
            format_select: self.ctl.format_select.clone(),
            method_select: self.ctl.method_select.clone(),
            method_selectable,
            method_hint,
            level_slider: self.ctl.level_slider.clone(),
            level_ix: self.form.level_ix,
            level_detail,
            level_speed,
            // 有压缩层（`caps.methods` 非空）才有级别轴；tar 纯打包 → 不渲染
            level_supported: !caps.methods.is_empty(),
            advanced_open: self.form.advanced_open,
            advanced_summary,
            split_select: self.ctl.split_select.clone(),
            split_custom_input: self.ctl.split_custom_input.clone(),
            split_custom_visible: labels::SPLIT
                .get(self.form.split_ix)
                .map(|(_, v)| *v == "custom")
                .unwrap_or(false),
            split_supported: caps.split,
            split_hint,
            threads_select: self.ctl.threads_select.clone(),
            threads_hint,
            // 纯打包格式（tar：`caps.methods` 为空）没有可并行的编码器 → 不渲染
            threads_supported: !caps.methods.is_empty(),
            dict_select: self.ctl.dict_select.clone(),
            dict_supported,
            dict_hint,
            update_select: self.ctl.update_select.clone(),
            append_supported,
            update_hint,
            solid: self.form.solid,
            solid_supported: caps.solid,
            solid_hint,
            excludes_input: self.ctl.excludes_input.clone(),
            exclude_hidden: self.form.exclude_hidden,
            follow_symlinks: self.form.follow_symlinks,
            keep_metadata: self.form.keep_metadata,
            verify_after: self.form.verify_after,
            best_effort: self.form.best_effort,
            enc_open: self.form.enc_open,
            enc_summary,
            password_input: self.ctl.password_input.clone(),
            password_confirm_input: self.ctl.password_confirm_input.clone(),
            password_error,
            enc_select: self.ctl.enc_select.clone(),
            enc_supported,
            enc_writable: enc_supported
                && WRITE_ENCRYPTION_READY
                && !caps.write_encryption.is_empty(),
            enc_names: self.form.enc_names,
            enc_names_supported,
            enc_hint,
            stream_hint,
            exists_hint,
            preview_text: self.preview(),
            start_enabled,
            error: self.error.clone(),
        }
    }

    /// 解压面板 props。
    fn extract_props(&self) -> crate::ui::dialogs::ExtractPanelProps {
        crate::ui::dialogs::ExtractPanelProps {
            summary: self.summary(),
            dest_input: self.ctl.dest_input.clone(),
            password_input: self.ctl.extract_pw_input.clone(),
            smart: self.form.smart,
            conflict_select: self.ctl.conflict_select.clone(),
            codepage_select: self.ctl.codepage_select.clone(),
            best_effort: self.form.best_effort,
            // 编码修复预览为空：修复在解压**执行**时做（core 侧），参数窗阶段
            // 无档案条目可预览——留字段以便后续接 core 的探测结果。
            repaired: Vec::new(),
            error: self.error.clone(),
        }
    }

    /// 压缩目标是否已存在（存在 → 「开始」先问覆盖，而不是默默改名/覆盖）。
    fn existing_target(&self) -> Option<String> {
        let OpKind::Compress { .. } = &self.kind else {
            return None;
        };
        let out = self.form.out_path()?;
        out.exists().then(|| out.display().to_string())
    }

    /// 点「开始」：装配 VM → 提交（走唯一提交点）。返回首条任务号。
    ///
    /// `overwrite`：`true` = 用户在覆盖确认里选了「覆盖」（写用户指定的路径）；
    /// `false` = 走「目标不存在」路径（若此刻已存在，由调用方先转入确认态）。
    fn start(&self, overwrite: bool) -> Result<u64, String> {
        match &self.kind {
            OpKind::Compress { inputs } => {
                let mut vm = self.form.to_compress_vm();
                if vm.out.is_none() {
                    return Err(crate::tr("dlg.params.need_output"));
                }
                // 加密方式 ≠ 无 → 密码必填（与面板禁提交双保险；core HZ-5003 兜底）
                if vm.encryption.is_some() && vm.password.is_none() {
                    return Err(crate::tr("dlg.compress.password_required"));
                }
                // 覆盖 = 原样写用户指定的路径；不覆盖 = 绝不静默覆盖既有档案
                // （目标若已存在，调用方先转入覆盖确认态；走到这里仍撞名 = 竞态，
                // 退化为自动改名而不是覆盖）。
                vm.out = target_path(&self.form, overwrite);
                submit_compress(&self.data, inputs, &vm)
            }
            OpKind::Extract { archives } => {
                let vm = self.form.to_extract_vm(&hzip_config::Config::load());
                let sel = crate::viewmodel::dialog_vm::SelectionArg::All;
                let mut first = None;
                for archive in archives {
                    let id = submit_extract(&self.data, archive, &vm, &sel)?;
                    first.get_or_insert(id);
                }
                first.ok_or_else(|| crate::tr("dlg.params.need_input"))
            }
        }
    }
}

/// 实际写入路径决策（纯函数，可测）：
///
/// - `overwrite = true`（用户在覆盖确认里选了「覆盖」）→ 原样用用户指定的路径；
/// - `overwrite = false` → 绝不写既有档案：撞名时走 [`unique_out_path`] 改名兜底。
///
/// [`unique_out_path`]: crate::viewmodel::dialog_vm::unique_out_path
fn target_path(form: &ParamsForm, overwrite: bool) -> Option<PathBuf> {
    let out = form.out_path()?;
    Some(if overwrite {
        out
    } else {
        crate::viewmodel::dialog_vm::unique_out_path(&out)
    })
}

/// 提交结果归一：成功 → 开输出（进度）窗并关参数窗；失败 → 就地报错、留在窗内。
///
/// 返回 `true` = 该关参数窗（调用方据此走统一关闭路径）。
fn finish_submit(
    state: &Shared,
    view: &Entity<ParamsWindow>,
    res: Result<u64, String>,
    cx: &mut gpui_kit::AsyncApp,
) -> bool {
    match res {
        Ok(job_id) => {
            let data = state.borrow().data.clone();
            let is_extract = matches!(state.borrow().kind, OpKind::Extract { .. });
            // 提交即清空密码表单态（02 §1.2；spec 内 Zeroizing 随任务生命周期）
            {
                let mut st = state.borrow_mut();
                st.form.password_text.clear();
                st.form.extract_pw_text.clear();
                st.form.password_confirm_text.clear();
            }
            // 关参数窗 → 开输出窗（顺序即用户体验；进度窗强制显示：
            // 直接模式无主界面兜底）。
            // 解压改为**挂起**（设计稿 11 §4.2 密码门时序）：core 的头加密
            // 打开重问与预热问询发生在首个进度事件之前——进度窗等密码应答
            // 后由泵开，密码弹窗不再压在进度窗上；压缩无门控，维持立即开。
            if is_extract {
                if let Ok(app) = data.lock()
                    && let Ok(mut j) = app.jobs.lock()
                {
                    j.register_progress_open(job_id, true);
                }
            } else {
                cx.update(|cx| {
                    open_progress_window_with(cx, &data, job_id, true);
                });
            }
            true
        }
        Err(e) => {
            let mut st = state.borrow_mut();
            st.error = e;
            drop(st);
            cx.update(|cx| refresh(view, cx));
            false
        }
    }
}

/// 状态变更后重绘参数面板（面板是本视图画的；`Root` 不 observe 子视图，
/// 故 notify 的必须是 [`ParamsWindow`] 自身）。
fn refresh(view: &Entity<ParamsWindow>, cx: &mut App) {
    view.update(cx, |_, cx| cx.notify());
}

/// 控件事件 → 表单（参数窗的**唯一落账处**；`Apply`/`Cancel`/`Browse` 除外）。
///
/// 返回 `true` = 表单已变、面板需重绘。
///
/// 抽成纯函数是为了**可测**：`panel_controls_reach_spec` 用测试窗口点真控件
/// （`window.click("compress-verify-after", cx)`）拿到事件，喂给本函数，再断言
/// `to_compress_vm().build_spec()` 逐字段带着这次点击——「控件画出来了但没接线」
/// 这类缺陷因此进不了主干。
fn apply_form_event(form: &mut ParamsForm, ev: &DialogEvent) -> bool {
    match ev {
        DialogEvent::CompressFormat(ix) => form.format_ix = *ix,
        DialogEvent::CompressMethod(ix) => form.method_ix = Some(*ix),
        DialogEvent::CompressSplitPreset(ix) => form.split_ix = *ix,
        DialogEvent::CompressThreads(ix) => form.threads_ix = *ix,
        DialogEvent::CompressEncMethod(ix) => form.enc_ix = *ix,
        DialogEvent::CompressDict(ix) => form.dict_ix = *ix,
        DialogEvent::CompressUpdateMode(ix) => form.update_ix = *ix,
        DialogEvent::CompressSolid(v) => form.solid = *v,
        DialogEvent::CompressExcludeHidden(v) => form.exclude_hidden = *v,
        DialogEvent::CompressFollowSymlinks(v) => form.follow_symlinks = *v,
        DialogEvent::CompressKeepMetadata(v) => form.keep_metadata = *v,
        DialogEvent::CompressVerifyAfter(v) => form.verify_after = *v,
        DialogEvent::CompressBestEffort(v) => form.best_effort = *v,
        DialogEvent::CompressEncryptNames(v) => form.enc_names = *v,
        DialogEvent::CompressToggleAdvanced(v) => form.advanced_open = *v,
        DialogEvent::CompressToggleEncryption(v) => form.enc_open = *v,
        DialogEvent::ExtractConflict(ix) => form.conflict_ix = *ix,
        DialogEvent::ExtractCodepage(ix) => form.codepage_ix = *ix,
        DialogEvent::ExtractSmart(v) => form.smart = *v,
        DialogEvent::ExtractBestEffort(v) => form.best_effort = *v,
        _ => return false,
    }
    true
}

/// 把表单值回灌到**受控实体**（浏览选路径 / 切格式等非用户直接编辑的改动之后）。
///
/// 输入框与下拉各自持值：改了表单却不同步实体，界面就会停在上一个值——用户按
/// 界面上看到的旧值理解，程序却按新值执行（本项目已吃过同类亏：设置页冷启动
/// 不回放列设置）。故凡是非用户键入造成的表单变更，都要经过本函数回灌。
fn resync_controls(
    state: &Shared,
    handle: &gpui_kit::AnyWindowHandle,
    cx: &mut gpui_kit::AsyncApp,
) {
    let (name, dir, f_ix, m_ix) = {
        let s = state.borrow();
        (
            s.form.filename_text.clone(),
            s.form.out_dir_text.clone(),
            s.form.format_ix,
            // 无方式（tar）时下拉显示首项即可——它同时是禁用态
            s.form.method_ix.unwrap_or(0),
        )
    };
    let (f_in, d_in, f_sel, m_sel) = {
        let s = state.borrow();
        (
            s.ctl.filename_input.clone(),
            s.ctl.out_dir_input.clone(),
            s.ctl.format_select.clone(),
            s.ctl.method_select.clone(),
        )
    };
    let _ = handle.update(cx, |_, window, cx| {
        f_in.update(cx, |st, cx| st.set_value(name, window, cx));
        d_in.update(cx, |st, cx| st.set_value(dir, window, cx));
        f_sel.update(cx, |st, cx| {
            st.set_selected_index(Some(IndexPath::new(f_ix)), window, cx);
        });
        m_sel.update(cx, |st, cx| {
            st.set_selected_index(Some(IndexPath::new(m_ix)), window, cx);
        });
    });
}

/// 打开压缩参数窗（右键「添加到压缩文件…」）。
pub(crate) fn open_compress_params(cx: &mut App, data: &AppDataHandle, inputs: Vec<PathBuf>) {
    let form = ParamsForm::defaults_compress(&inputs, &hzip_config::Config::load());
    open_params_window(
        cx,
        data,
        OpKind::Compress { inputs },
        form,
        "dlg.compress.title",
    );
}

/// 打开解压参数窗（右键「解压选项…」）。
pub(crate) fn open_extract_params(cx: &mut App, data: &AppDataHandle, archives: Vec<PathBuf>) {
    let form = ParamsForm::defaults_extract(&hzip_config::Config::load());
    open_params_window(
        cx,
        data,
        OpKind::Extract { archives },
        form,
        "dlg.extract.title",
    );
}

/// 标准消息弹窗（与业务无关；级别 + 标题 + 详情 + 关闭）。
///
/// # 规格（用户口径 2026-09-29）
///
/// - **可拖动调整大小**（`is_resizable: true` + 最小尺寸约束）；
/// - 详情**超长可滚动**（挂在正文自身，`.overflow_y_scrollbar()`——kit 的
///   `ScrollableElement` 会把 `flex_1`/`min_h_0` 搬进滚动根，见 `ui::dialogs` 的说明）；
/// - 结构固定：级别图标 + 标题 + 详情 + 「关闭」按钮；
/// - 级别决定图标与配色（信息/警告/错误），**文案全部由调用方给**（业务无关）。
///
/// 调用点：直接模式意图失败、密码连错上限、文件关联失败/部分失败等。
pub(crate) fn open_message_window(cx: &mut App, dialog: crate::viewmodel::message::MessageDialog) {
    use gpui_kit::component::scroll::ScrollableElement as _;

    /// 标准消息视图。
    struct MessageView {
        dialog: crate::viewmodel::message::MessageDialog,
    }

    impl Render for MessageView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_kit::component::button::{Button, ButtonVariants as _};
            let (icon, color) = level_style(self.dialog.level, cx);
            v_flex()
                .size_full()
                .gap(sp(3.))
                .p(sp(6.))
                // 头部：级别图标 + 标题（窗口标题栏另有同一标题，便于任务栏识别）
                .child(
                    h_flex()
                        .gap(sp(2.))
                        .items_center()
                        .child(Icon::new(icon).size(px(20.)).text_color(color))
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(SharedString::from(self.dialog.title.clone())),
                        ),
                )
                // 正文：可滚动（长清单/长报错不截断、不撑破窗口）
                .child(
                    div().flex_1().min_h_0().overflow_y_scrollbar().child(
                        div()
                            .text_sm()
                            .child(SharedString::from(self.dialog.detail.clone())),
                    ),
                )
                // 底部：关闭（唯一的确定动作）
                .child(
                    h_flex().justify_end().child(
                        Button::new("msg-close")
                            .with_size(component_size())
                            .primary()
                            .label(crate::tr("dlg.close"))
                            .on_click(|_, window, _| window.remove_window()),
                    ),
                )
        }
    }

    let options = gpui_kit::WindowOptions {
        window_bounds: Some(gpui_kit::WindowBounds::Windowed(
            gpui_kit::Bounds::centered(None, size(MESSAGE_WINDOW_W, MESSAGE_WINDOW_H), cx),
        )),
        window_min_size: Some(size(MESSAGE_WINDOW_MIN_W, MESSAGE_WINDOW_MIN_H)),
        // 用户口径 2026-09-29：错误窗口太小且不可调 —— 支持拖动调整大小
        is_resizable: true,
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some(SharedString::from(dialog.title.clone())),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        ..Default::default()
    };
    if let Err(e) = cx.open_window(options, move |window, cx| {
        let view = cx.new(|_| MessageView { dialog });
        cx.new(|cx| Root::new(view, window, cx))
    }) {
        tracing::error!("消息弹窗创建失败: {e}");
    }
}

/// 级别 → 图标。**形状必须互不相同**（不只靠颜色区分级别）。
fn level_icon(level: crate::viewmodel::message::MessageLevel) -> AppIcon {
    match level {
        crate::viewmodel::message::MessageLevel::Info => AppIcon::Info,
        crate::viewmodel::message::MessageLevel::Warning => AppIcon::Warn,
        crate::viewmodel::message::MessageLevel::Error => AppIcon::OctagonAlert,
    }
}

/// 级别 → （图标, 配色）。信息/警告/错误必须一眼可辨（图标形状不同，不只靠颜色）。
fn level_style(
    level: crate::viewmodel::message::MessageLevel,
    cx: &App,
) -> (AppIcon, gpui_kit::Hsla) {
    let theme = cx.theme();
    let color = match level {
        crate::viewmodel::message::MessageLevel::Info => theme.info,
        crate::viewmodel::message::MessageLevel::Warning => theme.warning,
        crate::viewmodel::message::MessageLevel::Error => theme.danger,
    };
    (level_icon(level), color)
}

/// 参数窗本体：建控件实体 → 建视图 → 订阅控件 → 消费者任务。
/// 参数窗高度按**内容**估：固定 660 对解压面会留下半屏空白（用户 2026-09-23 实测）。
///
/// 行数取该面实际渲染的字段条数（解压面 = 摘要/目标/智能/冲突/编码/尽力而为；
/// 压缩面 = 格式/级别/方式/线程/字典/分卷/更新/排除/高级/加密…）。结果夹在
/// `[PARAMS_WINDOW_MIN_H, PARAMS_WINDOW_H]`，压缩面仍取上限（本来就满）。
fn params_window_height(kind: &OpKind) -> gpui_kit::Pixels {
    /// 头部（标题 + 上下内边距）与底部（按钮区）固定开销。
    const CHROME: f32 = 234.;
    /// 一行控件的高度（含行距）。
    const ROW: f32 = 52.;
    let rows = match kind {
        // 摘要/目标/密码/智能/冲突/编码/尽力而为
        OpKind::Extract { .. } => 7,
        _ => 12,
    };
    let est = CHROME + rows as f32 * ROW;
    let (min, max) = (f32::from(PARAMS_WINDOW_MIN_H), f32::from(PARAMS_WINDOW_H));
    gpui_kit::px(est.clamp(min, max))
}

fn open_params_window(
    cx: &mut App,
    data: &AppDataHandle,
    kind: OpKind,
    form: ParamsForm,
    title_key: &str,
) {
    let (ev_tx, ev_rx) = smol::channel::unbounded::<DialogEvent>();
    let options = gpui_kit::WindowOptions {
        window_bounds: Some(gpui_kit::WindowBounds::Windowed(
            gpui_kit::Bounds::centered(
                None,
                size(PARAMS_WINDOW_W, params_window_height(&kind)),
                cx,
            ),
        )),
        window_min_size: Some(size(PARAMS_WINDOW_MIN_W, PARAMS_WINDOW_MIN_H)),
        is_resizable: true,
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some(SharedString::from(crate::tr(title_key))),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        ..Default::default()
    };
    let data_w = data.clone();
    let open = cx.open_window(options, move |window, cx| {
        // 控件初始值取自 form（借用只到这里：随后 form 归 OpState）
        let ctl = build_controls(&form, &ev_tx, window, cx);
        let state: Shared = Rc::new(RefCell::new(OpState {
            kind,
            data: data_w,
            form,
            error: String::new(),
            ctl,
        }));
        // 窗口内容视图 = 参数面板（不是空背景 + 对话框层，见模块文档「界面承载」）
        let view = cx.new(|_| ParamsWindow {
            state: state.clone(),
            ev_tx: ev_tx.clone(),
        });
        // `Root` 仍是窗口根：Select 下拉/native menu，以及 sheet/dialog/
        // notification 三层弹层，都由 `WindowState` 插件在 `gpui_base::Root::render`
        // 内自动挂载（0.7.1；本视图不再手工挂层）
        let root = cx.new(|cx| Root::new(view.clone(), window, cx));
        let handle = window.window_handle();

        // 控件 → 状态（输入框/滑块走实体事件；下拉/开关走事件通道）
        //
        // 七个输入框的绑定逻辑完全相同，抽成一个 fn 收口——复制七份的时代，
        // 漏一个字段的订阅就是「输入框能打字、参数没跟上」这类难查的静默缺陷。
        fn bind_input(
            view: &Entity<ParamsWindow>,
            state: &Shared,
            input: &Entity<InputState>,
            cx: &mut App,
            write: fn(&mut ParamsForm, String),
        ) {
            let s = state.clone();
            let r = view.clone();
            cx.subscribe(input, move |st, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    let v = st.read(cx).value().to_string();
                    write(&mut s.borrow_mut().form, v);
                    refresh(&r, cx);
                }
            })
            .detach();
        }
        {
            let cur = state.borrow().ctl.filename_input.clone();
            bind_input(&view, &state, &cur, cx, |f, v| f.filename_text = v);
        }
        {
            let cur = state.borrow().ctl.out_dir_input.clone();
            bind_input(&view, &state, &cur, cx, |f, v| f.out_dir_text = v);
        }
        {
            let cur = state.borrow().ctl.split_custom_input.clone();
            bind_input(&view, &state, &cur, cx, |f, v| f.split_custom_text = v);
        }
        {
            let cur = state.borrow().ctl.excludes_input.clone();
            bind_input(&view, &state, &cur, cx, |f, v| f.excludes_text = v);
        }
        {
            let cur = state.borrow().ctl.dest_input.clone();
            bind_input(&view, &state, &cur, cx, |f, v| f.dest_text = v);
        }
        {
            // 解压密码：组件内部态通道（提交后清空）；此处只同步到表单
            let cur = state.borrow().ctl.extract_pw_input.clone();
            bind_input(&view, &state, &cur, cx, |f, v| f.extract_pw_text = v);
        }
        {
            // 密码两个框都是「组件内部态」通道（提交后清空）；此处只同步到表单
            let cur = state.borrow().ctl.password_input.clone();
            bind_input(&view, &state, &cur, cx, |f, v| f.password_text = v);
        }
        {
            let cur = state.borrow().ctl.password_confirm_input.clone();
            bind_input(&view, &state, &cur, cx, |f, v| f.password_confirm_text = v);
        }
        {
            // 级别滑块（09 §6）：`Slider` 无元素级回调，走实体事件（同 InputState）
            let s = state.clone();
            let r = view.clone();
            let slider = state.borrow().ctl.level_slider.clone();
            cx.subscribe(&slider, move |_st, ev: &SliderEvent, cx| {
                if let SliderEvent::Change(v) = ev {
                    let ix = (v.end().round().max(0.) as usize).min(labels::LEVEL.len() - 1);
                    s.borrow_mut().form.level_ix = ix;
                    refresh(&r, cx);
                }
            })
            .detach();
        }

        // 事件消费者：一切需要 `cx`/`window` 的动作都在这里做（本仓库既有模式）
        cx.spawn({
            let state = state.clone();
            let view = view.clone();
            async move |cx| {
                while let Ok(ev) = ev_rx.recv().await {
                    let mut close = false;
                    match ev {
                        // 「开始」：目标已存在 → 先转入覆盖确认态（不提交、不改名）
                        // 「开始」：目标已存在 → **弹 Dialog 确认**（不提交）；否则直接提交
                        DialogEvent::CompressStart => {
                            let existing = state.borrow().existing_target();
                            match existing {
                                Some(target) => {
                                    // 对话框的事件出口与面板同一条（ev_tx）
                                    let tx = view.read_with(cx, |v, _| v.ev_tx.clone());
                                    let _ = handle.update(cx, |_, window, cx| {
                                        window.open_dialog(cx, move |_dialog, _window, cx| {
                                            let tx = tx.clone();
                                            crate::ui::dialogs::compress_overwrite_dialog(
                                                cx,
                                                &target,
                                                move |e| {
                                                    let _ = tx.try_send(e);
                                                },
                                            )
                                        });
                                    });
                                }
                                None => {
                                    let res = state.borrow().start(false);
                                    close = finish_submit(&state, &view, res, cx);
                                }
                            }
                        }
                        // 「覆盖」：按用户指定的路径写（不自动改名）
                        DialogEvent::CompressOverwriteAccept => {
                            let res = state.borrow().start(true);
                            close = finish_submit(&state, &view, res, cx);
                        }
                        // 「取消」/✕/Esc/遮罩：对话框自己关层，这里无需动作
                        //（落入下面的默认分支，`apply_form_event` 返回 false → 不重绘）
                        DialogEvent::ExtractStart => {
                            let res = state.borrow().start(false);
                            close = finish_submit(&state, &view, res, cx);
                        }
                        DialogEvent::CompressCancel | DialogEvent::ExtractCancel => close = true,
                        // 换格式：落表单后还要把「压缩方式」重置到该格式的默认
                        // 编码器，并回灌受控实体（下拉持值）——故不走下面那条纯路径。
                        //
                        // 界面不该显示一个该格式做不到的方式（越集虽会被 core 以
                        // 5001 拒绝，但那要等到提交时才报错）。
                        DialogEvent::CompressFormat(_) => {
                            {
                                let mut st = state.borrow_mut();
                                apply_form_event(&mut st.form, &ev);
                                let fmt = st.form.format_id();
                                st.form.method_ix = default_method_ix(&st.data, fmt);
                            }
                            resync_controls(&state, &handle, cx);
                            cx.update(|cx| refresh(&view, cx));
                        }
                        DialogEvent::CompressBrowse => {
                            // 原生保存框一次拿到**完整路径** → 拆回「文件名 + 保存到」
                            // 两段；扩展名若命中白名单格式，顺带切格式（所见即所得）
                            let init = state
                                .borrow()
                                .form
                                .out_path()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default();
                            let picked = cx
                                .background_executor()
                                .spawn(async move {
                                    smol::unblock(move || {
                                        crate::app::file_picker::pick_save(std::path::Path::new(
                                            &init,
                                        ))
                                    })
                                    .await
                                })
                                .await;
                            if let Some(p) = picked {
                                {
                                    let mut s = state.borrow_mut();
                                    if let Some(name) = p.file_stem() {
                                        s.form.filename_text = name.to_string_lossy().into_owned();
                                    }
                                    s.form.out_dir_text = p
                                        .parent()
                                        .map(|d| d.display().to_string())
                                        .unwrap_or_default();
                                    if let Some(ext) = p.extension().and_then(|e| e.to_str())
                                        && let Some(fx) = labels::UI_FORMAT
                                            .iter()
                                            .position(|(_, v)| v.eq_ignore_ascii_case(ext))
                                    {
                                        s.form.format_ix = fx;
                                    }
                                }
                                let fmt = state.borrow().form.format_id();
                                let data = state.borrow().data.clone();
                                state.borrow_mut().form.method_ix = default_method_ix(&data, fmt);
                                resync_controls(&state, &handle, cx);
                                cx.update(|cx| refresh(&view, cx));
                            }
                        }
                        DialogEvent::ExtractBrowse => {
                            let picked = cx
                                .background_executor()
                                .spawn(async {
                                    smol::unblock(crate::app::file_picker::pick_directory).await
                                })
                                .await;
                            if let Some(dir) = picked {
                                let text = dir.display().to_string();
                                state.borrow_mut().form.dest_text = text.clone();
                                let input = state.borrow().ctl.dest_input.clone();
                                let _ = handle.update(cx, |_, window, cx| {
                                    input.update(cx, |st, cx| st.set_value(text, window, cx));
                                });
                                cx.update(|cx| refresh(&view, cx));
                            }
                        }
                        // 其余控件事件：纯落表单（唯一落账处 = `apply_form_event`；
                        // 必须排在最后——Browse/Start/Cancel 都要先被上面的分支截走）
                        _ => {
                            if apply_form_event(&mut state.borrow_mut().form, &ev) {
                                cx.update(|cx| refresh(&view, cx));
                            }
                        }
                    }
                    if close {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                        break;
                    }
                }
            }
        })
        .detach();

        root
    });
    match open {
        // 参数窗是用户刚点出来的，必须在最前面（否则用户以为没反应）
        Ok(handle) => {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
        Err(e) => tracing::error!("参数窗创建失败: {e}"),
    }
}

#[cfg(test)]
#[path = "tests/op_window.rs"]
mod tests;
