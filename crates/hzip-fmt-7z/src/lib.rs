//! hzip-fmt-7z：7z 格式的 `FormatHandler`（02 §8.2 头解析/写入/编辑；T25 基础版）。
//!
//! # 职责（02 §8.2 / T25）
//!
//! - **头解析**：32 B 签名头 → nextHeader 偏移 CRC → 头加密 flag → streamsInfo /
//!   foldersInfo → BCJ2 判定 → entries 构建（绑定 sevenz-rust `Archive::read`，
//!   逐分支语义见 02 §8.2 图）。**数据区不读**（红线 1：`entries()` 大档案秒开，
//!   TC-CONF-010）。
//! - **lzma2 读写**：非固实——逐条目独立 folder（solid 块打包归 T46；本基础版
//!   等效 `solid=false`）。
//! - **级别 → dict 映射**：消费 `hzip_codec::level::lzma2_params`（02 §6.3 表）
//!   转 sevenz-rust LZMA2 编码选项（见 [`coder::lzma2_options`]）。
//! - **头回写两遍法**：`Sink::Stdout` 无 seek → 先落临时文件完成 `finish()`，
//!   再顺序拷出 stdout（02 §8.2 关键边界；临时文件磁盘需求 ≈ 档案最终大小）。
//! - **读侧卷集装配**：`Source::Volumes` → 卷字节串联（缺失 → `Format
//!   MissingVolume` HZ-2005，TC-X-005 读半场）。
//! - **BCJ2 解码**：sevenz-rust 0.6.1 内置 BCJ2 folder 解码（4×LZMA 子流 + 重组），
//!   经其 folder 级 API 消费。本 crate `hzip-codec::filter::Bcj2Decoder` 的接入
//!   边界见 [`coder`] rustdoc（子流级原始流不在 sevenz-rust 公开 API 面，T46 复核）。
//!
//! # caps 声明（02 §7.2 / T25 §1）
//!
//! | 操作 | caps | 行为 |
//! |---|---|---|
//! | read | true | sevenz-rust 头解析 + folder 物化缓存 |
//! | write | true | lzma2，逐条目独立 folder |
//! | append | false | `create_writer(append=true)` → `Err Unsupported` HZ-5001（TC-CONF-005） |
//! | edit | true | [`SevenZipEditor::rewrite`] 流式重写（`ChangeSet`） |
//! | encryption | None | AES/头加密读写归 T46；命中头加密 → `Password{Header}` HZ-3102 |
//! | split | false | 写侧分卷归 T46（读侧卷装配在 reader 内） |
//! | solid | false | 逐条目 folder（solid 归 T46） |
//! | comment | false | 7z 注释未启用 |
//!
//! # 错误映射（`reader::map_err` 统一，T25 §4.2 表）
//!
//! | sevenz-rust 错误 | HzipError | 码 |
//! |---|---|---|
//! | `Io` | `Io` | 1xxx |
//! | `BadSignature` | `Format{BadSignature}` | 2001 |
//! | `NextHeaderCrcMismatch` / `BadTerminated*` / `ExternalUnsupported` / `Other` | `Format{BadHeader}` | 2004 |
//! | `UnsupportedVersion` | `Format{UnknownMethod}` | 2003 |
//! | `UnsupportedCompressionMethod` / `MaxMemLimited` / `Unsupported` | `Format{UnknownMethod}` | 2003 |
//! | 数据区解码 io 错误（含 sevenz 内部 CRC 校验抛错——其丢末块不暴露 actual，见 `reader` rustdoc） | `Format{Truncated}`/`Format{BadHeader}` | 2002/2004 |
//! | 物化自校验 CRC 不符（干净 EOF 达成；期望值来自头 subStreamsInfo） | `Checksum` | 3001 |
//! | 头加密且无密码 `PasswordRequired` | `Password{Header}` | 3102 |
//! | 条目加密且无密码 | `Password{Archive}` | 3101 |
//!
//! 出处标注：02 §7.3 契约 trait、02 §8.2 语义、T25 实施要点。模块结构
//! （T25 §4.1）：`handler`/`reader`/`coder`/`writer`/`editor`。
//!
//! # 与 sevenz-rust API 的能力对齐与偏差（T25 §4.2 注、04 §3.1）
//!
//! - **写路径**：sevenz-rust `SevenZWriter` 要求 `Write + Seek`；每条目以
//!   `push_archive_entry` 提交（内部压缩立即完成），天然**逐条目独立 folder**。
//!   目录条目 = 空流（`has_stream=false, is_directory=true`）。
//! - **store/copy 语义**：sevenz-rust 0.6.1 写路径只支持 LZMA/LZMA2 编码器
//!   （`encoders::add_encoder` 无 COPY 分支）；02 §6.3 表中 `Store → copy`
//!   在本基础版以 **LZMA2 最小合法字典（4 KiB）** 近似（见 [`coder::lzma2_options`]
//!   rustdoc）。若需字节级 store 需自定义 COPY folder 头（T46 评估）。
//! - **symlink**：7z 容器无独立链接目标字段，7-Zip/sevenz-rust 以「目标字符串作为
//!   条目内容 + unix 属性 `S_IFLNK`」表达。写侧将链接目标作为数据写入
//!   （`unix_mode = S_IFLNK | mode` 置于 7z windows_attributes 高 16 位）；
//!   读侧以属性还原 `is_symlink`/`mode`，`symlink_target` 由消费方经
//!   `read_entry` 取得（头解析不触碰数据区，02 §9.6 净化归 core）。
//! - **BCJ2**：解码经 sevenz-rust 内置；`hzip-codec` `Bcj2Decoder` 四流原始接入
//!   需要子流装配（sevenz-rust 公开 API 未导出），见 [`coder`] rustdoc（T46 复核）。
//! - **多线程 lzma2 编码**：sevenz-rust compress 为单线程实现（02 §6.3 线程规则
//!   现状差距）；`threads` 经 [`coder::lzma2_options`] 记录，基础版并行由 core
//!   多条目调度补偿（逐条目 folder 天然可分），rustdoc 注记于 T25 §4.4。
//! - **头加密分支不实现解密**：`aes256` 功能未开启（workspace 根 features 含
//!   `aes256` 故编译启用，但本 crate 读路径以 `Password::empty()` 打开——命中头加密
//!   时 sevenz-rust 报 `PasswordRequired`，本层映射 HZ-3102，**零条目名泄露**）。
//!   真正 AES 解密/加密写归 T46。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![forbid(unsafe_code)]

