//! 打开链（条目打开 / 容器进出 / 打开进度窗 + 任务中心留痕）——自 `shell/mod.rs` 拆分。

use gpui_kit::App;

use super::app_shell::AppShell;
use super::errors::hz_reason;
use super::runtime::AppDataHandle;
use super::session::{SESSION_SUPERSEDED, start_session_switch};

impl AppShell {
    /// 打开当前单选文件条目（T-SEL4：右键「打开」/双击文件）。
    ///
    /// **一律物化副本后用系统程序打开**（用户口径 2026-09-28：内置文本/图片
    /// 查看器**暂时停用**）——`[ui].image_viewer` / `[ui].editor` 按扩展名分流
    /// 指定程序（设置-查看器），未指定则系统默认程序（无关联时 OS 弹「你要如何
    /// 打开这个文件」）。旧 `OpenAction::Preview` 的内置查看器路由已下线。
    ///
    /// **加密条目先过密码门**（用户口径 2026-09-28）：当前会话档案未缓存密码 →
    /// 弹密码框，验证通过后缓存并自动重跑本入口（此后同档案再打开文件/解压
    /// 都不再询问——即「该压缩包处于解密中状态」）。
    ///
    /// 非恰好单选文件（多选/目录）→ no-op（菜单 enable 已拦；双击路径的前置
    /// SelectRow 保证单选）。
    pub fn open_selection_entry(&mut self, cx: &mut App) {
        self.open_selected_entry(crate::viewmodel::OpenForm::Configured, cx);
    }

    /// 「打开方式…」（右键菜单；用户口径 2026-09-28）：走**系统原生询问框**
    /// （`SHOpenWithDialog`，与资源管理器「打开方式 ▸ 选择其他应用」同一个框）。
    ///
    /// 关键实测（见 `hzip_platform::open` 模块文档）：该 API **必须从非属主窗口线程
    /// 调用**——同线程调用立即 `ERROR_CANCELLED`、什么都不出现（这正是前一版误判
    /// 「系统通道不可用」而自实现候选列表的原因）。hzip 的打开流程跑在 `hzip-open`
    /// 工作线程上、gpui 主线程持续泵消息，天然满足条件。
    pub fn open_selection_entry_with_picker(&mut self, cx: &mut App) {
        self.open_selected_entry(crate::viewmodel::OpenForm::SystemPicker, cx);
    }

    /// 选中条目打开的**唯一入口**（形态由 [`OpenForm`] 决定；两条菜单项共用）。
    pub(super) fn open_selected_entry(&mut self, form: crate::viewmodel::OpenForm, cx: &mut App) {
        let Some((id, name, encrypted, key, cached)) = self.selected_single_file() else {
            return;
        };
        let Some(data) = self.data.clone() else {
            return;
        };
        // 密码门：加密条目 + 该档案未缓存密码 → 先问密码（提交验证通过后由主
        // 线程泵取出待执行动作重跑；届时缓存命中直接放行）。
        if encrypted && !cached {
            if let Some(key) = key {
                // 目标（id/名/形态）随门携带——不依赖选择集在门弹出期间不变
                arm_password_gate(
                    &data,
                    crate::viewmodel::GateAction::OpenSelection {
                        id,
                        name: name.clone(),
                        form,
                    },
                    key,
                );
            }
            return;
        }
        self.open_one_entry(id, name, form, cx);
    }

    /// 恰好单选一个**文件**条目时的打开目标（多选/目录/未选中 → `None`）。
    ///
    /// 返回 `(id, 名, 是否加密, 会话密码缓存键, 该键是否已缓存)`。
    fn selected_single_file(
        &self,
    ) -> Option<(
        hzip_format::EntryId,
        String,
        bool,
        Option<std::path::PathBuf>,
        bool,
    )> {
        let data = self.data.clone()?;
        let d = data.lock().ok()?;
        let ids = d.main.selection();
        // T-SEL：双通道合计恰一（多选/目录 → no-op，菜单 enable 同口径）
        if ids.len() + d.main.dir_selection().len() != 1 || ids.is_empty() {
            return None;
        }
        let entry = d.main.entry(ids[0])?;
        if entry.is_dir {
            return None;
        }
        let key = d.main.source_path().map(|p| p.to_path_buf());
        let cached = key
            .as_deref()
            .is_some_and(|k| d.cached_password(k).is_some());
        Some((ids[0], entry.name.clone(), entry.encrypted, key, cached))
    }

