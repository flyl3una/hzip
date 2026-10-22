//! 剪贴板操作（复制/粘贴）+ 过滤框聚焦 + 粘贴源展开——自 `shell/mod.rs` 拆分。

use gpui_kit::prelude::*;
use gpui_kit::{App, Window};

use super::app_shell::AppShell;
use super::errors::hz_reason;
use super::open_flow::{OpenFlow, run_open_flow};

impl AppShell {
    /// 复制选中条目到剪贴板（T-MENU8；B3 降级线）。
    ///
    /// ① 选中条目物化到进程级复制区（稳定路径；TC-SEC-015 净化）；
    /// ② 写**内部剪贴板**（hzip 内复制→粘贴闭环）；
    /// ③ 尽力写系统剪贴板：gpui Windows 侧 `ExternalPaths` 是空臂（Spike 结论），
    ///    故仅单文件时以**文本路径**兜底（跨应用粘贴文件列表挂账二期）。
    pub fn copy_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.data.clone() else {
            return;
        };
        // ① 锁内（微秒）：档案 + 选中条目 id
        let prep = data.lock().ok().and_then(|d| {
            let archive = d.archive.clone()?;
            let ids = d.main.effective_selection_ids();
            if ids.is_empty() {
                return None;
            }
            Some((archive, ids))
        });
        let Some((archive, ids)) = prep else {
            return;
        };
        // 防重闸：连点「复制」只放行一次（用户口径 2026-09-24）
        let Some(gate) = data.lock().ok().map(|d| d.session_switch.clone()) else {
            return;
        };
        let Some(ticket) = gate.try_acquire() else {
            tracing::info!("复制：已有重活在飞，忽略重复触发");
            return;
        };
        // ② **后台线程 + 进度窗**：物化要把选中条目整条解出来（大条目秒级到分钟级）
        //    ——旧实现在 UI 线程持 AppData 锁跑完整物化：主界面无响应且毫无提示。
        // 全量物化路径（目录壳 + 全部后代；契约 C 产物）——完成回调写系统
        // 剪贴板 CF_HDROP 用；降级文本路径取首元素（原 first_path 面收拢于此）。
        let first_path_sys =
            std::sync::Arc::new(std::sync::Mutex::new(None::<Vec<std::path::PathBuf>>));
        let all_paths_work = first_path_sys.clone();
        let count = ids.len();
        let archive_disp = archive.display().to_string();
        let work = {
            let data = data.clone();
            let archive = archive.clone();
            move |detail: &crate::ui::views::open_progress::OpenDetail| -> Result<(), String> {
                // 闸票被 `move` 闭包按值捕获：持有期 = 重活全程（复制不是换会话，
                // 故用原子闸而不是会话代数）
                let _ticket = ticket;
                detail.set_phase(crate::tr("openprogress.phase_materialize"));
                // 锁内摘出浏览器 + 会话密码 → 锁外物化 → 锁内放回（同 `open_container` 三段式）
                let (browser, pw) = {
                    let mut d = data.lock().map_err(|_| "state 锁中毒".to_string())?;
                    let pw = d.cached_password(&archive);
                    let browser = d
                        .main
                        .take_browser()
                        .ok_or_else(|| crate::tr("msg.copy_no_archive"))?;
                    (browser, pw)
                };
                let res = crate::viewmodel::MainViewModel::materialize_in(
                    &browser,
                    &ids,
                    &archive,
                    pw.as_ref(),
                );
                if let Ok(mut d) = data.lock() {
                    d.main.restore_browser(browser);
                }
                match res {
                    Ok((paths, failed)) => {
                        if !failed.is_empty() {
                            tracing::warn!("复制部分条目失败: {failed:?}");
                        }
                        if paths.is_empty() {
                            return Err(crate::tr("msg.copy_nothing"));
                        }
                        if let Ok(mut p) = all_paths_work.lock() {
                            *p = Some(paths.clone());
                        }
                        // 内部剪贴板（闭环；进程级资源，不依赖 gpui，可在后台写）
                        hzip_platform::clipboard::set_internal(paths.clone());
                        tracing::info!(count = paths.len(), "已复制到内部剪贴板");
                        if failed.is_empty() {
                            Ok(())
                        } else {
                            Err(format!(
                                "{} ({})",
                                crate::tr("msg.copy_partial"),
                                failed.join("、")
                            ))
                        }
                    }
                    Err(e) => Err(hz_reason(&e)),
                }
            }
        };
        let labels = crate::ui::views::open_progress::OpLabels {
            title_key: "openprogress.title_copy",
            running_key: "openprogress.copying",
            done_key: "openprogress.copied",
            record_kind: "copy",
        };
        let label = crate::tr_with("copy.label", &[("n", &count.to_string())]);
        let task = run_open_flow(
            cx,
            data.clone(),
            OpenFlow {
                label,
                path: archive_disp,
                show_window: true,
                labels,
                // epoch = 0：复制不是换会话（不改会话，不受顶替影响）
                epoch: 0,
                toast_on_error: true,
                immediate_show: false,
            },
            work,
        );
        cx.spawn(async move |_this, cx| {
            let res = task.await;
            // 系统剪贴板（T-MENU8 双向粘贴 P0）：自绘 win32 写 CF_HDROP 文件列表
            // （无 App 依赖，后台线程可调）。成功 → 外部粘贴可用，不再写文本；
            // 失败（非 Windows / 剪贴板被占用）→ 退回旧**单文件文本路径**兜底
            // （跨应用文本粘贴可见路径；完整文件列表兜底挂账二期）。
            // `paths` 含目录壳与全部后代（契约 C 物化产物）原样传入。
            if res.is_ok() {
                let paths = first_path_sys.lock().ok().and_then(|g| g.clone());
                let Some(paths) = paths else {
                    return;
                };
                if hzip_platform::clipboard::set_system_file_list(&paths) {
                    tracing::info!(count = paths.len(), "已写系统剪贴板文件列表");
                    return;
                }
                // 降级：单文件文本路径（gpui 侧需 App 上下文）
                if let Some(one) = paths.first()
                    && one.exists()
                {
                    cx.update(|cx: &mut App| {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                            one.to_string_lossy().into_owned(),
                        ));
                    });
                }
            }
        })
        .detach();
        let _ = window;
    }

    /// 从剪贴板粘贴文件到当前档案（T-MENU8；复用 T-MENU6 提交管线）。
    ///
    /// 来源优先级：内部剪贴板（hzip 内复制，闭环）→ 系统剪贴板 CF_HDROP
    /// （外部资源管理器复制；gpui 平台层已实现读取）。两处皆空 → 静默
    /// （菜单项已按 `has_clipboard` 置灰，正常路径到不了这里）。
    ///
    /// 目标名（双向粘贴 P0）：`current_dir` 非空时 `{current_dir}/{rel}`，根时
    /// `{rel}`；文件源 `rel` = 文件名，**目录源**经 [`collect_paste_entries`]
    /// walkdir 递归展开——源目录本身的名字作为一层目录进目标名，后代按相对
    /// 结构落位（`D:/pics` + `a.png` + `sub/b.png` → `cur/pics/a.png`、
    /// `cur/pics/sub/b.png`）。
    pub fn paste_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let mut sources = hzip_platform::clipboard::internal_paths();
        if sources.is_empty() {
            // 外部来源：gpui 剪贴板 `ExternalPaths`（Windows CF_HDROP 读取）
            if let Some(item) = cx.read_from_clipboard() {
                for entry in item.entries() {
                    if let gpui_kit::ClipboardEntry::ExternalPaths(paths) = entry {
                        sources.extend(paths.paths().iter().cloned());
                    }
                }
            }
        }
        if sources.is_empty() {
            return;
        }
        // 会话不可写 = 粘贴这种**改写档案**的操作当场拒绝（见
        // `refuse_readonly_write`）。菜单已置灰，这里是绕过入口的兜底。
        if self.refuse_readonly_write() {
            return;
        }
        let Some(data) = self.data.clone() else {
            return;
        };
        // 目录源 walkdir 递归展开（文件清单 + 源相对名）——纯文件系统 IO、
        // 无 App 依赖。既有粘贴路径本就在 UI 线程同步组装（文件多选秒回），
        // 目录展开沿用同一时机：典型剪贴板源是小集合，walkdir 常数开销可忽略；
        // 展开不持 AppData 锁。
        let expanded = expand_paste_sources(&sources);
        // 组装 Append 变更集（同名冲突统计，按目标全名比较）→ 暂存 → 复用
        // T-MENU6 确认对话框
        let (changes, conflicts, archive) = {
            let Ok(d) = data.lock() else {
                return;
            };
            // 落点 = 本会话源档案（嵌套会话 = 内层物化档：变更 id 属内层条目空间）
            let Some(archive) = d.edit_target() else {
                return;
            };
            // 粘贴落点 = 当前浏览目录（与「添加」同口径；用户口径 2026-09-29）
            // 判存闭包直接借用 VM 条目名（审计 B18：不克隆整表）
            let cur = d.main.current_dir().to_string();
            let entries = collect_paste_entries(&cur, &expanded);
            let (cs, conflicts) = crate::app::state::append_changeset(&entries, |n| {
                d.main.entries().iter().any(|e| e.name == n)
            });
            (cs, conflicts, archive)
        };
        if changes.changes.is_empty() {
            return;
        }
        let count = changes.changes.len();
        let staged = if let Ok(mut d) = data.lock() {
            d.stage_edit_batch(archive, changes)
        } else {
            return;
        };
        if let Err(msg) = staged {
            // 暂存被拒（组装与提交之间下钻进了内层容器）：不弹确认框，直说原因
            self.notify(crate::tr("edit.commit.failed"), msg);
            return;
        }
        let on = self.confirm_handler();
        window.open_dialog(cx, move |_, _window, cx| {
            crate::ui::dialogs::add_files_confirm_dialog(cx, count, conflicts, {
                let on = on.clone();
                move |e| on(e)
            })
        });
    }

    /// 聚焦浏览页过滤框（T-MENU1 菜单「查找文件」入口；Ctrl+F 键绑定同路径）。
    ///
    /// BrowseHold 是 Global——浏览页尚未渲染过（未打开档案）时 hold 不存在，
    /// 静默 no-op（菜单项在无档案时已 disabled，正常路径到不了这里）。
    pub fn focus_browse_filter(&mut self, window: &mut Window, cx: &mut App) {
        use gpui_kit::Focusable as _;
        if let Some(hold) = crate::ui::views::browse::filter_hold_in(cx) {
            let fh = hold.read(cx).filter_input.read(cx).focus_handle(cx);
            fh.focus(window, cx);
        }
    }
}