mod coder;
mod editor;
mod handler;
mod reader;
mod writer;

#[cfg(feature = "aes")]
mod header_crypt;
#[cfg(feature = "solid")]
mod solid;
#[cfg(feature = "split")]
mod split;

pub use handler::SevenZipFormatHandler;

/// 7z 格式 ID（02 §7.2）。
pub const FORMAT_ID: &str = "7z";

/// 模块内常量（02 附录 B.2：不散落魔法数字）。
pub(crate) mod consts {
    /// folder 物化落点阈值：**头声明的** folder 解出总量 ≤ 该值留内存，超限则
    /// 边解边写临时文件（`reader::materialize` 的**先验**判定）。
    ///
    /// （T25 §7-5：RSS < 512 MiB 语义；`SOLID_BLOCK_DEFAULT_MIB=64` 归 T46。）
    /// 注意判据是**先验总量**而非「攒完再看」：旧实现在解码后判 `block.len()`，
    /// 阈值只对驻留生效、对**峰值**无效——400 MiB 固实块取单条目实测峰值 449 MiB
    /// （2026-09-29 修正）。
    ///
    /// 与 `FolderCache` 的 LRU 容量（`MAX_MATERIALIZED = 2`）共同界定 RSS 上界
    /// ≈ 2 × 该值 + 解码/写缓冲——**与档案解出总量无关**。
    pub(crate) const FOLDER_CACHE_MEM_MAX: usize = 64 * 1024 * 1024;
    /// 物化循环取消检查点间隔（02 §4.2：每 1 MiB `ctx.checkpoint` 一次）。
    pub(crate) const MATERIALIZE_CHECKPOINT_BYTES: u64 = 1024 * 1024;
    /// 条目级写入内存缓冲上限（超出切临时文件；防大条目内存峰值，同 T22）。
    pub(crate) const ENTRY_BUFFER_MEM_MAX: u64 = 4 * 1024 * 1024;
    /// 拷贝缓冲（输出两遍法 stdout 拷出 / 流间拷贝）。
    pub(crate) const COPY_BUF_BYTES: usize = 64 * 1024;
}

/// 私有临时文件（两遍法/物化溢出用）：即弃文件，`Drop` 尽力删除。
///
/// 落在 `std::env::temp_dir()` 下唯一子目录内；目录随 struct drop 尽力
/// `remove_dir_all` 清理。Windows 上若有外部仍持路径打开的句柄，删除会静默
/// 失败（临时残留，OS 级临时目录语义）。
pub(crate) struct TempArea {
    pub(crate) dir: std::path::PathBuf,
}

impl TempArea {
    /// 创建唯一临时工作目录。
    pub(crate) fn new(tag: &str) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "hzip-7z-{tag}-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// 在临时目录内创建/打开一个文件（write 句柄）。
    pub(crate) fn create_file(&self, name: &str) -> std::io::Result<std::fs::File> {
        let p = self.dir.join(name);
        std::fs::File::create(p)
    }

    /// 目录路径（供再次打开只读句柄）。
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.dir
    }

    /// 目录内路径。
    pub(crate) fn join(&self, name: &str) -> std::path::PathBuf {
        self.dir.join(name)
    }
}

impl Drop for TempArea {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
#[path = "tests/lib_rs.rs"]
mod tests;
