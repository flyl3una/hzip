//! zipx 方法解码路由表（02 §8.1 `zipx` 行；T45；feature `zipx` 门控）。
//!
//! # 路由表（§3.3 代码化；const 表驱动，禁散落魔法数字）
//!
//! | 方法 ID | 方法 | 解码途径 | 本路由标注 |
//! |---|---|---|---|
//! | 0 | store | zip crate 默认 | `Native` |
//! | 8 | deflate | zip crate `deflate` | `Native` |
//! | 9 | deflate64 | zip crate `deflate64`（读兼容，写永不，02 §13.2） | `Native` |
//! | 12 | bzip2 | zip crate `bzip2` | `Zipx("bzip2")`（feature `zipx`） |
//! | 14 | LZMA | zip crate `lzma` | `Zipx("lzma")`（feature `zipx`） |
//! | 93 | zstd | zip crate `zstd` | `Zipx("zstd")`（feature `zipx`） |
//! | 99 | AES 包封 | aes 模块 → real_method 二次路由 | `Aes` |
//! | 98 等 | PPMd 等 | — | `Unsupported` → HZ-5001（明确报错，TC-X-001 语义） |
//!
//! # feature 并集现实（如实登记）
//!
//! zip crate 的 bzip2/zstd 由根 `[workspace.dependencies]` feature 并集对所有成员
//! 常开（T24 注释同），lzma 由本 crate zip 依赖行显式开启——**三方法解码能力在
//! workspace 内不可按成员裁剪**。本模块的 feature `zipx` 门控作用于**路由判定与
//! 语义映射**（feature 关闭时 12/14/93 归 `UnknownMethod 2003` 显式拒绝而非静默
//! 解码）；`route()` 纯函数两侧皆可单测（`cfg` 双分支）。
//!
//! 98(PPMd) 等：zip crate 未启用 ppmd feature → 条目读取时
//! `UnsupportedArchive("Compression method not supported")` → 本 crate 错误映射
//! `Unsupported 5001`（02 附录 A；不产出乱码，TC-X-001 语义）。

// 路由表（02 §8.1/§3.3）是交付面：zip crate 读取已把 12/93 解码为
// CompressionMethod 变体（route 的运行时判定发生在 zip 内部 feature 分派），
// 本表用于标注/校验与 feature 关闭语义；lib 非测试构建不可达即 dead_code，
// 经单测（zipx_method_routing）锁定语义。不删除。
#![allow(dead_code)]

use hzip_codec::traits::CodecId;

/// 解码途径标注。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MethodRoute {
    /// 0/8/9：zip crate 默认（T24 已有）。
    Native,
    /// 12/14/93：zip crate 扩展 feature（feature `zipx` 下标注 CodecId）。
    #[cfg(feature = "zipx")]
    Zipx(CodecId),
    /// 99：AES 包封 → aes 模块（实际读取经 zip crate aes-crypto，路由语义标注）。
    Aes,
    /// 98(PPMd) 等已知但不可解方法 → HZ-5001 明确报错。
    Unsupported(&'static str),
}

/// 方法 ID → 解码途径（§3.3 表代码化）。
pub(crate) fn route(method: u16) -> MethodRoute {
    match method {
        0 | 8 | 9 => MethodRoute::Native,
        99 => MethodRoute::Aes,
        // 98 PPMd 等已知方法族：明确 Unsupported（5001）而非 UnknownMethod
        98 => MethodRoute::Unsupported("PPMd (98)"),
        12 | 14 | 93 => {
            #[cfg(feature = "zipx")]
            {
                let codec: CodecId = match method {
                    12 => "bzip2".into(),
                    14 => "lzma".into(),
                    93 => "zstd".into(),
                    _ => unreachable!("路由表仅 12/14/93"),
                };
                MethodRoute::Zipx(codec)
            }
            #[cfg(not(feature = "zipx"))]
            {
                let _ = method;
                // feature 关闭：显式 UnknownMethod 语义由上层以 2003 报出
                // （见 [`route_known_zipx`] 的 cfg 分支）
                MethodRoute::Unsupported("zipx 方法 (feature zipx 未启用)")
            }
        }
        // 其余（1..7 shrink/reduce、10 implode、95 xz…）：不可解 → Unsupported 5001
        other => MethodRoute::Unsupported(match other {
            95 => "xz (95)",
            94 => "MP3 (94)",
            20 => "zstd 旧编号 (20)",
            97 => "WavPack (97)",
            96 => "JPEG (96)",
            _ => "未知压缩方法",
        }),
    }
}

/// feature `zipx` 关闭时对 12/14/93 的显式拒绝（HZ-2003 UnknownMethod 语义）。
/// 该判定在 feature 关闭路径被 entries 标注采用（解码实际不可达——zip crate 在
/// workspace 并集下仍可解；标注层如实拒绝，行为差异见模块文档「feature 并集现实」）。
#[cfg(not(feature = "zipx"))]
pub(crate) const fn zipx_disabled() -> bool {
    true
}

/// 供 entries 标注 `EntryInfo.codec` 的映射（zip crate 方法变体已在 feature 并集
/// 下可判；此处仅把 99/98 归 None——AES 条目 codec 应为 real_method，由 zip crate
/// 解析 0x9901 后以真实方法暴露（reader 见 `codec_of`），本路由为纯标注兜底）。
#[cfg(test)]
pub(crate) fn route_test_entries() -> Vec<(u16, MethodRoute)> {
    vec![
        (0, route(0)),
        (8, route(8)),
        (9, route(9)),
        (99, route(99)),
        (98, route(98)),
    ]
}

#[cfg(test)]
#[path = "tests/zipx.rs"]
mod tests;
