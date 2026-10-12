//! 单实例文件锁与 argv 转发（T42 instance；02 §11.2 B）。
//!
//! 载体：[`hzip_platform::file_lock`]（Windows LockFileEx / unix flock 排他
//! 锁——T42 平台补丁；进程退出自动释放，无陈旧锁问题）。二次获取失败 =
//! 已有实例 → **整条 argv** 追加 `hzip-forward-queue`（首实例消费）。
//!
//! 锁文件同时写 `pid created_ms`（诊断/孤儿辅助；锁语义以 OS 排他为准，
//! 不依赖内容判定）。
//!
//! # 为什么转发的是整条 argv，而不是路径
//!
//! 右键菜单项经 `CreateProcessW` 拉起的是**带动词**的命令行，例如
//! `hzip-gui --auto-extract <档案>`。只转发路径会把动词丢掉，首实例只能
//! 「打开」该路径——于是 GUI 已运行时点「智能解压 / 添加到 zip」等于没点
//! （实测症状：点击后无任何效果）。故队列按 **argv 数组**逐行 JSON 编码，
//! 动词与路径一并移交，由首实例用**同一个** argv 解析器还原。
//!
//! # 两个半场必须同时成立
//!
//! 1. [`acquire_or_forward`]：二实例写队列（本文件）；
//! 2. [`watch_forward_queue`]：**运行中的首实例**监听并消费它。
//!
//! 缺 (2) 的话转发就是死信——队列只堆积、永不执行。这是历史缺陷：
//! watch 函数写好了但从未被 `run_ui` 调用，正是「右键菜单项点击后毫无反应」
//! 的直接原因。任何新增转发入口都必须同步检查这两个半场。

use std::fs;
use std::path::{Path, PathBuf};

use hzip_platform::dirs;
use hzip_platform::file_lock::{FileLock, try_lock_exclusive};
use hzip_platform::watch::FileWatch;

/// 单实例仲裁结果。
pub enum InstanceRole {
    /// 首实例：持锁守卫（drop = 释放）
    First(FileLock),
    /// 二实例：已转发 argv，直接退出
    Second,
}

/// 转发队列文件（数据目录）。首实例**运行期** watch 它、启动期整表消费它。
fn queue_path() -> PathBuf {
    dirs::data_dir().join("hzip-forward-queue")
}

/// 旧版队列文件名（承载裸路径、无动词）。
///
/// 一次性丢弃：其内容无法安全反解出动词（同一路径可能是「打开 / 解压 / 压缩」
/// 任一意图），留着只会让新消费者反复读到无法解析的行。旧文件仅出现在升级前
/// 的机器上，丢弃是安全的降级（最坏情形 = 那一次点击本来也没生效）。
fn discard_legacy_queue() {
    discard_legacy_file(&dirs::data_dir().join("hzip-open-queue"));
}

/// 旧队列文件的丢弃实现（路径可注入——单测不得在真实数据目录里留文件）。
fn discard_legacy_file(legacy: &Path) {
    if legacy.exists() {
        tracing::info!(
            "丢弃旧版转发队列（裸路径、无动词，无法安全重放）: {}",
            legacy.display()
        );
        let _ = fs::remove_file(legacy);
    }
}

/// 仲裁：数据目录建锁。`argv`（不含程序名）在二实例时整条转发给首实例。
pub fn acquire_or_forward(argv: &[String]) -> InstanceRole {
    let data = dirs::data_dir();
    let _ = fs::create_dir_all(&data);
    let lock_path = data.join("hzip.lock");
    match try_lock_exclusive(&lock_path) {
        Ok(guard) => InstanceRole::First(guard),
        Err(_already) => {
            // 已有实例（锁被排他持有）：转发 argv 后退出
            if !argv.is_empty()
                && let Err(e) = forward_argv(argv)
            {
                tracing::warn!("转发 argv 失败（本次命令行参数已丢失）: {e}");
            }
            InstanceRole::Second
        }
    }
}

/// 二实例 → 追加一条 argv 记录（首实例消费；见本模块文档）。
fn forward_argv(argv: &[String]) -> std::io::Result<()> {
    forward_argv_at(&queue_path(), argv)
}