    /// 打开**已定位**的条目（选中条目入口与密码门放行后共用）。
    ///
    /// 与选择集口径分开的原因（用户口径 2026-09-28 实测「输入密码后不继续走」）：
    /// 门放行后要打开的是**武装门时捕获的那个条目 + 那份程序选择**——不能重新从
    /// 选择集推导（门弹出期间快照刷新/行集重建可能改写选择集，重新推导会直接
    /// no-op）。
    pub(crate) fn open_one_entry(
        &mut self,
        id: hzip_format::EntryId,
        name: String,
        form: crate::viewmodel::OpenForm,
        cx: &mut App,
    ) {
        let Some(data) = self.data.clone() else {
            return;
        };
        // 物化单条目 → 指定程序/系统默认程序；后台线程——磁盘 IO 不上 UI 线程。
        //
        // **三段式**（2026-09-24 用户实测「主界面卡住」根治）：物化要把整条内容
        // 解出来（大条目分钟级），旧实现在**持 `AppData` 锁**的情况下解 —— 主界面
        // 每帧渲染都要那把锁 ⇒ 界面无响应、也看不到任何进度。现在：
        // ① 锁内摘出浏览器 + 会话密码（微秒级）② 锁外物化解压 ③ 锁内放回；
        // 并在任务中心留痕（成功/失败都记，失败带原因）。
        let form_copy = form;
        let Some(archive) = data.lock().ok().and_then(|d| d.archive.clone()) else {
            tracing::warn!(
                entry_id = id,
                "打开条目：当前会话无档案（AppData.archive 为空）→ 忽略"
            );
            return;
        };
        tracing::info!(
            entry_id = id,
            name = %name,
            archive = %archive.display(),
            form = form_copy.kind(),
            "打开条目：起物化 + 外部打开流程"
        );
        // 写回目标（**登记时定死**）：嵌套会话 / 不支持编辑的格式 ⇒ 不写回，
        // 保存时如实告知（见 `app::external_edit` 的边界说明）。
        let target = data
            .lock()
            .map(|d| super::external_edits::write_back_target(&d, &archive, id))
            .unwrap_or(crate::app::external_edit::WriteBack::Disabled {
                reason: "edit.writeback.unsupported",
            });
        let edit_tx = self.edit_tx.clone();
        let work = {
            let data = data.clone();
            let archive = archive.clone();
            let name = name.clone();
            let target = target.clone();
            let edit_tx = edit_tx.clone();
            move |detail: &crate::ui::views::open_progress::OpenDetail| {
                detail.set_phase(crate::tr("openprogress.phase_materialize_one"));
                // ① 锁内摘出浏览器 + 会话密码（密码克隆：重活不持锁）
                let (browser, pw) = {
                    let mut d = data.lock().map_err(|_| "state 锁中毒".to_string())?;
                    let pw = d
                        .main
                        .source_path()
                        .map(|p| p.to_path_buf())
                        .and_then(|p| d.cached_password(&p));
                    let browser = d
                        .main
                        .take_browser()
                        .ok_or_else(|| crate::tr("msg.copy_no_archive"))?;
                    (browser, pw)
                };
                // ② 锁外物化（可能整条解压）→ ③ 锁内放回
                let res = crate::viewmodel::MainViewModel::materialize_in(
                    &browser,
                    &[id],
                    &archive,
                    pw.as_ref(),
                );
                if let Ok(mut d) = data.lock() {
                    d.main.restore_browser(browser);
                }
                let (paths, failed) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        // 密码错（缓存已失效/该格式无廉价预检）→ 丢缓存 + 重新武装
                        // 密码门（自愈：错密码绝不留缓存，重问而非死路）
                        if matches!(e, hzip_error::HzipError::Password { .. })
                            && let Some(key) = data
                                .lock()
                                .ok()
                                .and_then(|d| d.main.source_path().map(|p| p.to_path_buf()))
                        {
                            if let Ok(mut d) = data.lock() {
                                d.forget_password(&key);
                            }
                            arm_password_gate(
                                &data,
                                crate::viewmodel::GateAction::OpenSelection {
                                    id,
                                    name: name.clone(),
                                    form: form_copy,
                                },
                                key,
                            );
                        }
                        return Err(hz_reason(&e));
                    }
                };
                tracing::info!(?paths, ?failed, "打开条目：物化完成");
                if let Some(p) = paths.first() {
                    // 程序优先级：「打开方式…」当场选定的程序 > 设置-查看器按扩展名
                    // 指定的程序 > 系统默认程序。
                    //
                    // 系统默认程序那条：该扩展名**没有可用默认程序**时，Windows 自己
                    // 会弹「你要如何打开这个文件？」询问框（用户口径 2026-09-28：
                    // 不知道打开方式就问）——平台层已带属主窗口，询问框稳定可见
                    // （见 `hzip_platform::open` 模块文档实测表）。
                    // 形态 → 实际程序：系统询问框直接调 `open_with_picker`
                    // （阻塞到用户选定；跑在本流程的 `hzip-open` 工作线程上，
                    // 属主窗口所在线程继续泵消息——见平台层线程模型实测表）；
                    // 配置形态按扩展名分流 `[ui].image_viewer`/`editor`，都为空
                    // 则系统默认程序（该类型没有可用默认程序时 Windows 自己弹
                    // 询问框）。
                    let configured = data.lock().ok().and_then(|d| {
                        let iv = d.settings.image_viewer.trim();
                        let ed = d.settings.editor.trim();
                        let ext = p
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        let img = IMAGE_VIEWER_EXTS.contains(&ext.as_str());
                        let configured = if img { iv } else { ed };
                        (!configured.is_empty()).then(|| configured.to_string())
                    });
                    let opened = match form_copy {
                        crate::viewmodel::OpenForm::SystemPicker => {
                            hzip_platform::open::open_with_picker(p)
                        }
                        crate::viewmodel::OpenForm::Configured => match configured.as_deref() {
                            Some(prog) => hzip_platform::open::open_with_program(p, prog),
                            None => hzip_platform::open::open_with_default_app(p),
                        },
                    };
                    match &opened {
                        Ok(()) => tracing::info!(
                            path = %p.display(),
                            form = form_copy.kind(),
                            "外部打开：已发起"
                        ),
                        Err(e) => tracing::warn!(path = %p.display(), "外部打开失败: {e}"),
                    }
                    if let Err(e) = opened {
                        return Err(e.to_string());
                    }
                    // 写回登记（**只在外部程序确实起来了之后**）：登记会话 + 挂
                    // 临时文件监听；保存时要么写回档案，要么如实告知写不回
                    //（用户实测 2026-10-16：此前保存**什么都没发生**——静默丢改动）。
                    let _ = edit_tx.try_send(crate::app::external_edit::EditEvent::Arm {
                        name: name.clone(),
                        temp: p.clone(),
                        target: target.clone(),
                    });
                }
                if !failed.is_empty() {
                    tracing::warn!("外部打开：物化失败条目 {failed:?}");
                    return Err(format!(
                        "{} ({})",
                        crate::tr("msg.copy_partial"),
                        failed.join("、")
                    ));
                }
                let _ = &name;
                Ok(())
            }
        };
        // 走统一入口：进度窗（超门槛才弹、失败不关）+ 明细 + 任务中心留痕
        let labels = crate::ui::views::open_progress::OpLabels {
            title_key: "openprogress.title_external",
            running_key: "openprogress.opening_external",
            done_key: "openprogress.opened_external",
            record_kind: "open",
        };
        let path_disp = format!("{}::{}", archive.display(), name);
        run_open_flow(
            cx,
            data.clone(),
            OpenFlow {
                label: name,
                path: path_disp,
                // 系统「打开方式」询问框本身就是可见 UI，不要再叠进度窗
                show_window: !matches!(form_copy, crate::viewmodel::OpenForm::SystemPicker),
                labels,
                // epoch = 0：外部打开**不是换会话**（不动会话，不受顶替）
                epoch: 0,
                toast_on_error: true,
            },
            work,
        )
        .detach();
    }

    /// 双击**容器条目**（内层压缩包）→ 打开成下一层会话（R-GUI2-06）。
    ///
    /// 链路：把该条目从当前档案解到会话临时目录（大条目可能读很久）→
    /// `browse` 内层 → 压栈（当前会话整体进栈，`..`/面包屑可退回）→ 安装内层会话。
    /// **延迟组合层**（`init.cpio.gz` 的 `init.cpio`）走同一入口的另一分支：就地
    /// 展开（内层容器本就在同一档案里，无需物化）。
    ///
    /// 三条纪律（用户口径 2026-09-24）：
    ///
    /// 1. **重活不上 UI 线程、不持 AppData 锁**（UI 每帧读快照，持锁跑几秒 = 界面
    ///    冻住）：锁内只取引擎/条目 id/临时目录，提取与浏览在锁外；延迟层分支把
    ///    `Browser` **摘出** AppData 后展开、再连同结果一次落账；
    /// 2. **弹「正在打开」窗 + 过程明细**（[`run_open_flow`]）：看得到在解什么、
    ///    处理到哪了，并在任务中心留痕；
    /// 3. **原子闸**（[`AppData::session_switch`](crate::app::state::AppData::session_switch)）：
    ///    连点/树+列表双路只放行一次——两次并发切换会各自压栈换会话，落账互相覆盖。
    pub fn open_container(&mut self, entry_path: &str, cx: &mut App) {
        let Some(data) = self.data.clone() else {
            return;
        };
        // 密码门（用户口径 2026-09-28）：目标是**加密条目** + 当前会话档案未缓存
        // 密码 → 先问密码（提交验证通过后由主线程泵重跑本函数，届时带密码读条目）。
        {
            let key = data.lock().ok().and_then(|d| {
                let id = d.main.entry_id_of_path(entry_path)?;
                if !d.main.entry(id)?.encrypted {
                    return None;
                }
                let key = d.main.source_path()?.to_path_buf();
                d.cached_password(&key).is_none().then_some(key)
            });
            if let Some(key) = key {
                arm_password_gate(
                    &data,
                    crate::viewmodel::GateAction::OpenContainer(entry_path.to_string()),
                    key,
                );
                return;
            }
        }
        // 原子闸（见上文第 3 点）。拿不到闸 = 已有会话切换在飞 → 忽略这次触发
        // （用户连点想要的是「快点」，不是「开两次」）。
        let Some(gate) = data.lock().ok().map(|d| d.session_switch.clone()) else {
            return;
        };
        let Some(ticket) = gate.try_acquire() else {
            tracing::info!("进入容器：已有会话切换在飞，忽略重复触发（{entry_path}）");
            return;
        };
        let tx = self.signal_tx.clone();
        let label = entry_path
            .rsplit('/')
            .next()
            .unwrap_or(entry_path)
            .to_string();
        // 任务中心记录里的「输入」：当前会话源档案路径 + 档案内条目路径
        // （进入容器发生在已打开的档案内，此处取会话源即可）
        let record_path = self
            .data
            .as_ref()
            .and_then(|d| {
                d.lock()
                    .ok()
                    .and_then(|d| d.main.source_path().map(|p| p.display().to_string()))
            })
            .map(|p| format!("{p}::{entry_path}"))
            .unwrap_or_else(|| entry_path.to_string());
        // **延迟组合层**（用户口径 2026-09-24）：当前会话是「还没展开的组合档」
        // 且双击的正是那一行内层容器 → 就地展开（不物化条目：内层容器本来就在同
        // 一个档案里）。与嵌套容器同走 `run_open_flow`：打开进度窗 + 任务中心留痕。
        let is_deferred_layer = data
            .lock()
            .ok()
            .and_then(|d| d.main.deferred_layer().map(|l| l.to_string()))
            .is_some_and(|l| l == label);
        if is_deferred_layer {
            // 换会话发起（同目标连点忽略；不同目标顶替并关掉旧窗）
            let src_key = data
                .lock()
                .ok()
                .and_then(|d| d.main.source_path().map(|p| p.display().to_string()))
                .unwrap_or_default();
            let Some(epoch) = start_session_switch(
                cx,
                &data,
                crate::app::state::SwitchTarget {
                    key: format!("{src_key}::{entry_path}"),
                    label: label.clone(),
                },
            ) else {
                return;
            };
            let work = {
                let data = data.clone();
                let entry_path = entry_path.to_string();
                // 闸票随工作闭包走：跑完（成功/失败/panic）随 Drop 放行
                // 闸票被下面的 `move` 闭包**按值捕获**：闭包落地（= 重活结束，
                // 含早退/失败/panic 展开）时 `Drop` 放行——闸的持有期恰好是
                // 「会话正在被改写」的窗口（用户口径 2026-09-24）
                let _ticket = ticket;
                move |detail: &crate::ui::views::open_progress::OpenDetail| -> Result<(), String> {
                    use crate::viewmodel::MainViewModel;
                    detail.set_phase(crate::tr("openprogress.phase_expand_layer"));
                    // ① 锁内（微秒）：把浏览器**摘出** AppData + 引擎/源路径
                    let (engine, browser, src) = {
                        let mut d = data.lock().map_err(|_| "state 锁中毒".to_string())?;
                        let browser = d
                            .main
                            .take_browser()
                            .ok_or_else(|| "尚未打开档案".to_string())?;
                        let src = d
                            .main
                            .source_path()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_default();
                        (d.engine.clone(), browser, src)
                    };
                    // ② **锁外、后台线程**：唯一耗时的一步（装配解码链 → 内层容器
                    //    落 spool → 索引）。此前这一步在锁内跑，界面整体冻住。
                    //    `enter_deferred` 是只读借用（本层延迟态不消费）——于是
                    //    ③ 能把这一层浏览器**原样放回**帧里（见下）。
                    match browser.enter_deferred(engine.registry()) {
                        Ok(child_browser) => {
                            // ③ 锁内落账：先在子 VM 上装好（失败可回退），再一次性换
                            //    会话——不留下「已压栈但会话为空」的半成品
                            let mut child = MainViewModel::new();
                            if let Err(e) = child.install_browser(child_browser, &src) {
                                // 装会话失败：原浏览器放回（延迟态仍在，可重试）
                                if let Ok(mut d) = data.lock() {
                                    d.main.restore_browser(browser);
                                }
                                return Err(hz_reason(&e));
                            }
                            let mut d = data.lock().map_err(|_| "state 锁中毒".to_string())?;
                            // 落账前比对代数（与常规嵌套路径同款）：期间被更新的换
                            // 会话顶替 → 丢弃结果，**且不得**把外层浏览器放进新会话
                            // （它属于旧档案）
                            if !d.session_switch_is_current(epoch) {
                                return Err(SESSION_SUPERSEDED.to_string());
                            }
                            // **外层帧的浏览器必须放回**（用户实测 2026-10-14）：
                            // 不放回的话，退回这一层后 `deferred_layer()` 读到
                            // `None`、`is_open()` 为假——「看不到也进不了父压缩包」，
                            // 且同一行会走按条目物化的兜底路径，把**内层容器的首个
                            // 条目**当成那一行解出来（`bh_auto_ops.itr2.x86_64.tgz`
                            // 双击 `….tar` 行直接进 `ops/`）。
                            d.main.restore_browser(browser);
                            d.begin_nested_session(&entry_path);
                            d.main = child;
                            Ok(())
                        }
                        Err(e) => {
                            // `enter_deferred` 纯读、失败不改本层状态 → 原样还回会话
                            // （可重试，不是死局）
                            if let Ok(mut d) = data.lock() {
                                d.main.restore_browser(browser);
                            }
                            Err(hz_reason(&e))
                        }
                    }
                }
            };
            let task = run_open_flow(
                cx,
                data.clone(),
                OpenFlow {
                    label,
                    path: record_path,
                    show_window: true,
                    labels: crate::ui::views::open_progress::OpLabels::default(),
                    epoch,
                    toast_on_error: true,
                },
                work,
            );
            let data_end = data.clone();
            cx.spawn(async move |_cx| {
                let res = task.await;
                if let Ok(mut d) = data_end.lock() {
                    d.end_session_switch(epoch);
                }
                // 被顶替 → 静默（不发信号、不报错：用户已经在看新档案了）
                if let Some(tx) = tx
                    && res.as_ref().err().map(String::as_str) != Some(SESSION_SUPERSEDED)
                {
                    let _ = tx.try_send(if res.is_ok() {
                        crate::viewmodel::VmSignal::EntriesChanged
                    } else {
                        crate::viewmodel::VmSignal::DialogChanged
                    });
                }
            })
            .detach();
            return;
        }
        // 换会话发起（同目标连点忽略；不同目标顶替并关掉旧窗）
        let src_key = data
            .lock()
            .ok()
            .and_then(|d| d.main.source_path().map(|p| p.display().to_string()))
            .unwrap_or_default();
        let Some(epoch) = start_session_switch(
            cx,
            &data,
            crate::app::state::SwitchTarget {
                key: format!("{src_key}::{entry_path}"),
                label: label.clone(),
            },
        ) else {
            return;
        };
        let work = {
            let data = data.clone();
            let entry_path = entry_path.to_string();
            let _ticket = ticket;
            move |detail: &crate::ui::views::open_progress::OpenDetail| -> Result<(), String> {
                // ① 锁内（微秒级）：引擎 + 当前会话源档案 + 目标条目 + 临时目录
                //    + **会话密码**（加密外层档案读内层容器条目用；克隆后不持锁）
                let (engine, archive, entry_id, area, size_raw, pw) = {
                    let d = data.lock().map_err(|_| "state 锁中毒".to_string())?;
                    let archive = d
                        .main
                        .source_path()
                        .map(|p| p.to_path_buf())
                        .ok_or_else(|| "尚未打开档案".to_string())?;
                    let entry_id = d
                        .main
                        .entry_id_of_path(&entry_path)
                        .ok_or_else(|| format!("条目不存在: {entry_path}"))?;
                    let area = d
                        .temp_area
                        .clone()
                        .ok_or_else(|| "临时目录不可用".to_string())?;
                    let size_raw = d
                        .main
                        .entries()
                        .get(entry_id)
                        .map(|e| e.size_raw)
                        .unwrap_or(0);
                    let pw = d.cached_password(&archive);
                    (d.engine.clone(), archive, entry_id, area, size_raw, pw)
                };
                // ② 锁外：独立 reader 解出内层档案（与 `edit.rs` 的物化同款）。
                //    流经 `CountingReader` ——明细行报「已处理 x / y」（y = 条目声明
                //    原始大小，未知则只报已处理量）
                detail.set_phase(crate::tr("openprogress.phase_extract_inner"));
                detail.set_total(size_raw);
                let reader = engine
                    .registry()
                    .open_reader(
                        hzip_format::Source::File(archive.clone()),
                        &hzip_format::OpenOptions {
                            password: pw.clone(),
                            ..Default::default()
                        },
                    )
                    .map_err(|e| hz_reason(&e))?;
                let stream = reader
                    .read_entry(
                        entry_id,
                        match &pw {
                            Some(p) => hzip_format::PasswordRef::new(p),
                            None => hzip_format::PasswordRef::none(),
                        },
                    )
                    .map_err(|e| hz_reason(&e))?;
                let leaf = entry_path.rsplit('/').next().unwrap_or(&entry_path);
                let safe = hzip_platform::temp_area::EntryNameSafe::sanitize(leaf)
                    .map_err(|e| e.to_string())?;
                let mut stream =
                    crate::ui::views::open_progress::CountingReader::new(stream, detail.clone());
                let temp = area
                    .materialize(&archive, &safe, &mut stream)
                    .map_err(|e| e.to_string())?;
                drop(stream);
                // ③ 锁外：browse 内层（同一个同步 API，同样不进 UI 线程）
                detail.set_phase(crate::tr("openprogress.phase_index_inner"));
                let browser = engine
                    .browse(
                        hzip_format::Source::File(temp.clone()),
                        &hzip_format::OpenOptions {
                            defer_composite: true,
                            ..Default::default()
                        },
                    )
                    .map_err(|e| hz_reason(&e))?;
                // ④ 落账：先在新 VM 上装好（失败可回退），再一次性换栈——不留下
                //    「已压栈但会话为空」的半成品。落账前先比对代数（被顶替则丢弃）
                let mut child = crate::viewmodel::MainViewModel::new();
                child
                    .install_browser(browser, &temp)
                    .map_err(|e| hz_reason(&e))?;
                let mut d = data.lock().map_err(|_| "state 锁中毒".to_string())?;
                if !d.session_switch_is_current(epoch) {
                    return Err(SESSION_SUPERSEDED.to_string());
                }
                d.begin_nested_session(&entry_path);
                d.main = child;
                d.adopt_temp_source(temp);
                Ok(())
            }
        };
        let task = run_open_flow(
            cx,
            data.clone(),
            OpenFlow {
                label,
                path: record_path,
                show_window: true,
                labels: crate::ui::views::open_progress::OpLabels::default(),
                epoch,
                toast_on_error: true,
            },
            work,
        );
        let data_end = data.clone();
        cx.spawn(async move |_cx| {
            let res = task.await;
            if let Ok(mut d) = data_end.lock() {
                d.end_session_switch(epoch);
            }
            // 被顶替 → 静默（不发信号、不报错）
            if let Some(tx) = tx
                && res.as_ref().err().map(String::as_str) != Some(SESSION_SUPERSEDED)
            {
                let _ = tx.try_send(if res.is_ok() {
                    crate::viewmodel::VmSignal::EntriesChanged
                } else {
                    tracing::error!(
                        "进入容器失败: {}",
                        res.as_ref().err().cloned().unwrap_or_default()
                    );
                    crate::viewmodel::VmSignal::DialogChanged
                });
            }
        })
        .detach();
    }

    /// 左栏树单击（`id` = `binding::tree_item_id(level, path)`）。
    ///
    /// 三种目标：① 当前会话的目录 → 进入；② **外层会话**的目录 → 先弹回那层再进；
    /// ③ 内层压缩包（容器节点）→ 先弹回它所在层，再开成下一层会话。层号是必需的
    /// ——同名的 `docs/` 可能内外各有一份。
    pub fn tree_click(&mut self, id: &str, cx: &mut App) {
        let Some((level, path)) = crate::ui::binding::split_tree_id(id) else {
            return;
        };
        let Some(data) = self.data.clone() else {
            return;
        };
        // ① 外层节点：先弹回对应会话（弹栈会恢复那层的当前目录）
        if level > 0
            && let Ok(mut d) = data.lock()
        {
            for _ in 0..level {
                if !d.pop_open_frame() {
                    break;
                }
            }
        }
        // ② 容器节点：它在树里是「能展开的压缩包」——单击 = 进去
        let leaf = path.rsplit('/').next().unwrap_or(path);
        let is_container = !path.is_empty()
            && crate::viewmodel::MainViewModel::is_container_name(leaf)
            && data
                .lock()
                .map(|d| d.main.entry_id_of_path(path).is_some())
                .unwrap_or(false);
        if is_container {
            self.open_container(path, cx);
            return;
        }
        // ③ 目录：进入 + 左树定位（与面包屑/表格双击同一条导航语义）
        let Some(tx) = self.signal_tx.clone() else {
            return;
        };
        if let Ok(mut d) = data.lock() {
            if path.is_empty() {
                d.main.go_root_dir();
            } else {
                d.main.enter_dir(path);
                d.main.expand_to(path);
            }
            d.selected_path = Some(path.to_string());
        }
        let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
    }

    /// 退出嵌套层（该层档案根的 `..`）：恢复外层会话，删掉本层临时档案。
    ///
    /// 同样过会话切换原子闸：`..` 也是会话切换，连点两下不该**连退两层**
    /// （用户口径 2026-09-24：「避免多次触发」）。弹栈本身是微秒级操作，闸票
    /// 就地取就地放。
    pub fn leave_container(&mut self, _cx: &mut App) {
        let Some(data) = self.data.clone() else {
            return;
        };
        let Some(gate) = data.lock().ok().map(|d| d.session_switch.clone()) else {
            return;
        };
        let Some(_ticket) = gate.try_acquire() else {
            tracing::info!("退出嵌套层：已有会话切换在飞，忽略重复触发");
            return;
        };
        let tx = self.signal_tx.clone();
        let popped = data.lock().map(|mut d| d.pop_open_frame()).unwrap_or(false);
        if !popped {
            return; // 顶层会话：无处可退（旧语义 no-op）
        }
        if let Some(tx) = tx {
            let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
        }
    }

    /// 打开档案（T-MENU4：最近文件/工具栏 Open 共用后台流程；T-G52 三段式）。
    ///
    /// 成功 → MRU 记录（失败不记）+ `EntriesChanged`；失败 → busy 复位 +
    /// `DialogChanged`。重活（browse）在后台且**不持锁**（见 [`open_archive_task`]）。
    pub fn open_archive_path(&mut self, cx: &mut App, path: std::path::PathBuf) {
        let data = self.data.clone();
        let tx = self.signal_tx.clone();
        cx.spawn(async move |cx| {
            let Some(data) = data else {
                return;
            };
            let data_open = data.clone();
            let path_open = path.clone();
            // 统一入口：后台 browse + 按需「正在打开」窗（R-GUI2-06）
            let task =
                cx.update(|cx| open_archive_task(cx, data_open.clone(), path_open.clone(), true));
            let res = task.await;
            if let Some(tx) = tx {
                let _ = tx.try_send(if res.is_ok() {
                    crate::viewmodel::VmSignal::EntriesChanged
                } else {
                    crate::viewmodel::VmSignal::DialogChanged
                });
                let _ = tx.try_send(crate::viewmodel::VmSignal::MenuEpochBump);
            }
        })
        .detach();
    }
}

