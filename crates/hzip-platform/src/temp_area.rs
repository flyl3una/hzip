//! 双击打开/编辑保存的受监控临时目录（02 §5.3.5；TC-SEC-015 / TC-CORE-020）。
//!
//! 生命周期状态机（02 §5.3.5）：
//!
//! ```text
//! [*] --> Created: new_session() 创建会话目录
//! Created --> Watching: watch(callback)
//! Watching --> Watching: 变更事件(去抖500ms)
//! Watching --> Cleaning: cleanup()/进程退出
//! Created --> Cleaning: 无编辑需求直接退出
//! Cleaning --> [*]: 目录删除
//! ```
//!
//! 启动期孤儿清理独立执行：扫描 `{tmp}/hzip/*` 读 pid 文件，PID+创建时间双判定，
//! 死会话整目录删除（02 §5.3.5；TC-CORE-020）。
//!
//! 路径分配规则（稳定可复现，02 §5.3.5）：`{root}/{档案路径 sha256 前 8 字符}/{条目安全名}`；
//! 条目安全名经与解压相同的净化函数处理（TC-SEC-015）。本任务内置**私有简化净化**
//! （段校验 + 拒绝穿越），**T31 合入后对齐同源 `sanitize_entry_path`**（任务卡 §7.4
//! 净化双源风险跟踪 TODO）。

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::consts::TEMP_ENTRY_HASH_LEN;
use crate::watch::FileWatch;

/// 会话状态（02 §5.3.5 状态机）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TempState {
    /// 会话目录已创建
    Created,
    /// 已注册变更回调（Watching 态）
    Watching,
    /// 清理中/已清理（终态）
    Cleaning,
}

/// 净化后的条目安全名（单段、无路径穿越，TC-SEC-015）。
///
/// 构造即校验：拒绝 `..` / 绝对路径 / 分隔符 / 控制字符 / 空名 / `.`（02 §9.6 净化
/// 语义的段级子集；**T31 `sanitize_entry_path` 合入后收敛为同源实现**，任务卡 §7.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryNameSafe(String);

impl EntryNameSafe {
    /// 净化条目名（私有简化净化：段校验 + 拒绝穿越，02 §9.6 / TC-SEC-015）。
    /// 非法输入返回 `InvalidInput`，不产生任何路径效果。
    pub fn sanitize(raw: &str) -> io::Result<Self> {
        if raw.is_empty() || raw == "." || raw == ".." {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "条目名为空或 `.`/`..`",
            ));
        }
        if raw.starts_with('/') || raw.starts_with('\\') {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "绝对路径"));
        }
        if raw.contains('/') || raw.contains('\\') {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "含路径分隔符"));
        }
        if raw.contains('\0') || raw.chars().any(|c| c.is_control()) || raw.contains("..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "含控制字符或 `..`",
            ));
        }
        Ok(Self(raw.to_owned()))
    }

    /// 安全名切片（用于路径拼接，无 `..` / 分隔符）。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EntryNameSafe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 受监控临时会话（02 §5.3.5）。
///
/// `Drop` 时若未 `cleanup` 记 `tracing::warn!`（不阻塞 drop，孤儿清理兜底）。
pub struct TempArea {
    session: PathBuf,
    state: Mutex<TempState>,
    watcher: Mutex<Option<FileWatch>>,
}

impl TempArea {
    /// 创建 `{temp_root}/hzip/{uuid}` 会话目录 + 写 pid 文件（02 §5.3.5）。
    ///
    /// `root` 为 `{tmp}/hzip` 级目录（缺省 [`crate::dirs::temp_root`]）；
    /// 会话目录 = `root/{uuid}`，uuid 以 pid + 时间戳 + 序号生成（稳定唯一，
    /// 非 UUID 标准格式，仅作会话目录名）。
    pub fn new_session(root: Option<PathBuf>) -> io::Result<TempArea> {
        let root = root.unwrap_or_else(crate::dirs::temp_root);
        std::fs::create_dir_all(&root)?;
        let uuid = session_id();
        let session = root.join(uuid);
        std::fs::create_dir(&session)?;
        let ta = TempArea {
            session,
            state: Mutex::new(TempState::Created),
            watcher: Mutex::new(None),
        };
        ta.write_pid_file()?;
        Ok(ta)
    }

    /// 会话目录路径。
    pub fn session_dir(&self) -> &Path {
        &self.session
    }

