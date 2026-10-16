//! 应用/进程级模块（T-GUI 结构调整：自 ui/ 迁出——零 gpui 依赖面）。
//!
//! 不变量：**本目录零 gpui 依赖**（UI 无关的进程/业务基础设施）；
//! ui/ 内全部模块真依赖 gpui_kit。依赖方向：`ui/` → `app/` 单向
//! （UI 消费业务数据面），`app/` 不 import `ui/`
//! （`state.rs` 的 `contracts`/`binding` 引用为 UI 侧派生输入，见各文件说明）。
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`instance`] | 单实例锁 + 二实例 argv 转发 |
//! | [`exit`] | 退出钩子（2 s 宽限 + TempArea 清理） |
//! | [`pump_host`] | 后台任务泵线程宿主 |
//! | [`file_picker`] | rfd 原生文件/目录/可执行选择器 |
//! | [`state`] | AppData（engine/jobs/router/设置偏好业务面） |
//! | [`settings_registry`] | 设置注册表（T-SET：设置项清单 + load/store/effect + 活读视图） |
//! | [`labels`] | label↔value 白名单（UI 文案 ↔ config 落盘值） |
//! | [`open_event`] | macOS「打开方式」Apple Event 的 URL → 路径转换 |

pub mod exit;
pub mod external_edit;
pub mod file_picker;
pub mod instance;
pub mod labels;
pub mod open_event;
pub mod password_cache;
pub mod pump_host;
pub mod settings_registry;
pub mod state;
pub mod ui_lag;