/// 武装密码门 + 回投弹窗刷新信号（无任务态；用户口径 2026-09-28）。
///
/// 门只是**状态**（router），真正的弹窗由主线程泵投影 `UiDialogPrompt::Password`
/// → 提示窗渲染；此处必须回投信号把泵叫醒，否则门武装了却没人渲染。
///
/// 返回 `false` = 已有门在开（重复触发被忽略）。
pub(crate) fn arm_password_gate(
    data: &AppDataHandle,
    action: crate::viewmodel::GateAction,
    key: std::path::PathBuf,
) -> bool {
    let armed = data
        .lock()
        .ok()
        .and_then(|d| d.router.lock().ok().map(|mut r| r.arm_gate(action, key)))
        .unwrap_or(false);
    if armed
        && let Ok(d) = data.lock()
        && let Some(tx) = d.signal_tx.as_ref()
    {
        let _ = tx.try_send(crate::viewmodel::VmSignal::DialogChanged);
    }
    armed
}

/// 图片扩展名集（`[ui].image_viewer` 指定程序的适用范围；小写、无点）。
///
/// 用途（用户口径 2026-09-28：内置查看器停用后）：双击打开时判定该用
/// `[ui].image_viewer`（图片）还是 `[ui].editor`（其余）——两者为空即系统默认。
/// 用扩展名而非魔数：用户指定了查看器时，魔数不认识的图片格式（svg/ico）
/// 也应交给它。
pub(crate) const IMAGE_VIEWER_EXTS: [&str; 10] = [
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "ico", "svg",
];