// —— 粘贴源展开（T-MENU8 双向粘贴 P0；纯函数面，单测锚点） ——

/// 单个粘贴源的展开产物：文件清单 + 各自**源相对名**（两向量逐位对齐）。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PasteSource {
    /// 文件绝对路径（walkdir 深度优先 + 文件名排序，序可复现）
    pub files: Vec<std::path::PathBuf>,
    /// 源相对名（`/` 连接；文件源 = 文件名，目录源 = 「源目录名/内部结构」）
    pub rels: Vec<String>,
}

/// 粘贴源清单展开（T-MENU8 双向粘贴 P0）。
///
/// 文件源直收（相对名 = 文件名）；**目录源**经 walkdir 递归展开，相对名相对
/// **源目录的父目录**取——源目录名自然成为相对结构首段（`D:/pics/sub/b.png`
/// 相对 `D:/` → `pics/sub/b.png`），落位时经 `paste_target_name` 拼上
/// `current_dir`。**空目录（无任何文件后代）跳过**：档案条目以文件流为载体，
/// 纯空目录无内容可写；资源管理器复制的空目录粘贴后不出现是可接受降级
/// （编辑管线 Append 无空流通道，为其扩协议超出本任务范围）。
pub(crate) fn expand_paste_sources(sources: &[std::path::PathBuf]) -> Vec<PasteSource> {
    let mut out = Vec::new();
    for src in sources {
        if src.is_dir() {
            let mut src_entry = PasteSource::default();
            for entry in walkdir::WalkDir::new(src)
                .follow_links(false)
                .sort_by_file_name()
            {
                let Ok(entry) = entry else {
                    continue;
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                // 相对名相对源目录的父目录（见函数文档）；父目录不存在
                // （根路径源，实际到不了）退化为相对源目录本身。
                let base = src.parent().unwrap_or(src.as_path());
                let Ok(rel) = entry.path().strip_prefix(base) else {
                    continue;
                };
                let rel = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                src_entry.files.push(entry.path().to_path_buf());
                src_entry.rels.push(rel);
            }
            // 空目录（无文件后代）跳过——见函数文档「可接受降级」。
            if !src_entry.files.is_empty() {
                out.push(src_entry);
            }
        } else if src.is_file() {
            let Some(name) = src.file_name() else {
                continue;
            };
            out.push(PasteSource {
                files: vec![src.clone()],
                rels: vec![name.to_string_lossy().into_owned()],
            });
        }
    }
    out
}

/// 展开产物 → `(源路径, 档案内目标名)` 二元组（`append_changeset` 输入）。
///
/// 目标名 = `{current_dir}/{rel}`（current_dir 非空）或 `{rel}`（根）——
/// 组装走 [`crate::app::state::paste_target_name`] 纯函数（`/` 分隔，与档案
/// 内条目名口径一致；不用 `Path::join` 防 Windows `\`）。
pub(crate) fn collect_paste_entries(
    current_dir: &str,
    sources: &[PasteSource],
) -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();
    for s in sources {
        for (f, rel) in s.files.iter().zip(s.rels.iter()) {
            out.push((
                f.clone(),
                crate::app::state::paste_target_name(current_dir, rel),
            ));
        }
    }
    out
}
