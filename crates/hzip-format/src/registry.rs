//! 格式注册表与组合装配（02 §7.1/§7.5/§7.6；契约冻结）。
//!
//! [`FormatRegistry`]：`register_handler` → `sniff` → `open_reader`/
//! `create_writer`/`open_editor` → `Box<dyn trait 对象>`。core 启动时按 feature
//! 注册各 handler（02 §7.6 时序），此后只操作 trait 对象。
//!
//! **组合装配**（`.tar.gz` 类）：对 `Composite` 嗅探结果，`open_reader` 从原始
//! `Source` 逐层 wrap 解码器（外→内）得到已解压流，再以 `Source::Stream` 交给
//! 最内层容器 handler（tar/zip/7z/rar）。内层 reader 自身负责条目定位与乱序
//! 读取（组合链顺序流：`read_entry` 乱序时从流起点重扫定位，02 §7.5 要点②的
//! 代价语义由 fmt 实现承担，rustdoc 注明）。本层只做**委托**与**装配**。
//!
//! **写入方向**：组合写（`TarWriter→GzEncoder→File`）由 core 转换管线组装
//! （T33），本 registry 不直接提供；`create_writer` 需按已嗅探单格式直开。

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use hzip_error::HzipError;
use smol_str::SmolStr;

use crate::caps::FormatCaps;
use crate::editor::ArchiveEditor;
use crate::handler::{FormatHandler, OpenOptions, Sink, Source};
use crate::password::PasswordRef;
use crate::reader::{ArchiveReader, ExtractCtx, ExtractReport, Selection};
use crate::sniffer::{CompositeTail, Sniffed, sniff_impl};
use crate::types::{EntryId, EntryInfo};
use crate::writer::ArchiveWriter;
use crate::{CreateOptions, FormatId};

/// 格式注册表（02 §7.6）。
pub struct FormatRegistry {
    handlers: Vec<Box<dyn FormatHandler>>,
    index: HashMap<FormatId, usize>,
}

impl Default for FormatRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// 注册 handler（id 重复 → `Err Unsupported`）。
    pub fn register_handler(&mut self, h: Box<dyn FormatHandler>) -> Result<(), HzipError> {
        let id = h.id();
        if self.index.contains_key(&id) {
            return Err(HzipError::Unsupported {
                what: format!("duplicate handler {id}").into(),
            });
        }
        self.index.insert(id.clone(), self.handlers.len());
        self.handlers.push(h);
        Ok(())
    }

    /// 按 id 查找 handler。
    pub fn handler(&self, id: &str) -> Option<&dyn FormatHandler> {
        self.index
            .get(&SmolStr::from(id))
            .and_then(|&i| self.handlers.get(i))
            .map(|b| b.as_ref())
    }

    /// 已注册格式 id 列表。
    pub fn ids(&self) -> Vec<FormatId> {
        self.handlers.iter().map(|h| h.id()).collect()
    }

    /// 全部 handler 的遍历访问（R2 T-G52：GUI 按读能力派生扩展名过滤清单——
    /// 注册表为单一事实来源，GUI 不硬编码）。
    pub fn handlers(&self) -> impl Iterator<Item = &dyn FormatHandler> {
        self.handlers.iter().map(|b| b.as_ref())
    }

    /// 嗅探源格式（02 §7.4 全流程）。
    pub fn sniff(&self, src: &Source) -> Result<Sniffed, HzipError> {
        sniff_impl(src)
    }

    /// 打开读取器（02 §7.5）：`Single` 直开；`Composite` 装配组合 reader。
    pub fn open_reader(
        &self,
        src: Source,
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        match sniff_impl(&src)? {
            Sniffed::Single(id, _) => self
                .handler(id.as_str())
                .ok_or_else(|| HzipError::Unsupported {
                    what: format!("format {id} 未注册").into(),
                })?
                .open_reader(src, opts),
            Sniffed::Composite { layers, tail, .. } => {
                let tail_id = match tail {
                    CompositeTail::Container(id) => id,
                    CompositeTail::Raw => {
                        return Err(HzipError::Unsupported {
                            what: "Raw 组合读取（多层单文件流）由 T23 fmt-single 接入".into(),
                        });
                    }
                };
                let handler =
                    self.handler(tail_id.as_str())
                        .ok_or_else(|| HzipError::Unsupported {
                            what: format!("format {tail_id} 未注册").into(),
                        })?;
                // 从原始源逐层解压 → 内层容器以 Stream 打开
                let stream = build_decoder_chain(&src, &layers)?;
                let inner = handler.open_reader(Source::Stream(stream), opts)?;
                Ok(Box::new(CompositeReader { inner }))
            }
        }
    }

    /// 创建写入器：调用方须先经 handler 直接创建（组合写由 core 转换管线组装）。
    pub fn create_writer(
        &self,
        dst: Sink,
        opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        let _ = (dst, opts);
        Err(HzipError::Unsupported {
            what: "请按已嗅探格式调用 handler(id).create_writer（组合写归 core 转换管线）".into(),
        })
    }

    /// 打开编辑器：单格式直开（`caps.edit` 才实现）。
    pub fn open_editor(
        &self,
        src: Source,
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        match sniff_impl(&src)? {
            Sniffed::Single(id, _) => self
                .handler(id.as_str())
                .ok_or_else(|| HzipError::Unsupported {
                    what: format!("format {id} 未注册").into(),
                })?
                .open_editor(src, opts),
            Sniffed::Composite { .. } => Err(HzipError::Unsupported {
                what: "组合格式不支持编辑".into(),
            }),
        }
    }
}

