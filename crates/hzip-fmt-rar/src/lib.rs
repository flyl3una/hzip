//! hzip-fmt-rar：RAR 格式的只读 `FormatHandler`（02 §8.3；T26）。
//!
//! # 定位（02 §8.3 + T26 任务卡 §1）
//!
//! 基于 **unrar crate**（C++ unrar 源码静态绑定）实现 rar 格式**只读**
//! `ArchiveReader` 半场。rar4 / rar5 双版本、加密条目读取、卷集自动发现；
//! **创建 / 追加 / 编辑永不做**（D2 决策；unrar 源码许可亦禁止用于构造压缩器
//! ——合规红线，04 §9）：`create_writer` / `open_editor` 恒 `Err Unsupported`
//! HZ-5001。
//!
//! # caps（02 §7.2 / T26 §3.4）
//!
//! `read=true, write=false, append=false, edit=false, split=true（读侧卷集）,
//! solid=true（只读，固实块由 unrar 处理）, comment=false`；
//! `encryption = Aes(Bits256)` —— 读能力声明（rar5 数据加密为 AES-256；rar4
//! 专有算法加密的兼容读取同样经 unrar 路径完成，只是其密钥语义与 AES-256 不
//! 全同，此处 rustdoc 注明）。
//!
//! # C 边界三重防护（02 §8.3 / T26 §3.1 图）
//!
//! unrar C 库**非线程安全**（02 §13.2 风险项），且以 C++ 异常/越界方式暴露
//! 损坏输入时可能 panic。本 crate 对全部 unrar 调用做三重防护：
//!
//! 1. **全局互斥锁**（`panic_wall::UNRAR_MUTEX`）：串行化全部打开/列头/解包
//!    调用（`panic_wall::call_unrar`）；
//! 2. **catch_unwind 错误边界监控**：C 边界 panic 一律拦截，`tracing::error!`
//!    记录（错误边界监控），进程存活；
//! 3. **错误码→[`HzipError`] 映射表**：ERAR 码 → HZ 码（未知码 → `Format
//!    {BadHeader}` HZ-2004 + 原始码 Debug 附注），见 `panic_wall` 模块文档表。
//!
//! # read_entry 物化策略（契约红线 2 的 rar 特解，02 §7.3 / T26 §4.2）
//!
//! unrar C API 以文件为解包输出单元、不提供内存流——`read_entry(id, pw)` 返回
//! 的 `Box<dyn Read + Send>` 内部为 `reader::TempMaterialized`：持全局互斥经
//! `extract_to` 物化到**即弃临时文件**（std temp，`Drop` 清理）后返回该文件
//! 的读取流。乱序/重复读取天然独立（每次调用独立物化，TC-CONF-004）。磁盘
//! 上界 ≈ 条目解压大小（双写成本）。
//!
//! # 模块结构（02 §8.3 / T26 §4.1）
//!
//! | 子模块 | 职责 |
//! |---|---|
//! | `caps` | rar4/rar5 双魔数 `Signature` + 只读能力矩阵 |
//! | `reader` | unrar 绑定：`ArchiveReader` 实现、列头缓存、read_entry 物化、密码/加密分支、extract/test |
//! | `volume` | 卷集自动发现（新命名 `.partN.rar` glob / 旧命名 `.r00/.r01` 序列探测，缺失卷 HZ-2005） |
//! | `panic_wall` | C 边界三重防护（互斥 + catch_unwind + 错误码映射表） |

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod caps;
pub(crate) mod panic_wall;
mod reader;
pub(crate) mod volume;

use hzip_error::HzipError;
use hzip_format::{
    ArchiveEditor, ArchiveReader, ArchiveWriter, CreateOptions, FormatCaps, FormatHandler,
    FormatId, OpenOptions, Sink, Source,
};

/// RarHandler（02 §8.3）：只读处理器，无内部状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RarHandler;

impl RarHandler {
    /// 便捷构造（与 `Default` 同义；handler 无状态）。
    pub fn new() -> Self {
        Self
    }
}

impl Default for RarHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatHandler for RarHandler {
    fn id(&self) -> FormatId {
        FormatId::from(caps::FORMAT_ID)
    }

    fn signature(&self) -> &'static hzip_format::Signature {
        &caps::SIGNATURE
    }

    fn caps(&self) -> FormatCaps {
        caps::caps()
    }

    fn open_reader(
        &self,
        src: Source,
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        reader::RarReader::open(src, opts).map(|r| Box::new(r) as Box<dyn ArchiveReader>)
    }

    fn create_writer(
        &self,
        _dst: Sink,
        _opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        Err(HzipError::Unsupported {
            what: "rar 不支持压缩（D2：rar 创建能力永不做，HZ-5001；02 附录 A HZ-5001 行为示例即 rar）"
                .into(),
        })
    }

    fn open_editor(
        &self,
        _src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        Err(HzipError::Unsupported {
            what: "rar 不支持压缩与编辑（caps.edit=false，TC-CONF-009）".into(),
        })
    }
}

/// 便捷取 rar 格式 ID。
pub(crate) fn format_id() -> FormatId {
    FormatId::from(caps::FORMAT_ID)
}