/// 打开类操作**统一入口**（R-GUI2-06：打开进度窗 + 不卡界面）。
///
/// 四件事一起收口：
///
/// 1. **重活不上 UI 线程、也不占 AppData 锁**：`Engine::browse` 是同步 API
///    （02 §9.10「同步轻量，直接在调用线程执行」）。旧实现要么在意图消费者里
///    直呼（UI 线程冻住），要么在后台线程里**持锁**跑完（UI 每帧读 AppData 快照
///    ⇒ 一样冻）。`work` 在**独立后台线程**（用户口径 2026-09-24：「在后台新线程
///    加载」）上跑，且**调用时未持锁**——重活（browse / 解出内层档案 / 展开延迟层）
///    在闭包内自行安排在锁外，只在落账时短暂加锁。
///    为什么不是 gpui 的后台执行器：这类操作按秒~分钟计、CPU/IO 密集，占共享池
///    会拖住别人的后台任务（文件选择器同样在那儿）。
/// 2. **按需弹「正在打开」窗**：耗时超过 `[jobs].open_show_delay_ms` 才弹（小档案
///    秒开就不闪窗），结束后停留 `[jobs].open_close_delay_ms`（默认 0.1 s）自动关。
///    `show_window = false` 用于「打开只为接着解压」——那儿有任务进度窗，再弹一个
///    打开窗是噪音。
/// 3. **过程明细**：`work` 拿到 [`OpenDetail`] 往里写阶段/字节（「解出内层档案…
///    已处理 1.2 GiB / 3.4 GiB」），窗口渲染侧每 100 ms 读一次——用户口径
///    2026-09-24「增强该操作的可观测性」。
/// 4. 成功/失败都返回给调用方（信号、MRU、失败文案由调用方决定）；
///    同时**在任务中心留痕**（见 `data`）。
///
/// `data`：任务中心落账用的数据面句柄（打开**不是任务**，但要留痕——用户口径
/// 2026-09-24：「这些错误都需要在任务中心记录该操作及操作结果（如果失败，则
/// 需要记录比较明确的失败原因）」）。落账在**流程终局**做（成功与失败都记），
/// 且不依赖调用方 await——调用方 `detach` 掉 Task 也不丢记录。
pub(crate) struct OpenFlow {
    /// 进度窗展示名（文件名 / 条目名）
    pub label: String,
    /// 任务中心记录里的输入（`档案` 或 `档案::条目`）
    pub path: String,
    /// 是否弹「操作进度窗」（`false` = 只为接着解压，别叠两个窗）
    pub show_window: bool,
    /// 窗体文案 + 任务中心种类
    pub labels: crate::ui::views::open_progress::OpLabels,
    /// 换会话代数（[`crate::app::state::AppData::begin_session_switch`]）：
    /// 被顶替后不再开窗、不留痕（`0` = 不属于换会话流程，如「外部打开」）。
    pub epoch: u64,
    /// 快速失败（未弹进度窗）时是否再补一条界面 toast。系统「打开方式」询问框
    /// 走 `show_window=false`（询问框本身就是可见 UI），失败仍必须有出口。
    pub toast_on_error: bool,
}