/// 由源 + 压缩层清单重建解码链（外→内逐层 wrap；02 §7.5 装配）。
fn build_decoder_chain(
    src: &Source,
    layers: &[FormatId],
) -> Result<Box<dyn Read + Send>, HzipError> {
    let base: Box<dyn Read + Send> = match src {
        Source::File(p) => Box::new(std::io::BufReader::new(
            std::fs::File::open(p).map_err(crate::map_io)?,
        )),
        Source::Volumes { first, .. } => Box::new(std::io::BufReader::new(
            std::fs::File::open(first).map_err(crate::map_io)?,
        )),
        Source::Stdin | Source::Stream(_) => {
            return Err(HzipError::Unsupported {
                what: "组合装配不支持 Stdin/Stream 源（需可重放的 File/Volumes）".into(),
            });
        }
    };
    let mut chain = base;
    for f in layers {
        chain = crate::sniffer::wrap_layer_decoder(f.as_str(), chain).map_err(|_| {
            HzipError::Format {
                format: f.clone(),
                kind: hzip_error::FormatErrorKind::BadHeader,
            }
        })?;
    }
    Ok(chain)
}

/// 组合读取器：内层容器 reader 已持有解码流，本层全权委托。
///
/// 乱序/重复 `read_entry`（红线 2）的代价语义（顺序流重扫）由内层实现承担
/// （02 §7.5 要点②；tar handler 以条目索引 + 重扫定位实现）。
struct CompositeReader {
    inner: Box<dyn ArchiveReader>,
}

impl ArchiveReader for CompositeReader {
    fn caps(&self) -> FormatCaps {
        self.inner.caps()
    }
    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
        self.inner.entries()
    }
    fn read_entry(
        &self,
        id: EntryId,
        pw: PasswordRef<'_>,
    ) -> Result<Box<dyn Read + Send>, HzipError> {
        self.inner.read_entry(id, pw)
    }
    fn extract(
        &self,
        sel: &Selection,
        ctx: &ExtractCtx<'_>,
        dest: &Path,
    ) -> Result<ExtractReport, HzipError> {
        self.inner.extract(sel, ctx, dest)
    }
    fn test(&self, ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
        self.inner.test(ctx)
    }
}

#[cfg(test)]
#[path = "tests/registry.rs"]
mod tests;
