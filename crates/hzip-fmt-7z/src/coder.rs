//! lzma/lzma2/BCJ2 路由与参数映射（02 §8.2 coder 行、§6.3/§6.4；T25 §4.4）。
//!
//! # 解码路由（sevenz-rust 0.6.1 能力面）
//!
//! 单流 folder（coder 链 `[LZMA2]` / `[LZMA]` / `[COPY]`）与 BCJ2 folder
//! （`[BCJ2, 4×LZMA]`）均由 `sevenz_rust::Archive::read` 解析、`BlockDecoder`
//! 整块解码（sevenz-rust 自带 BCJ2 解码器 `bcj2::BCJ2Reader`，见 `reader` 模块
//! rustdoc）。本模块提供：
//!
//! - [`lzma2_options`]：`hzip_codec::level::lzma2_params`（02 §6.3 表第 3 列）→
//!   sevenz-rust LZMA2 编码选项（写侧用）。
//! - [`folder_codec_id`]：folder 主压缩方法 → `CodecId`（条目 `codec` 列）。
//!
//! # `hzip-codec::filter::Bcj2Decoder` 接入边界（rustdoc 记录，T46 复核）
//!
//! sevenz-rust 0.6.1 **公开 API 不导出 folder 级四流原始读取**：BCJ2 folder 的
//! `[BCJ2, 4×LZMA]` 子流由 `SevenZReader::build_decode_stack2`（`reader.rs` 私有）
//! 装配并喂给其内置 `BCJ2Reader`；`Coder` 字段（`decompression_method_id`/
//! `properties`）与 `Folder.packed_streams`/`bind_pairs` 公开，但**子流解码输出
//! （4 个 LZMA 解压后的流）不对外暴露**。因此本基础版：
//!
//! - BCJ2 读路径经 sevenz-rust 内置解码（可解 7-Zip 产物 `TC-X-001` 的
//!   lzma2+BCJ2 样本——本 crate 测试嵌入 7-Zip 产 BCJ2 档案验证）；
//! - `hzip_codec::Bcj2Decoder` 的四流重组接口（main/call/jump/rc）语义与
//!   sevenz-rust 内置一致，供 T46 在「子流装配公开」或「自组装 folder」路线下
//!   对称接入；本模块将四流顺序语义写死如下（7-Zip `BCJ2_NUM_STREAMS`）：
//!   `0=main`、`1=call`、`2=jump`、`3=rc`（T12 `Bcj2Decoder::new` 参数序）。
//!
//! # 级别 → dict 映射现状注记（T25 §4.4，02 §6.3 线程规则）
//!
//! - **store/copy**：sevenz-rust 0.6.1 写路径 `encoders::add_encoder` 只支持
//!   LZMA/LZMA2（无 COPY 分支）；02 §6.3 表 `Store → copy` 在本基础版以
//!   **LZMA2 最小合法字典 4096 B** 近似（rustdoc 记录偏差，T46 评估字节级 store）。
//! - **多线程**：sevenz-rust compress 路径为**单线程** lzma2；`threads` 经
//!   [`crate::writer`] `clamp_threads` 记录并传递（Options 无该字段，仅记录），
//!   实际吞吐并行由 core 多任务/多条目调度补偿（非 solid 逐条目 folder 天然可分）。
//!   多线程 lzma2 编码列为 T46 评估项（02 §13.2 风险「sevenz-rust BCJ2 支持
//!   不确定」的写侧收口同址）。

use hzip_codec::level::Lzma2Params;
use hzip_codec::level::{Level, Mode};

/// LZMA2 最小合法字典（lzma-rust `DICT_SIZE_MIN`）。
pub(crate) const LZMA2_DICT_MIN: u32 = 4096;
/// LZMA2 最大写入字典（lzma-rust 上限语义 `u32::MAX & !15`；保守取 256 MiB，
/// 02 §6.3 Ultra 行）。
pub(crate) const LZMA2_DICT_MAX: u32 = 256 * 1024 * 1024;