pub(crate) fn run_open_flow(
    cx: &mut App,
    data: AppDataHandle,
    flow: OpenFlow,
    work: impl FnOnce(&crate::ui::views::open_progress::OpenDetail) -> Result<(), String>
    + Send
    + 'static,
) -> gpui_kit::Task<Result<(), String>> {
    let OpenFlow {
        label,
        path,
        show_window,
        labels,
        epoch,
        toast_on_error,
    } = flow;
    let show_delay =
        std::time::Duration::from_millis(hzip_config::Config::load().jobs.open_show_delay_ms);
    let detail = crate::ui::views::open_progress::OpenDetail::default();
    let (tx, rx) = smol::channel::bounded::<Result<(), String>>(1);
    // `work` 上**独立后台线程**（见上文第 1 点）。线程建不起来时不再退回「UI 线程
    // 跑」——那正是本函数要消灭的东西；直接以失败收场（错误可见、可重试）。
    let detail_work = detail.clone();
    let spawn_err = std::thread::Builder::new()
        .name("hzip-open".into())
        .spawn(move || {
            let res = work(&detail_work);
            let _ = tx.send_blocking(res);
        })
        .err()
        .map(|e| format!("无法创建打开线程: {e}"));
    cx.spawn(async move |cx| {
        if let Some(e) = spawn_err {
            return Err(e);
        }
        // 门槛竞速：先完成 → 不弹窗；门槛先到 → 弹窗再等结果。
        // 用通道而非「取消 unblock future」：重活已经在后台跑着，取消 future
        // 只会把结果丢掉（后台线程不会因此停下）。
        let first = smol::future::or(async { Some(rx.recv().await) }, async {
            smol::Timer::after(show_delay).await;
            None
        })
        .await;
        let closed = |_| Err("打开任务通道关闭".to_string());
        // 被更新的换会话顶替（或流程本身已过期）→ 不再弹窗（旧档案的操作不该在
        // 用户已经打开新档案之后还冒出来）
        let still_current = || {
            epoch == 0
                || data
                    .lock()
                    .map(|d| d.session_switch_is_current(epoch))
                    .unwrap_or(false)
        };
        let (res, key) = match first {
            Some(r) => (r.unwrap_or_else(closed), None),
            None if !show_window || !still_current() => {
                (rx.recv().await.unwrap_or_else(closed), None)
            }
            None => {
                let key = cx.update(|cx| {
                    crate::ui::views::open_progress::open(cx, label.clone(), detail, labels.clone())
                });
                (rx.recv().await.unwrap_or_else(closed), Some(key))
            }
        };
        // 被顶替：静默收尾（关掉自己的窗、不写任务中心、不报错）
        if res.as_ref().err().map(String::as_str) == Some(SESSION_SUPERSEDED) {
            if let Some(key) = key {
                cx.update(|cx| crate::ui::views::open_progress::discard(cx, key));
            }
            return Err(SESSION_SUPERSEDED.to_string());
        }
        let had_window = key.is_some();
        if let Some(key) = key {
            let err = res.as_ref().err().cloned();
            cx.update(|cx| crate::ui::views::open_progress::finish(cx, key, err));
        }
        // 任务中心留痕（成功/失败都记；失败带明确原因）
        let (name, full_path) = (label.clone(), path.clone());
        cx.update(|_| {
            if let Ok(d) = data.lock()
                && let Ok(mut jobs) = d.jobs.lock()
            {
                jobs.record_op(labels.record_kind, &name, &full_path, &res);
            }
        });
        // 快速失败（没弹出进度窗 ⇒ 任务中心那行用户看不到）→ 界面 toast 兜底：
        // 「点了没反应」是不可接受的（用户口径 2026-09-28 实测：外部打开失败
        // 只进了任务中心，用户以为没生效）。
        if let Err(reason) = &res
            && !had_window
            && toast_on_error
        {
            let title = crate::tr("openprogress.failed_title");
            let body = format!("{name}：{reason}");
            cx.update(|_| {
                if let Ok(d) = data.lock()
                    && let Some(tx) = d.signal_tx.as_ref()
                {
                    let _ = tx.try_send(crate::viewmodel::VmSignal::Notify(
                        crate::viewmodel::pump::NotifyEvent {
                            title,
                            body,
                            force_system: false,
                        },
                    ));
                }
            });
        }
        res
    })
}

