//! 文件名编码修复：chardetng 批量检测 + 转码 + Suspect 标记（02 §8.1 `encoding`
//! 行；T45；feature 无关）。
//!
//! # 批量检测流程（02 §8.1 图）
//!
//! ```text
//! 读取条目名 → UTF-8 flag 置位? → 是: Utf8 直接用
//!                        ↓ 否
//!             ExtractCtx.codepage 指定（--codepage）?
//!                        ↓ 是          ↓ 否
//!                  强制指定转码      chardetng 批量检测（全部非 UTF-8 名为样本）
//!                  （优先级最高）           ↓ 置信度 ≥ 0.8
//!                                    转码 → Utf8（原始字节保留于报告）
//!                                          ↓ 低置信
//!                                    保留原始 + Suspect(codepage)（不乱猜，TC-CORE-016）
//! ```
//!
//! 本模块只做名字字节 → UTF-8 的**表征层**转换（02 §7.3 codepage 字段消费方）；
//! 净化（穿越/保留名）归 core 安全模块（T31/T32，02 §9.6）——**编码修复不是安全
//! 边界**（任务卡 §7-6）。
//!
//! # 置信度口径（与 chardetng 0.1.17 实证对齐）
//!
//! 02 §8.1/任务卡 §4.6 要求 `guess_assess` 返回 `(Encoding, f64)` 且阈值 0.8——
//! 但 chardetng 0.1.17 实际 API 为 `guess_assess(tld, allow_utf8) ->
//! (&'static Encoding, bool)`，`bool` 语义是「最高分候选是否高于至少一个其他
//! 候选」（true 仍可能低分，见其 lib.rs）。**f64 置信度不存在**；任务卡示例签名
//! 与锁定依赖 API 不符。本模块的裁决（登记依据 chardetng 0.1.17 源码）：
//!
//! - 以 chardetng 自评分 `score` 的量纲换算置信度不可行（内部非线性权重，
//!   `find_score` 为 testing-only feature）——采用**保守代理**：`guess_assess`
//!   的 `assess==false`（无竞争候选或得分低）→ 低置信拒绝（保留原始 + Suspect）；
//!   `assess==true` → 高置信接受。配合第二重保守条件「样本须含非 ASCII 且为
//!   检测集合（GBK/Big5/Shift_JIS/EUC-KR/CP1252 家族）解码合法」双重把关。
//! - 长样本（任务卡 TC-CORE-015 高置信 GBK 名）assess=true 确定修复；2 字节短
//!   样本（TC-CORE-016）经 CJK 候选 `longest_word < 2` 拒分（chardetng 内部
//!   score 过滤）→ assess 假/低 → 保留原始 + `Suspect(猜测)` 不乱猜。实测见
//!   [`detect_batch`] 单测。
//!
//! 常量 [`CONFIDENCE_*`] 保留为与 02 §8.1「0.8」语义对齐的命名占位（换算见上）。

use hzip_format::{Encoding, NameEncoding};

/// 02 §8.1 阈值常量（0.8）；实际换算见模块文档「置信度口径」。
/// （命名对齐 02 文档的 0.8 语义锚点；检测实现按 assess 判定。）
#[allow(dead_code)]
pub(crate) const CONFIDENCE_THRESHOLD: f64 = 0.8;

/// 批量检测结果（编码 + 判定强度）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatchGuess {
    /// 检测出的编码（GBK/Big5/Shift_JIS/EUC-KR/CP1252 家族或 UTF-8）。
    pub(crate) enc: Encoding,
    /// 高置信（assess 判定通过）——调用方转码为 Utf8；
    /// 低置信——保留原始 + Suspect(enc)。
    pub(crate) confident: bool,
}

/// 批量检测入口：samples = 全部非 UTF-8 flag 条目的原始名字节。
///
/// 实现：逐样本 `feed(s, true)` 终结喂入（短样本分段喂入会让候选得分被稀释，
/// 逐条终结使每条自成检测单元；但 chardetng 单实例 feed 多次 last=true 非法
/// （panic「Must not feed again after feeding with last」）——故每个样本独立
/// 检测器，汇总取「多数高置信」或首条高置信。
pub(crate) fn detect_batch(samples: &[&[u8]]) -> Option<BatchGuess> {
    let mut best: Option<(Encoding, bool)> = None;
    for s in samples {
        let mut d = chardetng::EncodingDetector::new();
        d.feed(s, true);
        let (enc, assess) = d.guess_assess(None, true);
        // 只认检测集合内的家族（chardetng 可返回 KOI8/ISO-8859-* 等——02 §8.1
        // D 节点限定 GBK/Big5/SJIS/EUC-KR/CP1252 + UTF-8）
        let Some(hz) = hzip_format::Encoding::from_charset(enc.name()) else {
            continue;
        };
        let non_ascii = s.iter().any(|&b| b >= 0x80);
        if !non_ascii {
            continue;
        }
        // 双把关：assess 且可解码合法 → 高置信
        let decodes = transcode(s, hz).is_some();
        let confident = assess && decodes;
        match best {
            Some((_, true)) => {}
            Some((_, false)) => {
                if confident {
                    best = Some((hz, true));
                }
            }
            None => best = Some((hz, confident)),
        }
    }
    best.and_then(|(enc, confident)| confident.then_some(BatchGuess { enc, confident }))
}

