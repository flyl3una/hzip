//! 磁盘空间预检与临时缓冲自愈（02 §9.4 D 框的公共部分 + 2026-09-24 实测补充）。
//!
//! # 两个消费者，一个事实来源
//!
//! | 消费者 | 时机 | 「需要多少」的来源 |
//! |---|---|---|
//! | [`crate::services::extract::precheck_disk`] | 解压开始前 | Σ 选中条目 `size_raw` |
//! | [`precheck_open_temp_space`] | **打开档案前**（组合链） | 编码层声明大小（gz 尾 ISIZE / xz 尾索引 / zst 帧头） |
//!
//! # 为什么「打开」也要预检（用户实测 2026-09-24）
//!
//! 组合档（`.cpio.gz` / `.tar.gz` / `.tar.xz`…）的内层容器 handler 必须先把
//! **解压后的整层**落到临时文件才能随机访问（cpio/tar 的索引/乱序读要 seek，
//! 红线 1）。于是「临时盘装得下解压后的大小」是打开的前置条件——而这一步发生
//! 在 `sniff` 之后、`entries()` 之前，用户看到的现象是：
//!
//! > 打开 `huge-init.cpio.gz`（940 MB），等了十多秒，然后直接报「磁盘」错误，
//! > 打开窗口也一闪就没了。
//!
//! 两个问题各自修：
//!
//! 1. **等十多秒**：本预检在解压**之前**用声明大小比可用空间，不足立刻
//!    `HZ-1002`（毫秒级失败，不白解一层）；
//! 2. **窗口一闪**：GUI 侧失败不再自动关窗（`ui/views/open_progress.rs`）；
//!    失败同时进任务中心（`viewmodel/jobs_vm.rs` 的 open 行）。
//!
//! 兜底：声明大小未知（bz2 / 自产 zst）时不预检——此时由 fmt 层 spool 的
//! 写盘错误兜底，错误同样带临时目录上下文（`fmt-cpio/fmt-tar` 的 `spool_error`）。
//!
//! # 临时缓冲自愈
//!
//! `NamedTempFile` 只在 drop 时删除：进程被强杀/崩溃会留下**孤儿缓冲**。实测
//! 连续几次测试中断累积 8 个孤儿、29.6 GiB，把系统盘挤到 3 GiB 可用——正是上面
//! 那次 ENOSPC 的直接原因。故 spool 文件名带 `hzip-spool-` 前缀，
//! [`sweep_stale_spools`] 在启动时回收超龄孤儿（在用的缓冲最多存活几分钟，
//! 24 h 阈值不可能误删）。

use std::path::PathBuf;
use std::time::Duration;

use hzip_error::HzipError;
#[cfg(feature = "default-formats")]
use hzip_format::Sniffed;
use hzip_format::{FormatRegistry, Source};

use crate::consts::SPACE_HEADROOM;

/// 孤儿临时缓冲的判定前缀（与 `hzip-fmt-cpio`/`hzip-fmt-tar` 的 spool 同值）。
pub const SPOOL_PREFIX: &str = "hzip-spool-";

/// 孤儿回收阈值：mtime 早于该时长的 `hzip-spool-*` 视为崩溃残留。
///
/// 24 h 是安全边界：正常一次打开/解压的缓冲存活以秒计，跨进程并发时也不可能
/// 有一个 24 h 前创建、此刻仍在写入的缓冲。清理只在启动时做一次。
pub const SPOOL_STALE_AFTER: Duration = Duration::from_secs(24 * 3600);

/// 磁盘空间不足（`HZ-1002`；`UserAction::FreeSpaceOrChangeTarget`）。
///
/// 用平台 ENOSPC/ERROR_DISK_FULL 的 os 码构造：`HzipError::code` 经 `io_code`
/// 把它归到 1002（`hzip-error/src/code.rs`），文案与修复指引都由错误码派生，
/// 上层（CLI/GUI/任务中心）不需要认识「磁盘满」这件事。`path` 给 `Display`
/// 提供落点（解压是目标目录，打开是临时目录）。
pub(crate) fn disk_full_error(path: Option<PathBuf>) -> HzipError {
    HzipError::Io {
        source: std::io::Error::from_raw_os_error(disk_full_os_code()),
        path,
    }
}

