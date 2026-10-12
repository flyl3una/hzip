//! Engine：任务编排入口（02 §9.2 初始化流程 + §9.3 提交；冻结 API 面）。
//!
//! **本文件只做模块声明与再导出**（T-STD 2026-10-08 用户口径：`mod.rs` 不写业务
//! 实现）——实体在 [`core`]（`Engine`/`EngineConfig`/内置 FormatHandler 注册），
//! 线程池在 [`pool`]。
//!
//! `Engine::new` 逐步：配置加载（损坏→默认+告警，不阻塞）→ 注册内置
//! FormatHandler（[`register_builtin_handlers`]，全 workspace 唯一格式注册
//! 汇集点）→ 构建线程池（[`pool::JOB_POOL_SIZE`]，可配）→ 信号桥接 →
//! 孤儿临时目录清理（异步，不阻塞启动）。
//!
//! 冻结范围（02 §13.1）：`Engine::submit`（含 browse 转发占位）、
//! `JobRequest/JobResponse`。`JobSpec/JobKind/EngineConfig/JobState` 为本
//! crate 新公开类型——字段集「非冻结」，T32~T34 追加 services 所需参数。

pub(crate) mod pool;

mod core;

pub use core::{Engine, EngineConfig, register_builtin_handlers};
pub use pool::job_pool_size;
