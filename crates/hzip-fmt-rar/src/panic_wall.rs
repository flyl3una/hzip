//! C 边界三重防护（02 §8.3 图 / T26 §4.3）。
//!
//! unrar C 库（C++ 静态绑定）**非线程安全**（02 §13.2 风险项），且对畸形输入
//! 以 C++ 层面可能 panic/崩溃的方式暴露。三重防护：
//!
//! ① **全局互斥锁** [`UNRAR_MUTEX`]：串行化全部 unrar 调用（`with_unrar`
//!    持锁期间 catch_unwind 包裹闭包）；
//! ② **catch_unwind 错误边界监控**：C 边界 panic 拦截 → `tracing::error!`
//!    记录摘要（错误边界监控），进程存活；
//! ③ **错误码映射**：[`map_unrar_err`] 把 unrar 错误码 → [`HzipError`]
//!   （未知码 → `Format{BadHeader}` HZ-2004 + 原始码附注）。
//!
//! # 错误码映射表（T26 §4.3 表）
//!
//! | unrar 错误（语义） | HzipError | HZ |
//! |---|---|---|
//! | `UnknownFormat`（魔数不识别） | `Format{BadSignature}` | 2001 |
//! | `EndArchive` 过早（截断/提前收尾） | `Format{Truncated}` | 2002 |
//! | `BadArchive` / `BadData`@Open/Read（头损坏） | `Format{BadHeader}` | 2004 |
//! | `EOpen` / 卷缺失 | `Format{MissingVolume(卷名)}`（卷发现层附名；处理期 EOpen 即卷缺失） | 2005 |
//! | `MissingPassword` / `BadPassword` | `Password{Archive}`（头加密场景由调用方改写 Header） | 3101/3102 |
//! | `BadData`@Process（解包 CRC 不符） | `Checksum{entry,0,0}`（调用方补真实 CRC） | 3001 |
//! | `ERead`/`EWrite`/`ECreate`/`EClose`/`SmallBuf` | `Io{source}`（经 io 错误重建） | 1xxx |
//! | 其余 / 未知 | `Format{BadHeader}` + 原始码 Debug 附注（tracing warn） | 2004 |
//!
//! # 设计注（T26 §4.3 / 边界 §7）
//!
//! - 单入口 `with_unrar`：**全部** unrar 调用（打开/列头/解包/测试）一律经此
//!   包裹；`UNRAR_MUTEX` 为进程级唯一互斥（C 库非线程安全，串行化为正确性让
//!   路——勿用细粒度锁「优化」C 边界，02 §8.3 ①）；
//! - `with_unrar` 返回 `UnrarOutcome`（结果/错误/panic 三态）——panic 与普通
//!   unrar 错误区分，供 [`map_unrar_err`] 只在真实错误上套映射（panic 固定映射
//!   到 HZ-2004 并已在 ② 记日志）；
//! - 密码相关：调用方在闭包外**预取**闭包内使用的密码字节（见 [`reader`]），
//!   保证零化串在 `with_unrar` 内外生命周期清晰；此处不持任何密码。

use std::sync::{Mutex, MutexGuard};

use hzip_error::{FormatErrorKind, HzipError};
use unrar::error::{Code, UnrarError, When};

/// rar 格式 ID（映射错误承载）。
const FORMAT_ID: &str = "rar";

/// ① 全局互斥：串行化全部 unrar 调用（C 库非线程安全，02 §8.3 ①）。
pub(crate) static UNRAR_MUTEX: Mutex<()> = Mutex::new(());

/// 闭包结果三态（区分 panic 与普通 unrar 错误）。
pub(crate) enum UnrarOutcome<T> {
    /// 闭包成功返回
    Ok(T),
    /// 闭包返回 unrar 错误（套 [`map_unrar_err`]）
    Err(UnrarError),
    /// 闭包内部 panic（已被 catch_unwind 拦截；套 HZ-2004）
    Panicked,
}

