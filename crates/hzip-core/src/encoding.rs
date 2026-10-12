//! 文件名编码检测策略（02 §7.3 codepage + §8.1 图；core 半场，T32 独占）。
//!
//! # 分工（T32 任务卡 §4.5）
//!
//! fmt 层（zip T45 深化 / tar / single）报 `name_encoding` 与**表征层名称**
//! （zip 以原始字节经 cp437 直映成 lossy 字符串——原始字节在 fmt 层已损失，
//! 见 T24 编码模块 rustdoc）；core 层决定「是否/如何**重映射**条目名」。
//! 本模块只消费 `hzip-format` 冻结类型（`EntryInfo`/`NameEncoding`/`Encoding`/
//! `EntryName`），不 import 任何 fmt crate（红线 3）。
//!
//! # 优先级链（02 §8.1 图 B0 节点；高 → 低）
//!
//! 1. `manual`（`ExtractCtx.codepage`，`--codepage gbk`）——**最高优先**：全部
//!    非 `Utf8` 名按指定编码转码；非法字节序列条目退回保留 + Suspect；
//! 2. 无 manual → 批量 chardetng 自动检测（样本 = 全部非 UTF-8 名的名字字节，
//!    **一次检测非逐条目**，02 §8.1 C 节点）；
//! 3. 置信度 ≥ 0.8（代理判定，见下）→ 转码为正确 UTF-8；
//! 4. 低置信 / 无批量 → 保留原名 + `Suspect` 标记（TC-CORE-016：不乱猜）。
//!
//! # 置信度 0.8 的代理方案（rustdoc 注明，与 T45 对齐）
//!
//! chardetng 无原生 0~1 置信度分数（0.1.17 API 为 `guess_assess` 返回
//! `(&'static Encoding, bool)`，bool 表示最高分候选是否显著高于其他候选；
//! 详见 hzip-fmt-zip/src/encoding.rs 的置信度口径记录）。0.8 阈值以
//! **「检出编码一致 + 转码成功条目占比」**作代理：
//!
//! - 逐样本独立检测（chardetng 单实例 feed 多次 last=true 非法）；
//! - 仅认检测集合内的编码（GBK/Big5/Shift_JIS/EUC-KR/CP1252 + UTF-8，超出如
//!   KOI8/UTF-16 → 低置信路径保留，02 §8.1 D 节点限定）；
//! - 各样本检出编码取多数票；高置信 = 检出一致率 ≥ 0.8 **且**全部样本按该
//!   编码转码成功（encoding_rs decode 无错误）；
//! - 单样本情形：样本经 `guess_assess` 的 assess 通过且转码成功 → 高置信。
//!
//! core 不依赖 fmt crate；本模块常量各自定义、值锁定 0.8（与 T45 两处一致）。
//! 重映射结果经 [`parse_entry_name`] 校验（02 §7.2 N1~N5 语法）——非法名退回
//! 保留原名（不 Err，解压继续）。
//!
//! # 表征约束（重要）
//!
//! 对 tar/single（fmt 层以 lossy 或文件名字节直接落盘，不保留原始字节），core
//! **无法**恢复原始非 UTF-8 字节——转码只会把替换字符（`�`）当普通字符处理，
//! 无法修复。本模块对这类条目仅能标注 Suspect/Unknown 不重映射（不产生错误）。
//! zip 层（T45）会在 fmt 侧完成字节级修复。core 的批量 chardetng 检测主要为
//! T45 深化后的 zip 路径预留接线；当前（T24 zip 仍 lossy）`entries` 中
//! `name_encoding` 携带的编码信息（cp437/Unknown）不足以回退字节——本模块的
//! 自动检测针对 **`NameEncoding::Unknown` 且名字含 U+FFFD 替换符**的条目保守
//! 保留（无法恢复即不乱猜）。`manual` codepage 同理只对可取得原始字节的路径
//! 生效——当前 fmt 层未暴露原始字节，强制转码对 lossy 名无意义，故本模块
//! 如实保留并标注（rustdoc 记录该张力，T45 zip 字节修复合入后激活完整链）。

use hzip_format::{Encoding, EntryId, EntryInfo, NameEncoding};