/// 打开档案（磁盘路径）——[`run_open_flow`] 的常规用法：browse + 落账 + 记 MRU。
///
/// **不分入口**：无论工具栏/最近文件/资源管理器双击右键/参数窗，成功打开都记
/// 「最近打开」（用户口径：只要能打开档案就要在最近记录里，方便下次直达）。
/// 档案内的**子压缩包**不走本函数（进容器 = 嵌套会话），故不会进记录。
///
/// 成功后**把浏览页拉回前台**（用户实测缺陷 2026-09-23：关掉浏览页签后，
/// 「再打开一次」看不到页签）。此处不依赖快照批次——同一档案重开时快照可能
/// 逐字不变，页签的回归必须由**打开动作本身**保证。
pub(crate) fn open_archive_task(
    cx: &mut App,
    data: AppDataHandle,
    path: std::path::PathBuf,
    show_window: bool,
) -> gpui_kit::Task<Result<(), String>> {
    let label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    // 换会话发起（用户口径 2026-09-24）：**同目标**连点忽略；**不同目标**顶替
    //（旧流程落账前会发现代数已变而丢弃结果，旧进度窗同时关掉）。旧实现用「原子闸
    // + 忽略」——于是「打开 A 后立刻打开 B」会被静默丢掉，屏幕上只剩 A 的进度窗
    // 在转（用户实测）。
    let Some(epoch) = start_session_switch(
        cx,
        &data,
        crate::app::state::SwitchTarget {
            key: path.display().to_string(),
            label: label.clone(),
        },
    ) else {
        return cx.spawn(async move |_cx| Err::<(), String>(SESSION_SUPERSEDED.to_string()));
    };
    // 重活闭包：锁内只取 engine（微秒级），browse 在锁外，落账再短暂加锁。
    let work = {
        let data = data.clone();
        let path = path.clone();
        move |_detail: &crate::ui::views::open_progress::OpenDetail| {
            // 会话密码缓存（用户口径 2026-09-28）：该档案此前输过密码 → 直接带密码
            // 打开（头加密档案不重复问；未缓存 = None，行为与旧实现一致）
            let (engine, cached_pw) = data
                .lock()
                .map(|d| (d.engine.clone(), d.cached_password(&path)))
                .map_err(|_| "state 锁中毒".to_string())?;
            let browser = match engine.browse(
                hzip_format::Source::File(path.clone()),
                &hzip_format::OpenOptions {
                    password: cached_pw,
                    // GUI 显示路径：组合档延迟展开（用户口径 2026-09-24）
                    defer_composite: true,
                    ..Default::default()
                },
            ) {
                Ok(b) => b,
                Err(hzip_error::HzipError::Password { .. }) => {
                    // 头加密档案（设计稿 11 §4.3 浏览密码门）：武装门 → 下一泵
                    // 投影密码弹窗；门内带密码重开并落账（MRU/会话/密码缓存在门内完成）。
                    let _ = data.lock().ok().and_then(|d| {
                        d.router.lock().ok().map(|mut r| {
                            r.arm_gate(crate::viewmodel::GateAction::OpenArchive, path.clone())
                        })
                    });
                    return Err("需要输入密码（头加密档案）".to_string());
                }
                Err(e) => return Err(hz_reason(&e)),
            };
            let mut d = data.lock().map_err(|_| "state 锁中毒".to_string())?;
            // 落账前比对代数：已被更新的换会话顶替 → 丢弃结果（不装会话、不记 MRU；
            // 否则「老压缩包的内容会盖掉刚打开的新档案」——用户实测）
            if !d.session_switch_is_current(epoch) {
                return Err(SESSION_SUPERSEDED.to_string());
            }
            // 顶层打开：清空嵌套会话栈（旧档案的内层层级/临时文件不该留到新档案里）
            d.reset_nested_sessions();
            d.main
                .install_browser(browser, &path)
                .map_err(|e| e.to_string())?;
            d.archive = Some(path.clone());
            d.record_mru_open(&path);
            Ok(())
        }
    };
    let task = run_open_flow(
        cx,
        data.clone(),
        OpenFlow {
            label,
            path: path.display().to_string(),
            show_window,
            labels: crate::ui::views::open_progress::OpLabels::default(),
            epoch,
            // 打开档案：失败已有进度窗/任务中心可见，无需再补 toast
            toast_on_error: false,
        },
        work,
    );
    let data_end = data.clone();
    cx.spawn(async move |_cx| {
        let res = task.await;
        if let Ok(mut d) = data_end.lock() {
            d.end_session_switch(epoch);
        }
        res
    })
}