/// 平台 ENOSPC/ERROR_DISK_FULL os 码（`io_code` 映射 1002）。
fn disk_full_os_code() -> i32 {
    #[cfg(windows)]
    {
        112 // ERROR_DISK_FULL
    }
    #[cfg(not(windows))]
    {
        28 // ENOSPC
    }
}

/// 空间是否不足（纯判定，便于单测注入两侧数值——与 extract 侧同款手法）。
///
/// 唯一调用点在 `default-formats` 门控的组合链预检里 ⇒ 裁掉该 feature 后本函数成了
/// 死代码（**配置合法**，不是缺陷），故按键精确放行告警而不是摘掉函数：它的单测在
/// 任何 feature 组合下都要照常编译。
#[cfg_attr(not(feature = "default-formats"), allow(dead_code))]
fn space_shortfall(need: u64, avail: u64) -> bool {
    avail < (need as f64 * SPACE_HEADROOM) as u64
}

/// 打开前的临时盘预检（组合链专用；非组合/大小未知/源不可预检 → `Ok`）。
///
/// 判据链（任何一环不成立都直接放行，绝不因预检本身挡住能打开的档案）：
/// 1. 源是 `File`（`Stdin`/`Stream` 没有可查的路径与长度）；
/// 2. `sniff` 判为 `Composite` 且有**编码层**（纯容器/嵌套条目不走这里）；
/// 3. 最内层编码层能报出解压后大小（gz/xz/zst；bz2 无字段 → 放行）；
/// 4. 临时目录可用空间 ≥ 声明大小 × [`SPACE_HEADROOM`]。
pub(crate) fn precheck_open_temp_space(
    registry: &FormatRegistry,
    src: &Source,
) -> Result<(), HzipError> {
    #[cfg(not(feature = "default-formats"))]
    {
        let _ = (registry, src);
        Ok(())
    }
    #[cfg(feature = "default-formats")]
    {
        let (path, packed) = match src {
            Source::File(p) => (p.clone(), std::fs::metadata(p).map(|m| m.len()).ok()),
            // 分卷组合（`.zip.001.gz` 之类）拿首卷估——估不准不预检（None 分支放行）
            Source::Volumes { first, .. } => (
                first.clone(),
                std::fs::metadata(first).map(|m| m.len()).ok(),
            ),
            Source::Stdin | Source::Stream(_) => return Ok(()),
        };
        let Some(packed) = packed else {
            return Ok(());
        };
        let Sniffed::Composite { layers, .. } = registry.sniff(src)? else {
            return Ok(());
        };
        // 最内层编码层的**输出**就是内层容器最终落盘的字节数
        let Some(codec) = layers.last() else {
            return Ok(());
        };
        let Some(need) = hzip_fmt_single::declared_payload_size(codec.as_str(), &path, packed)
        else {
            return Ok(());
        };
        let dir = std::env::temp_dir();
        let avail = hzip_platform::fs_util::available_bytes(&dir).map_err(|e| {
            // 临时目录本身不可用（不存在/无权限）：这里就失败，别等到解压完
            // 写临时文件才报——同一处语义、更早失败。
            tracing::error!(dir = %dir.display(), err = %e, "打开预检失败：临时目录不可用");
            HzipError::Io {
                source: e,
                path: Some(dir.clone()),
            }
        })?;
        if space_shortfall(need, avail) {
            tracing::error!(
                src = %path.display(),
                need, avail,
                dir = %dir.display(),
                "打开预检失败：临时盘装不下解压后的内层容器"
            );
            return Err(disk_full_error(Some(dir)));
        }
        Ok(())
    }
}

