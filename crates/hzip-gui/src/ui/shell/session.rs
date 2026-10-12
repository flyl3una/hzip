//! 会话保活/重建/换会话代数（密码门重开、行集重建）——自 `shell/mod.rs` 拆分。

use gpui_kit::App;

use super::errors::hz_reason;
use super::runtime::AppDataHandle;

/// 浏览密码门**密码提交** → 后台重开（返回 `true` = 已接管，调用方别再走同步路径）。
///
/// 为什么单独抽出来（用户口径 2026-09-24）：门内重开要 `engine.browse`（头加密档案
/// 解密头 + 建索引），大档案秒级；旧实现在 `AppData::consume_dialog_event` 里同步跑，
/// **且已持 AppData 锁** —— 主界面每帧渲染要那把锁 ⇒ 界面无响应且毫无提示。
///
/// 三段式（与 `open_container` 同款）：① 锁内取路径/引擎并放行 ② 后台线程锁外
/// `browse` ③ 锁内装会话（`install_browser` + 记录 MRU + 门状态收口），最后信号回投
/// `EntriesChanged` 刷新界面、任务中心留痕（成功/失败都记）。
///
/// 返回 `false` = 当前没有武装的浏览门（事件按原路走任务分支）。
pub(crate) fn begin_browse_gate_reopen(data: &AppDataHandle, pw: &str) -> bool {
    let Some(path) = data
        .lock()
        .ok()
        .and_then(|d| d.router.lock().ok().and_then(|r| r.gate_key()))
    else {
        return false;
    };
    let Some((tx, engine)) = data
        .lock()
        .ok()
        .map(|d| (d.signal_tx.clone(), d.engine.clone()))
    else {
        return false;
    };
    // 密码持有:一律 `Zeroizing`（设计稿 11 §5.4；线程结束时清零）
    let pw = zeroize::Zeroizing::new(pw.to_owned());
    let data = data.clone();
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let record_path = path.display().to_string();
    std::thread::Builder::new()
        .name("hzip-gate-open".into())
        .spawn(move || {
            // ② 锁外：带密码重开（头解密 + 索引）
            let opts = hzip_format::OpenOptions {
                password: Some(pw.clone()),
                // 显示路径：组合档延迟展开（与 `open_archive_task` 同口径）
                defer_composite: true,
                ..Default::default()
            };
            let opened = engine.browse(hzip_format::Source::File(path.clone()), &opts);
            // ③ 锁内：装会话 / 记门状态（错密码 → 门内重问，达上限解门）
            let outcome: Result<(), String> = match opened {
                Ok(browser) => match data.lock() {
                    Ok(mut d) => match d.main.install_browser(browser, &path) {
                        Ok(()) => {
                            d.archive = Some(path.clone());
                            d.record_mru_open(&path);
                            // 顶层打开：清空嵌套会话栈（旧档案的内层层级不该留到新档案）
                            d.reset_nested_sessions();
                            // 会话密码缓存（用户口径 2026-09-28）：解头成功即密码正确
                            // ⇒ 缓存下来，此后该档案打开文件/解压不再询问
                            d.remember_password(&path, pw);
                            if let Ok(mut r) = d.router.lock() {
                                r.gate_disarm();
                            }
                            Ok(())
                        }
                        Err(e) => {
                            if let Ok(mut r) = d.router.lock() {
                                r.gate_disarm();
                            }
                            Err(hz_reason(&e))
                        }
                    },
                    Err(_) => Err("state 锁中毒".to_string()),
                },
                Err(hzip_error::HzipError::Password { .. }) => {
                    // 错密码：门内重问（attempts+1；达上限解门关窗）
                    let retryable = data
                        .lock()
                        .ok()
                        .and_then(|d| d.router.lock().ok().map(|mut r| r.gate_fail()))
                        .unwrap_or(false);
                    if !retryable {
                        tracing::info!("浏览密码重试达上限，关闭密码窗");
                    }
                    Err("密码错误".to_string())
                }
                Err(e) => {
                    if let Ok(d) = data.lock()
                        && let Ok(mut r) = d.router.lock()
                    {
                        r.gate_disarm();
                    }
                    Err(hz_reason(&e))
                }
            };
            if let Ok(d) = data.lock()
                && let Ok(mut jobs) = d.jobs.lock()
            {
                jobs.record_op("open", &name, &record_path, &outcome);
            }
            if let Some(tx) = tx {
                let _ = tx.try_send(if outcome.is_ok() {
                    crate::viewmodel::VmSignal::EntriesChanged
                } else {
                    crate::viewmodel::VmSignal::DialogChanged
                });
                let _ = tx.try_send(crate::viewmodel::VmSignal::MenuEpochBump);
            }
        })
        .ok();
    true
}