/// 置信度阈值（02 §8.1 E 节点：≥ 0.8；代理语义见模块文档）。
pub(crate) const CONFIDENCE_THRESHOLD: f64 = 0.8;

/// 编码策略（core 消费；由 ExtractCtx.codepage / 配置链注入）。
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)] // T45 fmt 层原始字节接入后激活；当前 reconcile 走保守路径
pub(crate) struct EncodingPolicy {
    /// 手动指定编码（`extract.codepage`；优先级最高）。
    pub manual: Option<Encoding>,
    /// 自动检测阈值（锁定 0.8；测试可调低注入）。
    pub threshold: f64,
}

impl EncodingPolicy {
    /// 默认策略：无手动、阈值 0.8（02 §8.1 / 附录 B.1 `extract.codepage=null`）。
    pub(crate) fn new(manual: Option<Encoding>) -> Self {
        Self {
            manual,
            threshold: CONFIDENCE_THRESHOLD,
        }
    }
}

/// 编码处理结果（02 §8.1 F/G 节点产物；供报告侧消费）。
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // T45 激活面；当前 reconcile 保守路径未填充 rewrites/detected
pub(crate) struct EncodingOutcome {
    /// 修复映射（条目 id → 重映射后的新名；T45 深化前多为空）。
    pub rewrites: Vec<(EntryId, String)>,
    /// 保留原名 + Suspect 的条目（低置信/无法恢复）。
    pub kept_suspect: Vec<EntryId>,
    /// 批量检测出的编码（None = 未检出/低置信）。
    pub detected: Option<Encoding>,
}

