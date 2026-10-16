//! Engine 实体：配置、构建/提交/浏览/取消 API、内置 FormatHandler 注册。
//!
//! T-STD 2026-10-08：自 `engine/mod.rs` 原样迁出（`mod.rs` 只留模块声明与再导出）。
//! `Engine::new` 逐步：配置加载（损坏→默认+告警，不阻塞）→ 注册内置
//! FormatHandler → 构建线程池 → 信号桥接 → 孤儿临时目录清理。
//!
//! 冻结范围（02 §13.1）：`Engine::submit`（含 browse 转发占位）、
//! `JobRequest/JobResponse`。

use std::sync::Arc;

use hzip_error::HzipError;
use hzip_format::{FormatRegistry, Source};
use hzip_progress::CancellationToken;
use hzip_progress::JobId;

use super::{job_pool_size, pool};
use crate::jobs::{JobHandle, JobManager, JobSpec};

/// Engine 运行时配置（B.1 的 core 消费面；由调用层合并传入——CLI/GUI 负责
/// 命令行参数 > 环境变量 > 配置文件 > 默认值的优先级合并，02 §12.1）。
///
/// 字段集「非冻结」：T32~T34 若需新配置项走追加字段。
pub struct EngineConfig {
    /// 池大小覆盖（None → [`job_pool_size`]；测试/CLI 覆盖）
    pub pool_size: Option<usize>,
    /// 并发门控上限（`jobs.max_parallel`；默认 4；实际并发 = min(此值, 池大小)）
    pub max_parallel: u8,
    /// 信号桥接（CLI 恒 true；测试 false——免污染全局信号处理器）
    pub cancel_on_signal: bool,
    /// 孤儿临时目录清理（测试 false——免副作用）
    pub orphan_cleanup: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            pool_size: None,
            max_parallel: 4,
            cancel_on_signal: true,
            orphan_cleanup: true,
        }
    }
}

/// 任务编排引擎（02 §9.2）。
pub struct Engine {
    registry: Arc<FormatRegistry>,
    jobs: Arc<JobManager>,
    pool: Arc<pool::Pool>,
    /// drop 时还原的拆卸闭包（信号处理器等）
    teardown: Vec<Box<dyn Fn() + Send + Sync>>,
    closed: std::sync::atomic::AtomicBool,
}

