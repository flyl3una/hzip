//! 界面字体可用性探测：缺中文字形时把界面回退英文（用户口径 2026-10-12）。
//!
//! # 为什么需要
//!
//! `[ui].lang` 默认 `system`，而产品只维护 zh-CN / en 两套文案（02 §12.2）。
//! 在**没装中文字体**的机器上（干净的 Linux 容器 / 最小发行版最常见），中文
//! 文案会逐字渲染成 `.notdef`（豆腐块）：用户既读不懂、也看不出该装什么。
//! 此时「回退英文」是本项目选定的兜底——至少可识别、可用。
//!
//! # 判定方式：直接问文本系统，不猜字体族名
//!
//! 字体族名与字形覆盖没有必然关系（`Noto Sans` 有拉丁，汉字要 `Noto Sans
//! CJK SC` 这类才覆盖）。这里做的是**一次真实整形**：用界面当前主题字体整形
//! 一个探针字符，结果里出现 `GlyphId(0)`（`.notdef`）即「这个字形没有」。
//!
//! Linux 走 cosmic-text、Windows/macOS 走各自系统整形器，但 `GlyphId(0)` 的
//! 语义一致。**整形器本身失灵**时（连拉丁字母也判「缺字形」）结论无效——
//! [`cjk_available`] 用 `None` 表达「测不出来」，调用方据此**不做**回退：
//! 宁可维持原语言，也不能因为探测失灵把中文用户踢成英文。
//!
//! # 只回退「自动」语言
//!
//! `[ui].lang = "zh-CN"` 是用户的显式选择 → 尊重它（只记一条告警，讲清装什么
//! 字体）。只有 `system`（含脏值）这条**自动**路径会回退英文。
//!
//! # 先例：Bandizip 也不打包字体（2026-10-12 核对官方 changelog/help）
//!
//! 同类产品 Bandizip 的做法是**只用系统字体**：Windows 版 v7.33「对话框字体改用
//! `MS Shell Dlg 2`」（Windows 的逻辑字体别名，按系统区域设置映射到本地 UI 字体，
//! CJK 由系统 FontLink 补齐）、v7.32「树控件与压缩包注释的字体可改」＝把字体交给
//! 用户与系统；其 changelog（Windows 1284 行 / macOS 785 行）与 help 索引里**没有
//! 任何字体打包/嵌入条目**。它不需要「缺字体换语言」这一步，只是因为它只在
//! Windows/macOS 上发行，而这两个系统自带 CJK 字体（雅黑/Malgun Gothic/Meiryo、
//! 苹方）。本项目同路线（系统字体 + 引擎回退 + `[ui].font_family` 可改），
//! 差异只在 Linux——干净的 Linux 上可能一款中文字体都没有，故补这一步降级。
//!
//! # 扩展
//!
//! 现在只有 zh-CN 需要「特定字形」判定（en 用拉丁，各处必有）。将来若新增需要
//! 其它脚本的语言（日文/韩文/阿拉伯文……），在 [`cjk_available`] 旁加一个同形状
//! 的探针并在 [`decide`] 前把该语言也纳入「先探后判」即可——探针字符与语言标签
//! 一一对应，其余逻辑不变。

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Font, Hsla, TextRun, WindowTextSystem, px};

/// 需要「能渲染中文字形」的界面语言（产品只维护 zh-CN / en，见 02 §12.2）。
const ZH: &str = "zh-CN";
/// 中文字形探针（取最常用的汉字）。
const CJK_PROBE: &str = "中";
/// 拉丁字形探针：探测机制自证用（见 [`cjk_available`]）。
const LATIN_PROBE: &str = "A";
/// 探针字号（只影响整形度量，与字形覆盖判定无关）。
const PROBE_SIZE: f32 = 16.;

