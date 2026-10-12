//! macOS「用 hzip 打开」事件（Finder ▸ 打开方式 / 双击关联 / `open -a`）。
//!
//! # 为什么不能只靠 argv
//!
//! macOS 的「打开方式」**不走命令行**：LaunchServices 把待打开的文件作为
//! Apple Event（`kAEOpenDocuments` → `application:openURLs:`）投给进程——
//! 已运行的实例直接收到事件，未运行的实例则在启动**之后**收到同一事件。
//! 于是「双击/右键打开」的新进程 argv 是空的：实测症状是 hzip 窗口起来了、
//! 压缩包没打开，日志里 `open=[]`（argv 解析器永远看不到这个事件）。
//!
//! gpui 把该事件暴露为 `Application::on_open_urls`，回调拿到的是 **URL
//! 字符串**（`file:///…`，路径按百分号转义），不是路径——直接当路径用同样
//! 打不开（`%20` 会成为字面量）。
//!
//! # 职责切分
//!
//! 本模块只做「URL → 本地路径」的**纯**转换（零 gpui 依赖，可单测）；
//! 事件注册与意图投递在 [`crate::ui::shell::runtime`]（与 argv、二实例转发
//! 共用同一条意图通道）。

use percent_encoding::percent_decode_str;

/// URL 列表 → 本地路径列表（**保序**；非 `file://` 或解不出的条目丢弃）。
///
/// 丢弃而非报错是刻意的：系统可能把 `http(s)://` 之类的 URL 一并投来
/// （`CFBundleURLTypes` 未声明时不该出现，但事件是外部输入），一个坏条目
/// 不该让整批打开动作陪葬。
pub fn paths_from_urls(urls: &[String]) -> Vec<String> {
    urls.iter().filter_map(|u| path_from_url(u)).collect()
}

/// 单个 `file://` URL → 本地路径。
///
/// 接受 `file:///path`（空授权段）与 `file://localhost/path` 两种形态；
/// 其余主机名（`file://host/share`）不是本地文件，返回 `None`。
fn path_from_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    // 授权段为空（`file:///a` → rest 以 `/` 开头）或 `localhost` 才算本地。
    let path = match rest.split_once('/') {
        Some(("", tail)) => format!("/{tail}"),
        Some(("localhost", tail)) => format!("/{tail}"),
        _ => return None,
    };
    let decoded = percent_decode_str(&path).decode_utf8().ok()?;
    Some(decoded.into_owned())
}

#[cfg(test)]
#[path = "tests/open_event.rs"]
mod tests;
