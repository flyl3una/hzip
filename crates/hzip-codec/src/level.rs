//! 统一 `Level` 与各压缩库参数映射表（02 §6.3，表驱动实现）。
//!
//! 表中「—」列（zstd / bzip2 的 Store）语义：该 codec 在 `Level::Store` 下不可用，
//! 档案层改走 [`crate::store`] 恒等路径；本 crate 的映射函数仍按表值返回
//! （zstd 0 级、bzip2 0 块）供诊断，但调用方不应在 Store 级使用二者。

use crate::consts;

/// 统一压缩级别（02 §6.3 表逐行）。`Default = Normal`。
///
/// 非法数值级别（如 10）在本层不可表达（枚举闭合）；HZ-5003 的拒绝点在
/// Engine/CLI 层（TC-ALG-008），本 crate 不重复该用例。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Level {
    /// 存储（不压缩；zstd / bzip2 不可用）
    Store,
    /// 最快
    Fastest,
    /// 快
    Fast,
    /// 普通（默认）
    #[default]
    Normal,
    /// 最大
    Maximum,
    /// 极限
    Ultra,
}

/// lzma2 压缩模式（02 §6.3 表「7z lzma2 dict」列中的 mode；T25 消费）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// copy（字典 0，即存储语义）
    Copy,
    /// fast（快速模式）
    Fast,
    /// normal（普通模式）
    Normal,
}

impl Mode {
    /// 稳定标识（UI 提示行 / 诊断；**永不本地化**）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Fast => "fast",
            Self::Normal => "normal",
        }
    }
}

/// lzma2 参数对（`dict` 字节 + `mode`），供 hzip-fmt-7z（T25）装配 LZMA2 编码器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lzma2Params {
    /// 字典大小（字节）
    pub dict: u64,
    /// 压缩模式
    pub mode: Mode,
}

impl From<(u64, Mode)> for Lzma2Params {
    fn from((dict, mode): (u64, Mode)) -> Self {
        Self { dict, mode }
    }
}

/// zip deflate 级别列（02 §6.3）：`Store→0, Fastest→1, Fast→3, Normal→6, Maximum|Ultra→9`。
pub fn deflate_level(l: Level) -> u32 {
    match l {
        Level::Store => 0,
        Level::Fastest => 1,
        Level::Fast => 3,
        Level::Normal => 6,
        Level::Maximum | Level::Ultra => 9,
    }
}

/// 7z lzma2 dict/mode 列（02 §6.3）：字典尺寸见 [`crate::consts`]。
pub fn lzma2_params(l: Level) -> Lzma2Params {
    match l {
        Level::Store => (consts::LZMA2_DICT_STORE, Mode::Copy),
        Level::Fastest => (consts::LZMA2_DICT_FASTEST, Mode::Fast),
        Level::Fast => (consts::LZMA2_DICT_FAST, Mode::Normal),
        Level::Normal => (consts::LZMA2_DICT_NORMAL, Mode::Normal),
        Level::Maximum => (consts::LZMA2_DICT_MAXIMUM, Mode::Normal),
        Level::Ultra => (consts::LZMA2_DICT_ULTRA, Mode::Normal),
    }
    .into()
}

/// xz preset 列（02 §6.3）：`Store→0, Fastest→1, Fast→3, Normal→6, Maximum|Ultra→9`。
pub fn xz_preset(l: Level) -> u32 {
    match l {
        Level::Store => 0,
        Level::Fastest => 1,
        Level::Fast => 3,
        Level::Normal => 6,
        Level::Maximum | Level::Ultra => 9,
    }
}

/// xz extreme 位（9e 列）：仅 `Level::Ultra` 置位。
pub fn xz_extreme(l: Level) -> bool {
    matches!(l, Level::Ultra)
}

/// zstd 级别列（02 §6.3）：`Store→0, Fastest→1, Fast→5, Normal→9, Maximum→16, Ultra→22`。
pub fn zstd_level(l: Level) -> i32 {
    match l {
        Level::Store => 0,
        Level::Fastest => 1,
        Level::Fast => 5,
        Level::Normal => 9,
        Level::Maximum => 16,
        Level::Ultra => 22,
    }
}

/// bzip2 block 列（02 §6.3）：`Store→0, Fastest→1, Fast→3, Normal→6, Maximum|Ultra→9`。
pub fn bzip2_level(l: Level) -> u32 {
    match l {
        Level::Store => 0,
        Level::Fastest => 1,
        Level::Fast => 3,
        Level::Normal => 6,
        Level::Maximum | Level::Ultra => 9,
    }
}

/// 线程规则（02 §6.3）：`threads = min(请求值, 可用核数)`。
///
/// `requested == 0` 视为「自动」（映射结果为 0，各库以 0 表示 auto/全部核）。
pub fn clamp_threads(requested: u32) -> u32 {
    requested.min(
        std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1),
    )
}