/// 用界面当前字体整形 `sample`，判断是否真的拿到了字形。
///
/// - `Some(true)`：拿到了（`GlyphId != 0`）；
/// - `Some(false)`：只拿到 `.notdef`（`GlyphId(0)`）——这个字符渲染不出来；
/// - `None`：整形结果里一个字形都没有——结论不可信。
///
/// `shape_line` 挂在窗口级文本系统（`WindowTextSystem`）上：探测不需要真窗口，
/// 用 `App` 的文本系统现建一个句柄即可（同一套平台后端与字体库，结论与真渲染一致）。
fn glyphs_available(cx: &App, sample: &str) -> Option<bool> {
    let run = TextRun {
        len: sample.len(),
        font: Font {
            family: cx.theme().font_family.clone(),
            ..Font::default()
        },
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let text_system = WindowTextSystem::new(cx.text_system().clone());
    // 单行（探针是常量，不含换行）——`shape_line` 对含换行的输入会 panic。
    let line = text_system.shape_line(sample.into(), px(PROBE_SIZE), &[run], None);
    let mut glyphs = line.runs.iter().flat_map(|run| run.glyphs.iter());
    let first = glyphs.next()?;
    Some(first.id.0 != 0 && glyphs.all(|glyph| glyph.id.0 != 0))
}

/// 界面字体能否渲染中文字形：`None` = 探测不可信（见 [`glyphs_available`]）。
pub fn cjk_available(cx: &App) -> Option<bool> {
    // 自证：连拉丁字母都判「缺字形」⇒ 这台文本系统给不出可信结论，不下任何判断。
    if glyphs_available(cx, LATIN_PROBE) != Some(true) {
        return None;
    }
    glyphs_available(cx, CJK_PROBE)
}

/// 纯策略：给定「`[ui].lang` 原值 + 已落地语言 + 中文字形探测结果」，返回实际
/// 应生效的语言；`None` = 不改（维持 `init_locale` 的落地结果）。
fn decide(lang: &str, locale: &str, cjk: Option<bool>) -> Option<&'static str> {
    if locale != ZH {
        return None; // 界面不是中文 → 与中文字形无关
    }
    match cjk {
        // 测不出来 / 中文可渲染 → 不动。
        None | Some(true) => None,
        // 显式选了中文 → 尊重用户选择（只告警，不擅自改语言）。
        Some(false) if lang.trim() == ZH => None,
        // 自动语言 + 缺字形 → 回退英文，避免整屏豆腐块。
        Some(false) => Some("en"),
    }
}

/// 缺中文字形 → 回退英文（仅**自动**语言；见模块文档）。返回本次是否发生回退。
pub fn apply_cjk_guard(cx: &App, lang: &str) -> bool {
    let locale = crate::current_locale();
    let cjk = if locale == ZH {
        cjk_available(cx)
    } else {
        None
    };
    if locale == ZH {
        // 「界面为什么是英文」的第一现场：把探测结论留痕（GUI 基线 debug）。
        tracing::debug!(lang = %lang, probe = ?cjk, "界面字体中文字形探测");
    }
    match decide(lang, &locale, cjk) {
        Some(next) => {
            crate::init_locale(next);
            tracing::warn!(
                probe = ?cjk,
                lang = %lang,
                "本机没有能渲染中文字形的字体：界面语言自动回退英文（[ui].lang={lang}）。\
                 装一款中文字体后重启即可恢复中文：Linux `apt install fonts-noto-cjk` / \
                 `dnf install google-noto-sans-cjk-fonts`，Windows 装「微软雅黑」，macOS 自带「苹方」。"
            );
            true
        }
        None => {
            if cjk == Some(false) {
                tracing::warn!(
                    "界面语言被显式设为 zh-CN，但本机没有能渲染中文字形的字体：界面会显示为豆腐块。\
                     装一款中文字体后重启即可（推导见 02 §12.2）。"
                );
            }
            false
        }
    }
}

#[cfg(test)]
#[path = "tests/fonts.rs"]
mod tests;