    /// 物化条目到 `{root}/{sha256(档案路径)[0..8]}/{条目安全名}`，返回目标路径
    /// （02 §5.3.5 路径分配规则，稳定可复现）。
    ///
    /// 状态机约束：`Cleaning` 后调用返回 `InvalidInput`（非法迁移）。
    pub fn materialize(
        &self,
        archive: &Path,
        entry: &EntryNameSafe,
        mut bytes: impl Read,
    ) -> io::Result<PathBuf> {
        self.assert_not_cleaning()?;
        let hash_dir = self.session.join(archive_hash(archive));
        std::fs::create_dir_all(&hash_dir)?;
        let target = hash_dir.join(entry.as_str());
        let mut out = std::fs::File::create(&target)?;
        // 写盘失败（ENOSPC 等）时删掉半截文件：否则几 GB 的残留会一直占着临时盘，
        // 下一次尝试还会撞上「空间不足」（用户实测 2026-10-22）。
        if let Err(e) = io::copy(&mut bytes, &mut out) {
            drop(out);
            let _ = std::fs::remove_file(&target);
            return Err(e);
        }
        Ok(target)
    }

    /// 把调用方手里**已经在盘上**的一份完整条目副本收进会话目录。
    ///
    /// 与 [`Self::materialize`] 同一个落点规则，但走 `rename`：同卷移动近乎免费、
    /// 不额外占空间（跨卷/目标占用时退回复制）。用于「格式层已经 spool 过一整份」
    /// 的加密条目——再复制一遍会让临时盘峰值翻倍（4.34 GB 条目 = 8.7 GB，用户实测
    /// 2026-10-22 直接被挤爆）。
    ///
    /// `src` 的所有权归本方法：移动成功即消失，复制回退会删掉它。
    pub fn adopt_file(
        &self,
        archive: &Path,
        entry: &EntryNameSafe,
        src: &Path,
    ) -> io::Result<PathBuf> {
        self.assert_not_cleaning()?;
        let hash_dir = self.session.join(archive_hash(archive));
        std::fs::create_dir_all(&hash_dir)?;
        let target = hash_dir.join(entry.as_str());
        match std::fs::rename(src, &target) {
            Ok(()) => Ok(target),
            Err(_) => {
                // 跨卷（%TEMP% 与会话目录不同盘）或目标被占用 → 复制后删源
                std::fs::copy(src, &target)?;
                let _ = std::fs::remove_file(src);
                Ok(target)
            }
        }
    }

    /// 注册变更回调（内部转 [`crate::watch`] 模块，进入 Watching 态，02 §5.3.5）。
    ///
    /// 状态机约束：`Created` → `Watching` 合法；已 `Watching` 幂等；`Cleaning` 后
    /// 报错。回调经 500 ms 去抖后触发（TC-CORE-019）。
    pub fn watch(&self, cb: Box<dyn Fn(PathBuf) + Send>) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("状态锁中毒"))?;
        match *state {
            TempState::Cleaning => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cleaning 后不可 watch",
            )),
            TempState::Created | TempState::Watching => {
                *state = TempState::Watching;
                drop(state);
                let w = FileWatch::watch_dir(&self.session, cb)?;
                *self
                    .watcher
                    .lock()
                    .map_err(|_| io::Error::other("watcher 锁中毒"))? = Some(w);
                Ok(())
            }
        }
    }

    /// 整目录删除（Cleaning → 终态，02 §5.3.5），幂等。
    pub fn cleanup(&self) -> io::Result<()> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| io::Error::other("状态锁中毒"))?;
            *state = TempState::Cleaning;
        }
        if self.session.exists() {
            crate::fs_util::remove_all_no_follow(&self.session)?;
        }
        Ok(())
    }

    fn assert_not_cleaning(&self) -> io::Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("状态锁中毒"))?;
        match *state {
            TempState::Cleaning => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cleaning 后不可 materialize",
            )),
            TempState::Created | TempState::Watching => Ok(()),
        }
    }

    /// 写 pid 文件（会话目录下 `session.json`，02 §5.3.8）：
    /// `{"pid": <pid>, "created_ms": <本进程创建时刻 Unix 毫秒>}`。
    fn write_pid_file(&self) -> io::Result<()> {
        let pid = std::process::id();
        let created_ms = crate::proc::self_created_ms();
        let body = format!("{{\"pid\": {pid}, \"created_ms\": {created_ms}}}");
        std::fs::write(self.session.join("session.json"), body)
    }
}

impl Drop for TempArea {
    fn drop(&mut self) {
        let state = self.state.lock().map(|g| *g).unwrap_or(TempState::Cleaning);
        if state != TempState::Cleaning {
            tracing::warn!(session = %self.session.display(), "TempArea 未 cleanup 即丢弃，留待孤儿清理兜底（02 §5.3.5）");
        }
    }
}