/// 后台重开浏览会话（编辑释放句柄后的保活；审计 B3）。
///
/// 旧实现：泵每 wake 在 **UI 线程 + 持 AppData 锁**里跑完整 `engine.browse`
/// —— 编辑（添加/删除/粘贴）完成后必现卡顿，并同时堵住渲染帧。
///
/// 现形态（与 `open_container` 同款三段式 + 在飞标志）：
/// ① 锁内取路径/引擎 → ② **专用线程**锁外 `browse` → ③ 锁内 `finish_browse_reopen`
/// （装会话 + 重建行；成败都清在飞标志）→ ④ 信号回投 `BrowseRowsRebuilt`。
///
/// 标志与去重由 [`crate::app::state::AppData::browse_session_reopen_request`] 负责：
/// 调用方先拿到 `Some(path)` 才调本函数。
pub(crate) fn spawn_browse_reopen(data: AppDataHandle, path: std::path::PathBuf) {
    let data_thread = data.clone();
    let spawned = std::thread::Builder::new()
        .name("hzip-reopen".into())
        .spawn(move || {
            let data = data_thread;
            let (engine, cached_pw) = {
                let d = match data.lock() {
                    Ok(d) => d,
                    Err(_) => {
                        // 锁中毒：清在飞，别把保活锁死
                        if let Ok(mut d) = data.lock() {
                            d.finish_browse_reopen(Err(hzip_error::HzipError::Unsupported {
                                what: "hz-gui-0001 state-lock-poisoned".into(),
                            }));
                        }
                        return;
                    }
                };
                // 会话密码缓存（用户口径 2026-09-29）：编辑前该档案已解锁过 →
                // 带密码重开（头加密档案否则重开必败，界面会空掉）
                (d.engine.clone(), d.cached_password(&path))
            };
            // ② 锁外重开（与 `open_archive_task` 同口径：组合档延迟展开）
            let res = engine
                .browse(
                    hzip_format::Source::File(path.clone()),
                    &hzip_format::OpenOptions {
                        password: cached_pw,
                        defer_composite: true,
                        ..Default::default()
                    },
                )
                .map(|browser| {
                    // ③ 锁内装会话（失败按失败收口）。
                    //    先校验「会话仍是关着的」：保活重开过程里用户可能已经打开了
                    //    别的档案（那时 `is_open` 为真）——此时**丢弃**本次重开结果，
                    //    绝不能把旧档案盖回去（用户实测 2026-09-24「不应被老内容干扰」）。
                    match data.lock() {
                        Ok(mut d) => {
                            if d.main.is_open() {
                                // 会话已被别的操作恢复（例如用户打开了新档案）：
                                // 丢弃本次重开结果，**不能**把旧档案盖回去
                                //（用户实测 2026-09-24「不应被老内容干扰」）。
                                tracing::info!(
                                    "保活重开：会话已被其它操作恢复，丢弃本次结果（{path:?}）"
                                );
                                Ok(())
                            } else {
                                let installed = d.main.install_browser(browser, &path);
                                match installed {
                                    Ok(()) => {
                                        d.archive = Some(path.clone());
                                        Ok(())
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                        }
                        Err(_) => Err(hzip_error::HzipError::Unsupported {
                            what: "hz-gui-0001 state-lock-poisoned".into(),
                        }),
                    }
                });
            let outcome = match res {
                Ok(inner) => inner,
                Err(e) => Err(e),
            };
            if let Ok(mut d) = data.lock() {
                d.finish_browse_reopen(outcome);
            }
            if let Ok(d) = data.lock()
                && let Some(tx) = d.signal_tx.clone()
            {
                let _ = tx.try_send(crate::viewmodel::VmSignal::BrowseRowsRebuilt);
            }
        });
    if let Err(e) = spawned {
        tracing::error!("重开线程创建失败: {e}");
        // 建线程都失败了：清在飞标志，避免保活永久停摆
        if let Ok(mut d) = data.lock() {
            d.finish_browse_reopen(Err(hzip_error::HzipError::Unsupported {
                what: format!("hz-gui-0002 spawn-reopen-failed: {e}").into(),
            }));
        }
    }
}

/// 后台重建浏览行（O(行集) 派生；审计 B5）。
///
/// 泵批内只**请求**：多窗口 + 多信号可能同 tick 请求多次，故用进程内
/// `rows_rebuild_in_flight` 标志去重——在飞时直接返回（重建是幂等的，跑一次
/// 拿最新状态即可）。完成后投 `BrowseRowsRebuilt` 让下一次泵批重导快照。
pub(crate) fn spawn_rows_rebuild(data: AppDataHandle) {
    let flag = match data.lock() {
        Ok(d) => d.rows_rebuild_in_flight(),
        Err(_) => return,
    };
    if flag.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return; // 已有重建在飞
    }
    let flag_thread = flag.clone();
    let spawned = std::thread::Builder::new()
        .name("hzip-rows".into())
        .spawn(move || {
            if let Ok(mut d) = data.lock() {
                d.rebuild_browse_rows();
            }
            flag_thread.store(false, std::sync::atomic::Ordering::Release);
            if let Ok(d) = data.lock()
                && let Some(tx) = d.signal_tx.clone()
            {
                let _ = tx.try_send(crate::viewmodel::VmSignal::BrowseRowsRebuilt);
            }
        });
    if let Err(e) = spawned {
        tracing::error!("行重建线程创建失败: {e}");
        flag.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// 发起一次换会话（统一入口）：同目标连点 → `None`（忽略本次触发）；
/// 不同目标 → 关掉被顶替目标的「正在打开」窗，返回本次代数。
///
/// 见 [`crate::app::state::AppData::begin_session_switch`]（用户实测 2026-09-24：
/// 「打开了 cpio.gz 没展开，再打开 qcow2.gz，却冒出还在打开 cpio 的进度窗」）。
pub(crate) fn start_session_switch(
    cx: &mut App,
    data: &AppDataHandle,
    target: crate::app::state::SwitchTarget,
) -> Option<u64> {
    let start = data
        .lock()
        .ok()
        .map(|mut d| d.begin_session_switch(target))?;
    match start {
        crate::app::state::SwitchStart::Duplicate => {
            tracing::info!("换会话：同目标已有在飞流程，忽略重复触发");
            None
        }
        crate::app::state::SwitchStart::Started { epoch, superseded } => {
            if let Some(label) = superseded {
                // 旧目标的进度窗立刻关掉：它已经过期，留着只会让用户以为「还在打开
                // 上一个档案」
                crate::ui::views::open_progress::close_by_label(cx, &label);
            }
            Some(epoch)
        }
    }
}

/// 「本次换会话已被更新的请求顶替」哨兵（用户实测 2026-09-24 的跨操作串扰）。
///
/// 后台流程落账前比对 [`crate::app::state::AppData::session_switch_is_current`]：
/// 不等即丢弃结果，并以本哨兵收场——`run_open_flow` 认它 = **静默收尾**（关掉自己
/// 的进度窗、不写任务中心、不报错）：「新开一个档案」不该被旧档案的操作干扰。
pub(crate) const SESSION_SUPERSEDED: &str = "hz-gui-0003 session-superseded";