/// ② catch_unwind 错误边界监控 + ① 互斥的收敛入口。
///
/// 持锁 → `catch_unwind(AssertUnwindSafe(闭包))` → panic 记 `tracing::error!`
/// （错误边界监控）并返回 [`UnrarOutcome::Panicked`]。闭包内只允许返回
/// `Result<T, UnrarError>`（unrar 错误）；io 等自身错误请在闭包内先映射为
/// [`HzipError`] 或用 `crate::map_io`。
pub(crate) fn call_unrar<T>(
    label: &'static str,
    f: impl FnOnce() -> Result<T, UnrarError>,
) -> Result<UnrarOutcome<T>, HzipError> {
    let _guard: MutexGuard<'_, ()> = UNRAR_MUTEX.lock().map_err(|_| poisoned())?;
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(UnrarOutcome::Ok(v)),
        Ok(Err(e)) => Ok(UnrarOutcome::Err(e)),
        Err(payload) => {
            let msg = panic_payload_dbg(&payload);
            tracing::error!(
                target: "hzip_fmt_rar",
                label,
                panic = %msg,
                "unrar C 边界 panic 已拦截（错误边界监控，02 §8.3 ②）"
            );
            Ok(UnrarOutcome::Panicked)
        }
    }
}

/// ③ unrar 错误码 → HzipError 映射（T26 §4.3 表；未知 → 2004 + 原始码附注）。
///
/// 调用方负责附上下文：`MissingPassword`/`BadPassword` 时目标 Archive 或
/// Entry；卷缺失名由卷发现层先产出 [`FormatErrorKind::MissingVolume`]，处理期
/// `EOpen` 视为「unrar 找不到下一卷」（其名来自卷列表）。
pub(crate) fn map_unrar_err(e: &UnrarError) -> HzipError {
    use Code::*;
    use FormatErrorKind::*;
    match e.code {
        UnknownFormat => fmt(BadSignature),
        EndArchive => fmt(Truncated),
        BadArchive => fmt(BadHeader),
        BadData => match e.when {
            When::Process => HzipError::Checksum {
                entry: hzip_error::EntryName::new("<rar entry>"),
                expected: 0,
                actual: 0,
            },
            _ => fmt(BadHeader),
        },
        MissingPassword | BadPassword => HzipError::Password {
            target: hzip_error::PasswordTarget::Archive,
        },
        EOpen => fmt(BadHeader), // 打开期 EOpen（档案不存在等）；处理期卷缺失由 volume 层先转 MissingVolume
        ECreate | EClose | ERead | EWrite | SmallBuf => {
            // 真实 IO：经 io::Error 重建 → Io{source}（1xxx）
            HzipError::Io {
                source: std::io::Error::other(format!("{e}")),
                path: None,
            }
        }
        NoMemory => {
            tracing::warn!(err = %e, "unrar 内存不足（ERAR_NO_MEMORY）");
            fmt(BadHeader)
        }
        EReference => {
            // 引用记录缺源文件（`rar -oi`）——结构性问题按坏头
            tracing::warn!(err = %e, "unrar EReference（引用记录缺源）");
            fmt(BadHeader)
        }
        other => {
            // 其余/未知码 → Format{BadHeader} + 原始码 Debug 附注
            tracing::warn!(code = ?other, err = %e, "unrar 未映射错误码 → 按 HZ-2004 处理");
            fmt(BadHeader)
        }
    }
}

/// panic 载荷 → 人读摘要（防 panic payload 非 str 时丢失）。
fn panic_payload_dbg(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        format!("非字符串 panic payload: {:?}", payload.type_id())
    }
}

/// 锁中毒 → Format{BadHeader}（毒只在 panic 时出现，理论不可达——with_unrar
/// 内 panic 已被 catch_unwind 拦截；保守防御）。
fn poisoned() -> HzipError {
    HzipError::Format {
        format: hzip_error::FormatId::from(FORMAT_ID),
        kind: FormatErrorKind::BadHeader,
    }
}

fn fmt(kind: FormatErrorKind) -> HzipError {
    HzipError::Format {
        format: hzip_error::FormatId::from(FORMAT_ID),
        kind,
    }
}

#[cfg(test)]
#[path = "tests/panic_wall.rs"]
mod tests;