/// 消费 02 §6.3 表的统一映射 → sevenz-rust LZMA2 编码选项。
///
/// sevenz-rust 的 `LZMA2Options` 基于 lzma-rust：`with_preset` 同时决定
/// `mode`（Fast≤3 / Normal≥4）与匹配深度等参数；本函数从 preset 起步后仅覆写
/// `dict_size`（02 §6.3 dict 列）。`mode` 字段类型为 lzma-rust 私有枚举（未再导出），
/// 故无法跨 crate 构造 Fast mode 对象——以 preset 决定 mode 是等价路径。
///
/// `dict_override`（UI「字典大小」）：`Some` 时**取代**级别派生的字典（这正是该
/// 选项的意义——用户要在级别之外单独调字典）；`None` = 随级别派生。
///
/// 特殊值：`dict == 0`（Store 行）→ 以 [`LZMA2_DICT_MIN`]（4096）近似（见模块
/// rustdoc）；超上限 → 截到 [`LZMA2_DICT_MAX`]（lzma-rust 合法域）。
pub(crate) fn lzma2_options(
    level: Level,
    _threads: u32,
    dict_override: Option<u64>,
) -> sevenz_rust::lzma::LZMA2Options {
    let p = lzma2_params_for(level);
    let preset = match p.mode {
        Mode::Fast => 1,                // fast 行（Fastest）→ preset 1（mode Fast）
        Mode::Copy | Mode::Normal => 6, // normal 行 → preset 6（mode Normal）
    };
    let mut opts = sevenz_rust::lzma::LZMA2Options::with_preset(preset);
    let raw = dict_override.unwrap_or(p.dict);
    let dict = if raw == 0 {
        LZMA2_DICT_MIN // Store 行（copy）近似
    } else {
        (raw as u32).clamp(LZMA2_DICT_MIN, LZMA2_DICT_MAX)
    };
    opts.dict_size = dict;
    // 线程：sevenz-rust 单线程实现；threads 仅记录于 writer（rustdoc 注记）。
    let _ = _threads;
    opts
}

fn lzma2_params_for(level: Level) -> Lzma2Params {
    hzip_codec::level::lzma2_params(level)
}

/// folder 主压缩方法 → `CodecId`（02 §6.3 压缩编码列）。
///
/// sevenz folder 链可为过滤器+压缩器（如 BCJ2 + 4×LZMA）；本函数扫描该 folder 的
/// 全部 coder 返回「压缩性方法」（LZMA2/LZMA/COPY 之一，LZMA2 优先），过滤器
/// （BCJ/BCJ2/DELTA/BCJx）不参与 codec 标识。未知 → None（`entries()` 的
/// `codec` 列）。`Folder`/`Coder` 类型在 sevenz-rust 中为 crate 私有不可具名，仅经
/// 公开字段/方法经类型推导访问（rustdoc 注记）。
pub(crate) fn folder_codec_id(
    archive: &sevenz_rust::Archive,
    folder_index: usize,
) -> Option<hzip_codec::traits::CodecId> {
    let folder = archive.folders.get(folder_index)?;
    let mut lzma: Option<hzip_codec::traits::CodecId> = None;
    for coder in folder.coders.iter() {
        let id = coder.decompression_method_id();
        match id {
            sevenz_rust::SevenZMethod::ID_LZMA2 => return Some("lzma2".into()),
            sevenz_rust::SevenZMethod::ID_LZMA => {
                lzma = Some("lzma".into());
            }
            sevenz_rust::SevenZMethod::ID_COPY if lzma.is_none() => {
                lzma = Some("store".into());
            }
            _ => {}
        }
    }
    lzma
}

/// 逐文件的 `CodecId`（`stream_map.file_folder_index` → folder codec）。
pub(crate) fn entry_codec_ids(
    archive: &sevenz_rust::Archive,
) -> Vec<Option<hzip_codec::traits::CodecId>> {
    archive
        .files
        .iter()
        .enumerate()
        .map(|(id, _)| {
            archive
                .stream_map
                .file_folder_index
                .get(id)
                .copied()
                .flatten()
                .and_then(|fi| folder_codec_id(archive, fi))
        })
        .collect()
}

/// BCJ2 四流顺序（7-Zip 容器子流序；T12 `Bcj2Decoder::new` 参数序）。
///
/// 写死接口语义供 T46 对称实现：
/// `0 = main`（主体字节流，LZMA 解压后）、`1 = call`、`2 = jump`、`3 = rc`。
///
/// 注：这些常量仅作**接口契约记录**（rustdoc/测试引用），本基础版经 sevenz-rust
/// 内置解码不直接装配子流——保持 doc 语义可达（T46 复核启用）。
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const BCJ2_STREAM_MAIN: usize = 0;
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const BCJ2_STREAM_CALL: usize = 1;
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const BCJ2_STREAM_JUMP: usize = 2;
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const BCJ2_STREAM_RC: usize = 3;

#[cfg(test)]
#[path = "tests/coder.rs"]
mod tests;