/// 可注入路径的转发实现（与平台的 `install_at` 同一套测试接缝：单测用临时
/// 路径，绝不污染真实数据目录——多个用例共用一个真实队列会互相消费）。
///
/// **整行一次 `write_all`**：Win32 `FILE_APPEND_DATA` 下单次写入 < 4 KiB 是
/// 原子的，故消费者不会读到半行（分两次写就可能被截在行中间——这也是旧实现
/// 逐个参数 `writeln!` 的隐患）。
fn forward_argv_at(queue: &Path, argv: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    let mut line = serde_json::to_string(argv).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(queue)?;
    f.write_all(line.as_bytes())?;
    // 显式 flush：append 句柄的缓冲若在进程退出前未落盘，二实例已退出而
    // 首实例可能刚好在此刻读队列 → 该次转发静默丢失。
    f.flush()
}

/// 整表消费转发队列（首实例**启动**时调用一次）。
///
/// 启动前可能有二实例已写入（首实例正在初始化、watch 尚未挂载），故这里
/// 必须与运行期 watch 共用同一解析/清空语义。
pub fn take_forwarded() -> Vec<Vec<String>> {
    discard_legacy_queue();
    drain_queue_at(&queue_path())
}

/// 读队列 → 解析 → 清空。**保留文件本身**：运行期 watch 依附于该路径，
/// 删除文件会让监听失效（旧实现在这里 `remove_file`，与 watch 语义冲突）。
fn drain_queue() -> Vec<Vec<String>> {
    drain_queue_at(&queue_path())
}

/// 可注入路径的消费实现（测试接缝，同 [`forward_argv_at`]）。
fn drain_queue_at(queue: &Path) -> Vec<Vec<String>> {
    let Ok(content) = fs::read_to_string(queue) else {
        return Vec::new();
    };
    if content.is_empty() {
        return Vec::new();
    }
    let parsed = parse_queue_lines(&content);
    // 整体清空（含无法解析的脏行——留着只会每次 watch 重复告警）
    if let Err(e) = fs::write(queue, "") {
        tracing::warn!("转发队列清空失败（下次 watch 会重复消费同一批）: {e}");
    }
    parsed
}

/// 队列文本 → argv 记录列表（逐行 JSON；容忍脏行）。
///
/// 无法解析的行**逐行丢弃并告警**，绝不整体失败：队列是跨进程共享的追加
/// 文件，任何一条坏记录都不该让其余意图陪葬。
fn parse_queue_lines(content: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for line in content.split('\n') {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Vec<String>>(line) {
            Ok(argv) => out.push(argv),
            Err(e) => tracing::warn!("转发队列脏行（丢弃）: {line:?} / {e}"),
        }
    }
    out
}

/// 运行期队列 watch（T42 挂账 #3 闭环；T-G13 §3.1）。
///
/// `hzip_platform::watch` 监听队列文件（500 ms 去抖 02 §5.3.6），变更 →
/// [`drain_queue`] 整体消费 → `on_argv(records)` 回调。
/// 回调在 watch 去抖线程执行——**不得阻塞**（重活交给调用方线程化：消费方
/// 应把结果投进通道，由 UI 线程执行意图）。
///
/// # 为什么必须有调用方
///
/// 首实例**运行中**才有转发可言，而启动期的 [`take_forwarded`] 只覆盖「首实例
/// 尚未起来时」那一段窗口。没有本函数接线，运行期转发全部是死信。
pub fn watch_forward_queue(
    on_argv: impl Fn(Vec<Vec<String>>) + Send + Sync + 'static,
) -> Option<FileWatch> {
    discard_legacy_queue();
    let queue = queue_path();
    // watch 目标文件可能尚不存在：先触碰创建（`watch` 内部监听父目录 + 路径
    // 过滤，目标不存在时事件匹配不上）
    if !queue.exists()
        && let Err(e) = fs::File::create(&queue)
    {
        tracing::warn!("转发队列文件创建失败（watch 不生效）: {e}");
        return None;
    }
    let cb = Box::new(move |_changed: PathBuf| {
        let records = drain_queue();
        if !records.is_empty() {
            on_argv(records);
        }
    });
    match FileWatch::new(&queue, cb) {
        Ok(watch) => {
            tracing::debug!("转发队列 watch 已挂载: {}", queue.display());
            Some(watch)
        }
        Err(e) => {
            tracing::warn!("转发队列 watch 挂载失败（降级：仅启动时 take_forwarded）: {e}");
            None
        }
    }
}

#[cfg(test)]
#[path = "tests/instance.rs"]
mod tests;
