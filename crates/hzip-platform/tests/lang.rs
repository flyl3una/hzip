//! `hzip_platform::lang::system_language` 的形态断言（T-ICON 拍板3）。
//!
//! 本机语言未知（开发机/CI 各不相同），故只钉**形态**不钉具体值：探测成功必须
//! 给出可直接用作 locale 标签的结果，`None`（探测不到）同样是合法结果。具体
//! 探测值以 `--nocapture` 打印留档。

#![allow(clippy::print_stdout)]

use hzip_platform::lang::system_language;

/// 形态约束：非空、无 GNU locale 残留分隔符、首段 ASCII 字母、长度 ≤ 20。
///
/// 这些恰是 GUI 侧做「主标签 == en」判定的前提：带 `.`/`_` 的原文（`zh_CN.UTF-8`）
/// 会让 `split('-')` 取到 `zh_CN.UTF-8` 整段，判定失效。
fn assert_shape(tag: &str) {
    assert!(!tag.is_empty(), "探测结果不得为空串");
    assert!(
        !tag.contains('.') && !tag.contains('_'),
        "codeset / 地区分隔符应已归一为 `-`：{tag:?}"
    );
    let primary = tag.split('-').next().unwrap_or_default();
    assert!(
        !primary.is_empty() && primary.chars().all(|c| c.is_ascii_alphabetic()),
        "首段应为 ASCII 语言码：{tag:?}"
    );
    assert!(tag.len() <= 20, "语言标签长度超出预期：{tag:?}");
}

/// 形态 + 稳定性 + 不 panic（`system_language` 契约的三条）。
#[test]
fn detected_language_is_wellformed_and_stable() {
    let first = system_language();
    println!("detected = {first:?}");

    if let Some(tag) = &first {
        assert_shape(tag);
    }

    // 同一进程内环境变量与系统设置不会变：结果必须可重复（缓存/全局态才敢依赖它）。
    for round in 0..3 {
        assert_eq!(
            system_language(),
            first,
            "第 {} 次探测与首次不一致（探测必须稳定）",
            round + 2
        );
    }
}
