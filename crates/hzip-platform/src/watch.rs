//! 文件变更监听（notify 封装）+ 去抖（02 §5.3.6；TC-CORE-019）。
//!
//! notify 8.x：`notify::recommended_watcher`（Windows → `ReadDirectoryChangesW`、
//! Linux → `inotify`、macOS → `FSEvents`，自动选择）。对「具体文件」监听，
//! 内部退化为监听其父目录 + 路径过滤（notify 文件级 watch 在部分后端对
//! rename/close_write 不可靠，02 §5.3.6）。
//!
//! 事件映射：只保留 `EventKind::Modify(_) | Access(Close(Write))`；
//! 去抖窗口 [`crate::consts::EDIT_DEBOUNCE_MS`]（500 ms）：窗口内多次保存只触发
//! 一次回调（TC-CORE-019）。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::consts::EDIT_DEBOUNCE_MS;

/// 去抖回调类型（Arc 包裹以便跨线程共享；type 别名消解 type-complexity lint）。
type DebounceCb = Arc<Mutex<Box<dyn Fn(PathBuf) + Send>>>;

enum Msg {
    /// 去抖事件（路径已过滤为目标文件）
    Changed(PathBuf),
    /// 停止信号（Drop 时发送）
    Stop,
}

/// 文件变更监听句柄（02 §5.3.6）。
///
/// 去抖线程在 `Drop` 时随句柄释放（句柄随 TempArea 生命周期自动释放）。
/// 回调异常捕获记日志，不允许 panic 穿越。
pub struct FileWatch {
    tx: Sender<Msg>,
    _thread: JoinHandle<()>,
    _watcher: RecommendedWatcher,
}

impl FileWatch {
    /// 监听具体文件（02 §5.3.6；notify 文件级 watch 在部分后端不可靠 →
    /// 退化为监听父目录 + 路径过滤）。
    ///
    /// `cb` 在去抖窗口（500 ms）内合并后触发，参数为变化文件路径。
    pub fn new(file: &Path, cb: Box<dyn Fn(PathBuf) + Send>) -> io::Result<FileWatch> {
        let parent = file
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "watch 目标无父目录"))?
            .to_path_buf();
        let file = file.to_path_buf();
        let (tx, rx): (Sender<Msg>, Receiver<Msg>) = std::sync::mpsc::channel();

        // macOS FSEvents 回报**规范化**路径（`/var` → `/private/var`，TMPDIR 即此形态），
        // 而调用方持有原路径。两种形态都接受，回调一律回投调用方的原路径（平台路径
        // 形态细节不外泄，红线 5）；Windows/Linux 及已规范化的路径行为不变。
        let canon_file = parent
            .canonicalize()
            .ok()
            .and_then(|c| file.file_name().map(|n| c.join(n)))
            .filter(|c| c != &file);

        // notify 事件回调 → 事件过滤 → 投递到去抖线程
        let ev_tx = tx.clone();
        let watch_file = file.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let Ok(ev) = res else { return };
            if !is_relevant(&ev) {
                return;
            }
            let hit = ev
                .paths
                .iter()
                .any(|p| p == &watch_file || canon_file.as_ref() == Some(p));
            if hit {
                let _ = ev_tx.send(Msg::Changed(watch_file.clone()));
            }
        })
        .map_err(io::Error::other)?;

        watcher
            .watch(&parent, RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;

        // 去抖线程：recv_timeout 循环，静默满 500 ms 才触发一次回调
        let cb_arc = Arc::new(Mutex::new(cb));
        let thread = std::thread::spawn(move || {
            debounce_loop(rx, cb_arc, file);
        });

        Ok(FileWatch {
            tx,
            _thread: thread,
            _watcher: watcher,
        })
    }

    /// 监听目录内全部变更（TempArea 内部使用，02 §5.3.5 状态机 Watching 态）。
    /// 回调参数为发生变更的具体路径（在 `dir` 之下）。
    pub(crate) fn watch_dir(dir: &Path, cb: Box<dyn Fn(PathBuf) + Send>) -> io::Result<FileWatch> {
        let dir = dir.to_path_buf();
        let (tx, rx): (Sender<Msg>, Receiver<Msg>) = std::sync::mpsc::channel();

        // 同 [`FileWatch::new`]：接受原路径与规范化路径两种形态，回调回投原路径。
        let canon_dir = dir.canonicalize().ok().filter(|c| c != &dir);

        let ev_tx = tx.clone();
        let watch_dir = dir.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let Ok(ev) = res else { return };
            if !is_relevant(&ev) {
                return;
            }
            let hit = ev.paths.iter().find_map(|p| {
                if let Ok(rest) = p.strip_prefix(&watch_dir) {
                    Some(watch_dir.join(rest))
                } else {
                    let cd = canon_dir.as_ref()?;
                    p.strip_prefix(cd).ok().map(|rest| watch_dir.join(rest))
                }
            });
            if let Some(p) = hit {
                let _ = ev_tx.send(Msg::Changed(p));
            }
        })
        .map_err(io::Error::other)?;

        watcher
            .watch(&dir, RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;

        let cb_arc = Arc::new(Mutex::new(cb));
        let thread = std::thread::spawn(move || {
            debounce_loop(rx, cb_arc, dir);
        });

        Ok(FileWatch {
            tx,
            _thread: thread,
            _watcher: watcher,
        })
    }
}

