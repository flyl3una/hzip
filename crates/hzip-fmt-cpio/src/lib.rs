//! hzip-fmt-cpio：cpio 格式的 `FormatHandler`（02 §8.7，只读引入格式）。
//!
//! # 职责
//!
//! - **四变体兼容读取**：SVR4 `newc`（`070701`）/ `crc`（`070702`，数据字节和校验）
//!   / 旧可移植 ASCII `odc`（`070707`）/ 旧二进制 `bin`（`0x71C7`，LE 与 BE 两种字序）；
//! - **目录结构如实呈现**：目录/符号链接按头 `mode` 的类型位判定，
//!   `EntryInfo::symlink_target` 由数据区（有界）读出；
//! - **归档根的自我描述条目**（`find . | cpio -o` 写出的 `.`）不展示成条目
//!   ——Bandizip/7-Zip 同口径，否则标准工具产出的 cpio 一律多一行噪音。
//!
//! # 定位：与 rar 同档的**只读引入格式**
//!
//! 现实用例是解包 Linux initramfs（`newc`）/ rpm 载荷 / 发行版镜像；hzip 自身
//! 不产出 cpio（写侧无需求），故 `caps.write = false`、`append = false`、
//! `edit = false`，`create_writer`/`open_editor` 恒 `Err Unsupported`（HZ-5001）。
//!
//! # 组合形态（`.cpio.gz` 等）不在本 crate
//!
//! 压缩层归 registry 组合装配（02 §7.5）：嗅探到「gz + cpio 头」→
//! `Sniffed::Composite{layers:[gz], tail:Container(cpio)}` → registry 解层后以
//! [`Source::Stream`] 交给本 handler（[`CpioHandler::open_reader`] 把它 spool 成
//! 临时文件再随机访问）。前置条件是嗅探表认得 cpio 魔数
//! （`hzip-format::sniffer::MAGIC_TABLE`）与扩展名表列了 `.cpio`
//! （`hzip_format::exts::FORMAT_EXTS`）。
//!
//! # 错误映射（`variant::map_err` 统一）
//!
//! | io 错误 | HzipError | 码 |
//! |---|---|---|
//! | 记录内部截断（头/名字/数据区） | `Format{Truncated}` | 2002 |
//! | 坏头 / 域非法 / 变体不一致 / namesize 越界 | `Format{BadHeader}` | 2004 |
//! | 首块非四变体 | `Format{BadSignature}` | 2001 |
//! | `crc` 变体数据字节和不符（`test()`） | `Checksum` | 3001 |
//!
//! 模块结构：`caps`（能力矩阵与签名）/ `consts`（变体常量）/ `variant`（变体判定与
//! 头解析）/ `reader`（`ArchiveReader`）/ `spool`（Stream 源缓冲）。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod caps;
mod consts;
mod reader;
mod spool;
mod variant;

use hzip_error::HzipError;
use hzip_format::{
    ArchiveEditor, ArchiveReader, ArchiveWriter, CreateOptions, FormatCaps, FormatHandler,
    FormatId, OpenOptions, Sink, Source,
};

/// cpio 格式 ID（`"cpio"`；re-export 自模块 `caps`）。
pub use caps::FORMAT_ID;

/// cpio 处理器（`Send + Sync`，无内部状态）。
#[derive(Debug, Default, Clone, Copy)]
pub struct CpioHandler;

impl CpioHandler {
    /// 便捷构造（与 `Default` 同义；handler 无状态）。
    pub fn new() -> Self {
        Self
    }
}

impl FormatHandler for CpioHandler {
    fn id(&self) -> FormatId {
        FormatId::from(FORMAT_ID)
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
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        match src {
            Source::File(p) => reader::CpioReader::from_path(p, format_id()),
            // 一次性流 spool 成临时文件：cpio 的索引/乱序读都要 seek
            // （红线 1 跳数据区、红线 2 判 id 独立可用）
            Source::Stdin => {
                let sp = spool::spool_stream(std::io::stdin().lock())?;
                reader::CpioReader::from_path_spooled(sp.path().to_path_buf(), sp)
            }
            Source::Stream(s) => {
                let sp = spool::spool_stream(s)?;
                reader::CpioReader::from_path_spooled(sp.path().to_path_buf(), sp)
            }
            Source::Volumes { .. } => Err(HzipError::Unsupported {
                what: "cpio 无分卷（caps.split=false，02 §8.7）".into(),
            }),
        }
    }

    fn create_writer(
        &self,
        _dst: Sink,
        _opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        Err(HzipError::Unsupported {
            what: "cpio 只读（caps.write=false，02 §8.7）：hzip 不产出 cpio 档案，\
                   解压走 core 转换管线"
                .into(),
        })
    }

    fn open_editor(
        &self,
        _src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        Err(HzipError::Unsupported {
            what: "cpio 无编辑能力（caps.edit=false，02 §8.7；编辑走 core 转换管线）".into(),
        })
    }
}

/// 供 reader 的 `from_path_spooled` 用的格式 ID 便捷。
pub(crate) fn format_id() -> FormatId {
    FormatId::from(FORMAT_ID)
}

#[cfg(test)]
#[path = "tests/lib_rs.rs"]
mod tests;
