//! 系统通知（02 §5.3；R-GUI4 #3 任务完成流程）。
//!
//! 三平台 toast/通知中心（notify-rust：Windows 经 WinRT toast、macOS 经
//! 通知中心、Linux 经 zbus）。**线程语义**：非阻塞快调用（Windows WinRT
//! toast ~ms 级）——泵线程直接调用即可；发送失败仅 warn（通知是尽力而为
//! 的旁路，不影响主流程）。
//!
//! 模块名用 `notification` 而非 `notify`——避免与本 crate 依赖的文件监听
//! crate `notify`（watch.rs 消费）路径冲突。

/// 发送桌面通知（title + body；失败仅 warn 不上抛）。
pub fn notify(title: &str, body: &str) {
    if let Err(e) = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show()
    {
        tracing::warn!("系统通知发送失败（不影响主流程）: {e}");
    }
}
