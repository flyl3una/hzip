//! 会话保活/重建/换会话代数（密码门重开、行集重建）——自 `shell/mod.rs` 拆分。

use gpui_kit::App;

use super::runtime::AppDataHandle;

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