/// 会话唯一目录名（pid + 时间戳纳秒 + 原子序号）。
fn session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("session-{:x}-{nanos:x}-{seq}", std::process::id())
}

/// 档案路径 sha256 前 8 字符（02 §5.3.5 路径分配；`TEMP_ENTRY_HASH_LEN=8`）。
fn archive_hash(archive: &Path) -> String {
    let bytes = archive.to_string_lossy().as_bytes().to_vec();
    let digest = Sha256::digest(&bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..TEMP_ENTRY_HASH_LEN.min(hex.len())].to_owned()
}

/// 启动期孤儿清理（02 §5.3.5 / TC-CORE-020）：扫描 `{tmp}/hzip/*`（即 `root/*`），
/// 读 pid 文件；PID 不存在或创建时间不符 → 死会话整目录删除；活会话保留。
///
/// 双判定失败/读不到/解析失败 → 整目录删除；**PID 存在且创建时间一致 → 保留**。
/// 返回删除的会话目录数。异步执行不阻塞启动（02 §9.2 F 步由调用方调度）。
///
/// **生产必须在启动时调**（2026-10-19 用户实测：`%TEMP%\hzip` 堆了 2115 个会话
/// 目录 / 47 GB——退出钩子只清当次会话，崩溃/强杀留下的没人收，而本函数在
/// 生产代码里从未被调用过）。GUI 启动时在后台线程调一次见
/// `hzip-gui/src/ui/shell/runtime.rs`。
pub fn cleanup_orphans(root: &Path) -> usize {
    cleanup_orphans_with(root, ORPHAN_MISSING_PID_MIN_AGE)
}

/// 「无 pid 文件」目录的最小存活时间：小于它一律放过。
///
/// `new_session` 是**先 mkdir、后写 `session.json`**，中间有微秒级窗口；另一个
/// 实例恰在此刻扫盘就会把「刚建好还没写 pid 文件」的活会话删掉。存活时间一过
/// 仍没有 pid 文件，才算孤儿（留待下一轮启动清理）。
const ORPHAN_MISSING_PID_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// [`cleanup_orphans`] 的可注入年龄版（测试用最小年龄 0 覆盖两条分支）。
fn cleanup_orphans_with(root: &Path, missing_pid_min_age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0usize;
    for ent in entries.flatten() {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let pid_file = path.join("session.json");
        let Some((pid, created_ms)) = read_pid_file(&pid_file) else {
            // 读不到 / 解析失败：可能是**另一个实例刚 mkdir、还没写完 pid 文件**
            // ⇒ 太新的先放过；过了存活时间仍没有 → 孤儿，整目录删除。
            if !missing_pid_min_age.is_zero() && dir_is_fresh(&path, missing_pid_min_age) {
                continue;
            }
            if remove_session_dir(&path) {
                removed += 1;
            }
            continue;
        };
        if crate::proc::is_alive(pid, created_ms) {
            // 活会话：保留
            continue;
        }
        if remove_session_dir(&path) {
            removed += 1;
        }
    }
    removed
}

/// 目录是否「还很新」（mtime 距今 < `min_age`）；取不到时间戳按**不新**处理
/// （宁可删孤儿，不留垃圾——真正在用的会话有 pid 文件，走不到这条分支）。
fn dir_is_fresh(path: &Path, min_age: std::time::Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().map(|age| age < min_age).unwrap_or(false))
        .unwrap_or(false)
}

/// 解析 pid 文件（`{"pid": n, "created_ms": m}` 手工解析，不引入 serde）。
fn read_pid_file(path: &Path) -> Option<(u32, u64)> {
    let text = std::fs::read_to_string(path).ok()?;
    let pid = parse_json_u64(&text, "pid")?;
    let created_ms = parse_json_u64(&text, "created_ms")?;
    Some((pid as u32, created_ms))
}

/// 极小 JSON 数值提取（key 后第一个整数），非目标 key 忽略。
fn parse_json_u64(text: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\"");
    let idx = text.find(&needle)? + needle.len();
    let rest = &text[idx..];
    let digits: String = rest
        .chars()
        .skip_while(|c| *c != ':' && *c != ' ')
        .skip_while(|c| *c == ':' || *c == ' ')
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn remove_session_dir(path: &Path) -> bool {
    crate::fs_util::remove_all_no_follow(path).is_ok()
}

#[cfg(test)]
#[path = "tests/temp_area.rs"]
mod tests;