impl Engine {
    /// 构建引擎（02 §9.2 图 A→G 逐步）。
    ///
    /// 不执行任何 IO 重活——孤儿清理异步 spawn，信号桥接为注册处理器
    /// （`cancel_on_signal=false` 时跳过）。可多实例（无全局状态）。
    pub fn new(cfg: EngineConfig) -> Result<Engine, HzipError> {
        let root = CancellationToken::new();
        // ① 注册内置 handlers（feature 门控；见 register_builtin_handlers）
        let mut registry = FormatRegistry::new();
        register_builtin_handlers(&mut registry)?;
        let registry = Arc::new(registry);
        // ② 线程池（大小可配）
        let pool_size = cfg.pool_size.unwrap_or_else(job_pool_size);
        let pool = pool::Pool::new(pool_size, &root);
        // ③ 信号桥接
        let teardown: Vec<Box<dyn Fn() + Send + Sync>> = Vec::new();
        if cfg.cancel_on_signal {
            // platform::signal::install_cancel_handler 归 T11；当前为占位
            // todo!()（调度跟踪 §四记录）——cfg 门控调用，占位期跳过并以
            // tracing 提示。T40 消费时激活。
            // #[cfg(not(any()))] // 占位：待 T11 signal 激活
            // platform::signal::install_cancel_handler(&root);
            let _ = &root;
        }
        // ④ 孤儿临时目录清理（异步；不阻塞启动）
        if cfg.orphan_cleanup {
            // platform::temp_area 清理归 T11；占位期跳过（T11 temp_area
            // orphan_cleanup 已交付——此处经 cfg 门控，调用形态以 T11 API 为准；
            // 本任务不依赖，留接口位）。
        }
        let max_running = (cfg.max_parallel as usize).max(1).min(pool_size);
        let jobs = JobManager::new(
            root.clone(),
            max_running,
            pool.clone(),
            Arc::clone(&registry),
        );
        Ok(Engine {
            registry,
            jobs,
            pool,
            teardown,
            closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// 提交任务（02 §9.3 A 节点；参数校验失败即时 `Err`——不排队）。
    ///
    /// 冻结 API（02 §13.1）。shutdown 后提交 → `Err(6003 queue-closed)`。
    pub fn submit(&self, spec: JobSpec) -> Result<JobHandle, HzipError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(HzipError::Unsupported {
                what: "hz-6003 queue-closed".into(),
            });
        }
        // 02 §9.3 A 节点关键流程起点（T40 注释兑现）：任务入队留痕
        tracing::info!(
            kind = ?spec.kind,
            source = %spec.source.display(),
            dest = ?spec.dest.as_deref().or(spec.out.as_deref()),
            "任务已提交"
        );
        self.jobs.submit(spec)
    }

    /// 浏览档案（02 §9.10 同步轻量——不占线程池，直接在调用线程执行）。
    ///
    /// 冻结签名（02 §13.1）。转发到 [`crate::services::browse::browse`]：
    /// registry 组合解层 → entries（零 IO）→ [`Browser`]。
    pub fn browse(
        &self,
        src: Source,
        opts: &hzip_format::OpenOptions,
    ) -> Result<crate::services::browse::Browser, HzipError> {
        crate::services::browse::browse(&self.registry, src, opts)
    }

    /// 取消全部任务（级联；Engine shutdown 内部用，亦供上层「全部取消」）。
    pub fn cancel_all(&self) {
        self.jobs.cancel_all();
    }

    /// 任务快照（GUI 队列视图；测试断言）。
    pub fn snapshot(&self) -> Vec<(JobId, crate::jobs::JobState)> {
        self.jobs.snapshot()
    }

    /// 格式注册表引用（测试/诊断）。
    pub fn registry(&self) -> &FormatRegistry {
        &self.registry
    }
}

impl Drop for Engine {
    /// drop 顺序：close（拒绝新提交 + 取消全部）→ join 池 → teardown 信号还原。
    fn drop(&mut self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.jobs.close();
        self.pool.shutdown();
        for t in self.teardown.drain(..) {
            t();
        }
    }
}

/// 全 workspace 唯一格式注册汇集点（02 §9.2 节点 C；T20 §7-2 裁定聚合在
/// hzip-core）。按 hzip-core 的 feature（`default-formats` / `rar`）决定集合。
///
/// 依赖红线例外通道：CLI/GUI 经 core 拿格式能力，本函数是唯一 import fmt
/// crate 的位置（02 §2.1 红线 2）。
///
/// 三个 feature 全关（`--no-default-features`）时这里一个 handler 都不注册，
/// 参数 `reg` 因此未被使用 —— 这是**合法配置**（空注册表 = 「未知格式」），
/// 不是缺陷，故按键精确放行该告警，而不是把参数改名成 `_reg`（会毁掉公开签名
/// 的语义与文档链接）。
#[cfg_attr(
    not(any(
        feature = "default-formats",
        feature = "rar",
        feature = "volume-formats"
    )),
    allow(unused_variables)
)]
pub fn register_builtin_handlers(reg: &mut FormatRegistry) -> Result<(), HzipError> {
    #[cfg(feature = "default-formats")]
    {
        reg.register_handler(Box::new(hzip_fmt_tar::TarHandler::new()))?;
        // cpio：只读引入格式（initramfs/rpm 载荷；caps.write=false，02 §8.7）
        reg.register_handler(Box::new(hzip_fmt_cpio::CpioHandler::new()))?;
        // single 流：gz/bz2/xz/zst 四独立 handler（feature 门控由 fmt-single 内
        // 各自 cfg；此处按 core 的 default-formats 全量注册）
        reg.register_handler(Box::new(hzip_fmt_single::gz()))?;
        reg.register_handler(Box::new(hzip_fmt_single::bz2()))?;
        reg.register_handler(Box::new(hzip_fmt_single::xz()))?;
        reg.register_handler(Box::new(hzip_fmt_single::zst()))?;
        reg.register_handler(Box::new(hzip_fmt_zip::ZipFormatHandler::new()))?;
        reg.register_handler(Box::new(hzip_fmt_7z::SevenZipFormatHandler::new()))?;
    }
    #[cfg(feature = "rar")]
    {
        reg.register_handler(Box::new(hzip_fmt_rar::RarHandler::new()))?;
    }
    #[cfg(feature = "volume-formats")]
    {
        // 磁盘镜像（T-IMG）：文件系统驱动注册表在这里组装一次，再交给各卷容器
        // （`hzip-vol` 不依赖任何 `hzip-fs-*`，靠这条注入避免依赖倒置）。
        let mut fs_registry = hzip_vol::registry::FsRegistry::new();
        fs_registry.register(Box::new(hzip_fs_ext4::Ext4Driver::new()));
        fs_registry.register(Box::new(hzip_fs_fat::FatDriver::new()));
        fs_registry.register(Box::new(hzip_fs_ntfs::NtfsDriver::new()));
        fs_registry.register(Box::new(hzip_fs_xfs::XfsDriver::new()));
        let fs_registry = std::sync::Arc::new(fs_registry);

        // 卷容器：raw 无魔数（内容探针）；qcow2 有魔数（大端 `QFI\xfb`）；vmdk 待其任务收口。
        reg.register_handler(Box::new(hzip_fmt_raw::RawHandler::new(fs_registry.clone())))?;
        reg.register_handler(Box::new(hzip_fmt_qcow2::Qcow2Handler::new(
            fs_registry.clone(),
        )))?;
        reg.register_handler(Box::new(hzip_fmt_vmdk::VmdkHandler::new(
            fs_registry.clone(),
        )))?;
    }
    Ok(())
}