impl Drop for FileWatch {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

/// 事件相关性过滤（02 §5.3.6：Modify / Access(Close(Write))）。
fn is_relevant(ev: &Event) -> bool {
    use notify::event::{AccessKind, AccessMode, ModifyKind};
    matches!(
        ev.kind,
        EventKind::Modify(ModifyKind::Any | ModifyKind::Data(_) | ModifyKind::Metadata(_))
            | EventKind::Access(AccessKind::Close(AccessMode::Write))
    )
}

/// 去抖循环：收到事件重置定时器；连续静默 500 ms 触发一次回调（多次保存只触发
/// 一次确认请求，TC-CORE-019），回调携带最近一次变更路径。回调 panic 捕获记日志。
fn debounce_loop(rx: Receiver<Msg>, cb: DebounceCb, target: PathBuf) {
    let mut deadline: Option<Instant> = None;
    let mut pending: Option<PathBuf> = None;
    loop {
        if let Some(d) = deadline {
            let now = Instant::now();
            if now >= d {
                fire(&cb, pending.take().unwrap_or_else(|| target.clone()));
                deadline = None;
                continue;
            }
            let wait = d - now;
            match rx.recv_timeout(wait) {
                Ok(Msg::Changed(p)) => {
                    pending = Some(p);
                    deadline =
                        Some(Instant::now() + std::time::Duration::from_millis(EDIT_DEBOUNCE_MS));
                }
                Ok(Msg::Stop) => return,
                // 超时是**正常路径**：回到循环顶检查 `now >= d` 触发冲刷；
                // 仅 Disconnected（对端全 drop）才退出去抖线程。
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(Msg::Changed(p)) => {
                    pending = Some(p);
                    deadline =
                        Some(Instant::now() + std::time::Duration::from_millis(EDIT_DEBOUNCE_MS));
                }
                Ok(Msg::Stop) | Err(_) => return,
            }
        }
    }
}

/// 触发回调，panic 捕获记日志（不允许 panic 穿越去抖线程）。
fn fire(cb: &DebounceCb, path: PathBuf) {
    let lock = cb.lock();
    let Ok(mut guard) = lock else {
        tracing::error!("watch 回调锁中毒，丢弃事件 {}", path.display());
        return;
    };
    let f = &mut **guard;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(path)));
    if let Err(payload) = result {
        let msg = payload
            .downcast_ref::<&str>()
            .copied()
            .unwrap_or("unknown panic");
        tracing::error!("watch 回调 panic: {msg}");
    }
}

#[cfg(test)]
#[path = "tests/watch.rs"]
mod tests;