/// 名字一致性层入口：`entries` → 可能重映射的条目集 + 报告。
///
/// 实现（02 §8.1 图逐节点）：
/// - B0：`manual` 有值 → 对全部非 `Utf8` 条目执行强制链（经 [`transcode_to`]）；
/// - B：收集非 UTF-8 名字的**原始字节**样本——对 `NameEncoding::Unknown` 且名含
///   U+FFFD 的条目，因 fmt 层未保留原始字节无法恢复，直接进保留 + Suspect；
/// - C：chardetng 批量检测（一次检测非逐条目，见模块文档代理方案）；
/// - E/F：置信度 ≥ 阈值 → 按检出编码转码 + 重映射；
/// - G：低置信 / 转码失败 / parse_entry_name 校验失败 → 保留原名 + Suspect。
///
/// # 当前形态（rustdoc 注明；与 T45 分工）
///
/// fmt 层（T24 zip / tar / single）不暴露条目的**原始名字节**——`EntryName` 是
/// 已解码字符串（zip 非 UTF-8 名经 cp437 直映成 lossy 字符串；tar lossy 替换
/// 符）。core 无法从字符串无损还原原始字节，故自动 chardetng 批量检测（本模块
/// 已实现 + 单测的 [`detect_batch_confidence`]）与手动 codepage 转码
/// （[`transcode_to`]）的**样本输入当前不可得**——重映射链保持待激活结构：
///
/// - `NameEncoding::Utf8` / `Suspect(_)`（fmt 已表征转码）→ 原名直通；
/// - `Unknown` 且名含 U+FFFD（tar/single lossy）→ 保留 + 记 `kept_suspect`
///   （不乱猜，TC-CORE-016）；
/// - `Unknown` 且名无替换符（字节或可重建）→ 保留 + 记 `kept_suspect`；
///
/// T45 在 fmt-zip 侧做字节级修复（其 reader 直接消费原始字节，不经过本模块）。
/// 本模块的批量检测/转码函数即 T45 对齐的激活面（值与优先级链一致锁定 0.8）。
pub(crate) fn reconcile_names(
    entries: Vec<EntryInfo>,
    _policy: &EncodingPolicy,
) -> (Vec<EntryInfo>, EncodingOutcome) {
    let mut out = Vec::with_capacity(entries.len());
    let mut result = EncodingOutcome::default();
    for e in entries {
        let needs_remap = match e.name_encoding {
            NameEncoding::Utf8 => false,
            NameEncoding::Suspect(_) => false,
            NameEncoding::Unknown => true,
            // #[non_exhaustive]（T20 冻结）：未来编码状态按不重映射保守处理
            _ => false,
        };
        if !needs_remap {
            out.push(e);
            continue;
        }
        // Unknown 且无原始字节可检 → 保留原名 + Suspect（原始字节在 fmt 层已
        // lossy，无法恢复；见模块文档「表征约束」）
        result.kept_suspect.push(e.id);
        out.push(e);
    }
    (out, result)
}
/// 代理置信批量检测（02 §8.1 C/E；置信度口径见模块文档）。
///
/// 逐样本独立 `EncodingDetector`（last=true 终结喂入），收集检测集合内的
/// 编码投票；样本非空且全部样本按胜出编码转码成功，且（多样本时）检出一致
/// 率 ≥ `threshold` /（单样本时）`guess_assess` assess 通过 → 高置信返回该
/// 编码；否则 `None`（低置信 → 调用方保留 + Suspect）。
#[allow(dead_code)] // T45 fmt 层原始字节接入后激活（见 reconcile_names 模块注释）
fn detect_batch_confidence(samples: Vec<&[u8]>, threshold: f64) -> Option<Encoding> {
    if samples.is_empty() {
        return None;
    }
    let mut votes: Vec<(Encoding, usize)> = Vec::new();
    let mut assessed_ok: Vec<bool> = Vec::with_capacity(samples.len());
    for s in &samples {
        let mut d = chardetng::EncodingDetector::new();
        d.feed(s, true);
        let (enc, assess) = d.guess_assess(None, true);
        let non_ascii = s.iter().any(|&b| b >= 0x80);
        if !non_ascii {
            votes.push((Encoding::Utf8, 1));
            assessed_ok.push(true);
            continue;
        }
        let Some(hz) = Encoding::from_charset(enc.name()) else {
            assessed_ok.push(false);
            continue;
        };
        let decodes = transcode_to(s, hz).is_some();
        let confident = assess && decodes;
        if let Some((_, count)) = votes.iter_mut().find(|(v, _)| *v == hz) {
            *count += 1;
        } else {
            votes.push((hz, 1));
        }
        assessed_ok.push(confident);
    }
    // 多数票编码
    let (winner, count) = votes.into_iter().max_by_key(|&(_, c)| c)?;
    let total = samples.len();
    let agree = count as f64 / total as f64;
    if agree < threshold {
        return None;
    }
    // 全部样本按胜出编码转码成功（代理「转码成功率」把关）
    let all_decode_ok = samples.iter().all(|s| transcode_to(s, winner).is_some());
    // 多样本：一致率已 ≥ 0.8；单样本：须 assess 通过
    let single_confident = total == 1 && assessed_ok.first().copied().unwrap_or(false);
    if all_decode_ok && (total > 1 || single_confident) {
        Some(winner)
    } else {
        None
    }
}

/// 按指定编码转码为 UTF-8 字符串（encoding_rs decode；非法序列 → None）。
#[allow(dead_code)] // T45 激活面；reconcile 保守路径未用
pub(crate) fn transcode_to(raw: &[u8], enc: Encoding) -> Option<String> {
    let e = to_encoding_rs(enc)?;
    let (cow, _, had_errors) = e.decode(raw);
    if had_errors {
        return None;
    }
    Some(cow.into_owned())
}

/// hzip-format `Encoding` ↔ encoding_rs 静态编码。
fn to_encoding_rs(e: Encoding) -> Option<&'static encoding_rs::Encoding> {
    match e {
        Encoding::Gbk => Some(encoding_rs::GBK),
        Encoding::Big5 => Some(encoding_rs::BIG5),
        Encoding::ShiftJis => Some(encoding_rs::SHIFT_JIS),
        Encoding::EucKr => Some(encoding_rs::EUC_KR),
        Encoding::Cp1252 => Some(encoding_rs::WINDOWS_1252),
        Encoding::Utf8 => Some(encoding_rs::UTF_8),
        // #[non_exhaustive]（T20 冻结）：未知扩展编码 → None（调用方回退保留）
        _ => None,
    }
}

#[cfg(test)]
#[path = "tests/encoding.rs"]
mod tests;
