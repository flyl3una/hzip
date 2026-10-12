#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! hzip 图形前端 ViewModel 层（02 §11；T41）。
//!
//! # 架构分层（02 §11.1）
//!
//! gpui/gpui_kit 界面层·薄壳（T-G 系列）只负责渲染与事件；**业务逻辑全部在本层**：
//!
//! - [`viewmodel::MainViewModel`]：浏览数据面（条目/过滤/选择/树）——`Engine::browse`
//!   的 UI 投影（02 §9.10）；
//! - [`viewmodel::JobsViewModel`]：任务队列/进度/取消——订阅 `JobHandle` 事件流经
//!   10 Hz 节流合并后的状态聚合（02 §4.3 C2 消费者）；
//! - [`viewmodel`] 对话框与请求路由：压缩/解压参数 → `JobSpec`、密码三态状态机、
//!   冲突决策（含「全部应用」会话）——`JobRequest → JobResponse` 请求-应答
//!   状态机（02 §11.3 时序的 ViewModel 半场）。
//!
//! # 红线（02 §11.1 原文）
//!
//! - gpui/gpui_kit 组件内**零业务逻辑**；本层 **不 import gpui/gpui_kit 类型**（纯数据
//!   [`viewmodel::rows`]），保证 `cargo test -p hzip-gui --lib` 无图形环境可跑；
//! - UI 通知经纯 Rust 回调 trait（[`viewmodel::pump::VmNotifier`]），gpui_kit 侧绑定由
//!   T-G 系列桥接；本 crate lib 目标不含 gpui/gpui_kit 依赖。
//!
//! # 结构修正说明（05 §5 允许路径）
//!
//! T00 骨架只建了 bin（`src/main.rs`），未建 lib 目标。T41 卡隐含 lib 目标
//! （`cargo test -p hzip-gui --lib` 无图形可跑）——本文件按隐含结构补建
//! `[lib]`（Cargo.toml `name = "hzip_gui"`），`src/main.rs` 保持 bin 不受影响。

// rust-i18n 加载本 crate locales 目录（v2 格式单文件多语言；02 §12.2）。
rust_i18n::i18n!("locales", fallback = "zh-CN");

/// 失败原因/界面文案 locale（02 §12.2）；启动与语言设置变更时调用（幂等）。
///
/// # 取值
///
/// 本产品只维护 `en` / `zh-CN` 两套文案（02 §12.2 的取舍：语种数量按用户盘子，
/// 而不是按「理论上能翻多少」）：
///
/// - `"en"` / `"zh-CN"`：设置面板的显式选择，直接生效；
/// - 其余（含默认值 `"system"` 与任何脏值）：**真探测**系统语言
///   （[`hzip_platform::lang::system_language`]，T-ICON 拍板3），语言主标签为
///   `en` 走 `en`，**其余系统语言一律回退 `zh-CN`**——中文仍是产品默认，
///   探测不到时行为与「`system` 恒等 zh-CN」的旧实现一致。de/fr/ja 等语言
///   宁可给一个完整的中文界面，也不给半吊子的英文界面。
///
/// 测试进程里 locale 是**进程级全局**且会被「切语言」用例短暂改掉；
/// 未进入切语言临界区的用例由 [`i18n::tr`] 自动钉在 zh-CN，无需各自加锁。
pub fn init_locale(lang: &str) {
    let locale = match lang {
        "en" => "en",
        "zh-CN" => "zh-CN",
        _ => {
            // 只比主标签（`en-US`/`en-GB` → en）：产品只区分「英文环境」。
            let english = hzip_platform::lang::system_language().is_some_and(|tag| {
                tag.split('-')
                    .next()
                    .is_some_and(|p| p.eq_ignore_ascii_case("en"))
            });
            if english { "en" } else { "zh-CN" }
        }
    };
    rust_i18n::set_locale(locale);
}

/// 界面文案 i18n 收口（crate 根：三层都能用；见模块文档）。
mod i18n;
pub use i18n::{current_locale, locale_is, tr, tr_with};

pub mod viewmodel;

// 进程/应用级基础设施（零 gpui——单实例/退出钩子/后台泵/文件选择器/
// AppData 业务面/label↔value 白名单）。T-GUI 结构调整：自 ui/ 迁出；
// 不变量：`grep -rEn "use (gpui|gpui_kit)" src/app/` 必须为空。
pub mod app;

// T-G 系列：gpui_kit 薄壳装配层（gpui Application + 绑定/泵/单实例/退出钩子）。
// 本模块 import gpui_kit（T41 红线仅约束 viewmodel/ 目录）；`viewmodel/`
// 保持纯 Rust 无 gpui/gpui_kit，`cargo test -p hzip-gui --lib` 的 VM 测试无窗口可跑。
// 红线维持由验收 grep `! grep -rEn "use (gpui|gpui_kit)" src/viewmodel/` 机械把守。
pub mod ui;

// crate 根测试索引（T-STD 裁定 C）：跨模块集成冒烟清单（原 `src/ui/tests.rs`，
// 含 `#[ignore]` 占位）；各业务文件的单元测试在同目录 `tests/` 子目录下。
#[cfg(test)]
mod tests;