/// 转码：encoding_rs decode（UTF-8 表征）。原始字节由调用方保留进报告
/// （02 §8.1 F 节点；本函数不丢原始信息）。
pub(crate) fn transcode(raw: &[u8], enc: Encoding) -> Option<String> {
    let e = to_encoding_rs(enc)?;
    let (cow, _, had_errors) = e.decode(raw);
    if had_errors {
        return None;
    }
    Some(cow.into_owned())
}

/// encoding_rs 静态编码 ↔ hzip-format Encoding。
fn to_encoding_rs(e: Encoding) -> Option<&'static encoding_rs::Encoding> {
    match e {
        Encoding::Gbk => Some(encoding_rs::GBK),
        Encoding::Big5 => Some(encoding_rs::BIG5),
        Encoding::ShiftJis => Some(encoding_rs::SHIFT_JIS),
        Encoding::EucKr => Some(encoding_rs::EUC_KR),
        Encoding::Cp1252 => Some(encoding_rs::WINDOWS_1252),
        Encoding::Utf8 => Some(encoding_rs::UTF_8),
        // #[non_exhaustive]（T20 冻结）：未来扩展编码未知 → None（调用方回退保留原始）
        _ => None,
    }
}

/// 检测结果 → 名字 + NameEncoding 标注（02 §7.2）。
///
/// 优先级（02 §8.1 强制链）：
/// 1. `forced`（--codepage 手动指定）最高：直接转码；非法字节序列 → 保留原始
///    + `Suspect(forced)`（不 panic，不产出乱码名）；
/// 2. 无 forced：原始即合法 UTF-8 → `Utf8`；
/// 3. 其余：batch 高置信 → 转码 + `Suspect(enc)`（02 §7.2 注释：T45 批量修复后
///    置为确定值——但 NameEncoding 冻结枚举只有 Utf8/Suspect/Unknown 三态；
///    **不新增公开 API（T20）** → 修复结果以 Suspect(enc) 标注，rustdoc 注明
///    表征层语义：name 字段已是正确 UTF-8 字符串，Suspect 携带源编码供报告）；
/// 4. 低置信/无批量 → 保留原始字节 + Suspect(猜测 codepage)（TC-CORE-016 不乱猜）。
///
/// 返回 `(条目名 str, NameEncoding)`；`name` 经 [`parse_entry_name`] 语法校验
/// （N1..N5，02 §7.2）——原始字节转出的非法名（控制字符/NUL 等）保留原始形态
/// 由 parse 失败方处理（调用方归 BadHeader 或 Suspect 保留，视上下文）。
pub(crate) fn classify(
    raw: &[u8],
    forced: Option<Encoding>,
    batch: Option<BatchGuess>,
) -> (String, NameEncoding) {
    // 强制链最高优先级
    if let Some(f) = forced {
        if let Some(s) = transcode(raw, f) {
            return (s, NameEncoding::Suspect(f));
        }
        return (lossy(raw), NameEncoding::Suspect(f));
    }
    // 合法 UTF-8 → 直接用
    if let Ok(s) = std::str::from_utf8(raw) {
        return (s.to_string(), NameEncoding::Utf8);
    }
    // 批量高置信 → 转码
    if let Some(b) = batch {
        if b.confident
            && let Some(s) = transcode(raw, b.enc)
        {
            return (s, NameEncoding::Suspect(b.enc));
        }
        // 低置信：保留原始 + Suspect（不乱猜；TC-CORE-016）
        return (lossy(raw), NameEncoding::Suspect(b.enc));
    }
    // 无批量信息：原始保留 + Unknown（T24 基础版语义）
    (lossy(raw), NameEncoding::Unknown)
}

/// 原始字节损失型字符串化（cp437 直映语义，与 zip crate 逐字节 → char 对齐）。
fn lossy(raw: &[u8]) -> String {
    raw.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
#[path = "tests/encoding.rs"]
mod tests;