/// 进入嵌套容器所需的临时盘空间（字节，已含 [`SPACE_HEADROOM`]）。
///
/// 峰值 = **一份**内层档案：明文条目由格式层直读、流式写出；加密条目在 zip 里
/// 先 spool 解码成一份副本，随后被 [`hzip_platform::temp_area::TempArea::adopt_file`]
/// **移动**进会话目录（不再复制）。
///
/// 为什么不复用 [`precheck_open_temp_space`]：那条算的是「编码层声明大小」，只在
/// **打开档案**时跑；双击压缩包里的条目是另一条路径，而它正是在这一步把临时盘
/// 写爆的（用户实测 2026-10-22：4.2 GB 可用 vs 7.9 GB 条目）。
pub fn container_entry_need(size_raw: u64) -> u64 {
    (size_raw as f64 * SPACE_HEADROOM) as u64
}

/// 这些目录里**最紧**的一块能否放下 `need`；放不下返回 `(need, avail)`。
///
/// `None` = 可用空间查不到（目录不存在/无权限）⇒ 放行：预检本身绝不能挡住
/// 能打开的档案，真正的写盘错误仍由物化/长度校验兜底。
pub fn temp_space_shortfall(dirs: &[PathBuf], need: u64) -> Option<(u64, u64)> {
    let avail = dirs
        .iter()
        .filter_map(|d| hzip_platform::fs_util::available_bytes(d).ok())
        .min()?;
    (avail < need).then_some((need, avail))
}

/// 回收超龄的临时缓冲孤儿（`hzip-spool-*`；返回删除个数）。
///
/// 见模块文档「临时缓冲自愈」。**尽力而为**：任何 IO 错误（无权限的临时目录、
/// 并发删除竞态）都静默跳过——清理绝不能把启动拖挂或变成失败路径。
///
/// 只删常规文件（不递归、不跟随符号链接），且要求 `mtime` 早于 `max_age`。
///
/// # 扫两处（2026-10-17 核对补齐）
///
/// spool 缓冲有两处落点，**同名前缀**（[`SPOOL_PREFIX`]）：
/// - `zip`/`cpio`/`tar` 的 `Spool` → [`std::env::temp_dir`]（`/tmp`）；
/// - 卷容器（`hzip-vol::SpoolPolicy::default().dir` = `hzip-platform::dirs::temp_root()`）
///   → `temp_dir()/hzip`（Linux 上 `XDG_RUNTIME_DIR/hzip`）。
///
/// 此前只扫第一处 ⇒ 卷容器的孤儿**从未被回收**（40 GB 镜像的 spool 崩一次就永久
/// 占着临时盘）。第二处目录不存在时 `read_dir` 直接失败返回 0，无副作用。
pub fn sweep_stale_spools(max_age: Duration) -> usize {
    sweep_stale_spools_in(&std::env::temp_dir(), max_age)
        + sweep_stale_spools_in(&hzip_platform::dirs::temp_root(), max_age)
}

/// [`sweep_stale_spools`] 的可注入目录版本（测试用同一套判定逻辑）。
pub fn sweep_stale_spools_in(dir: &std::path::Path, max_age: Duration) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0usize;
    let mut bytes = 0u64;
    for ent in rd.flatten() {
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(SPOOL_PREFIX) {
            continue;
        }
        let Ok(meta) = ent.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let stale = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > max_age);
        if !stale {
            continue;
        }
        if std::fs::remove_file(ent.path()).is_ok() {
            removed += 1;
            bytes = bytes.saturating_add(meta.len());
        }
    }
    if removed > 0 {
        tracing::info!(
            removed,
            mib = bytes / (1024 * 1024),
            "已回收超龄临时缓冲孤儿（hzip-spool-*）"
        );
    }
    removed
}

#[cfg(test)]
#[path = "tests/space.rs"]
mod tests;
