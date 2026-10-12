//! 界面文案 i18n 收口（`Locales` 单一入口；02 §12.2）。
//!
//! # 为什么在 crate 根
//!
//! 文案分布在三层（`viewmodel` / `app` / `ui`），而分层红线是
//! `ui → app → viewmodel`。把 `tr` 放在 `app` 会让 `viewmodel` 反向依赖
//! `app`；放在 crate 根则三层都能用，且与既有的 `crate::init_locale` 同址
//! （locale 本身就是 crate 级全局）。
//!
//! # 约定
//!
//! - **key 用英文点分命名**（`所在表面.语义`，如 `menu.file`、`setting.theme.label`）；
//! - **`zh-CN` 值必须与改造前的中文硬编码逐字节一致**（改文案≠翻译：中文值漂移
//!   会连带改掉既有测试断言与用户预期）；
//! - **落盘值/匹配键/元素 id 永不经 i18n**——只有「给人看的字符串」才翻译
//!   （反例：`labels::Pair.1` 是 config 契约值，`WidgetKind` 的 key 是分组键）；
//! - **日志不翻译**（`tracing::*` 保持中文，见仓库日志约定）。
//!
//! # 占位符
//!
//! `tr_with` 做字面 `{name}` 替换（不做格式化语法解析）：文案里写
//! `"提交解压任务 {id}: {src}"`，调用处给 `&[("id", &id.to_string())]`。
//! 这样翻译与参数解耦，且不依赖 `t!` 宏对动态 key 的参数展开能力。
//!
//! # 缺失 key 的行为
//!
//! rust-i18n 查不到 key 时**原样返回 key**，界面上会出现 `menu.file` 这样的
//! 原文——测试 `every_declared_key_resolves_in_both_locales` 与
//! `source_keys_exist_in_locale_tables` 把这条挡在 CI 里。

/// 查表（动态 key；`&str` 变量亦可）。
///
/// 缺失 key → 返回 key 本身（rust-i18n 行为），由单测兜底。
///
/// **测试确定性**：locale 是进程级全局，而「切语言」用例会临时改成 en。
/// 测试构建下，**非**临界区线程的查表先取 locale 锁并把语言钉为 zh-CN 再查
/// （临界区线程按自己设定的语言查）——断言中文原文的用例因此绝不会被并发
/// 切换串扰，切语言用例照样能验证 en。见 `viewmodel::testutil::LOCALE_LOCK`。
/// 发布构建无此分支。
pub fn tr(key: &str) -> String {
    #[cfg(test)]
    if !crate::viewmodel::testutil::in_locale_critical_section() {
        let _pin = crate::viewmodel::testutil::LOCALE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        rust_i18n::set_locale("zh-CN");
        return rust_i18n::t!(key).to_string();
    }
    rust_i18n::t!(key).to_string()
}

/// 带占位符查表：把文案里的 `{name}` 逐字面替换。
///
/// 未在 `args` 中出现的占位符原样保留（便于发现漏传参数）。
pub fn tr_with(key: &str, args: &[(&str, &str)]) -> String {
    substitute(&tr(key), args)
}

/// 纯替换（与查表解耦，便于单测占位符语义）。
fn substitute(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// 当前 locale 的展示名（设置面板/诊断用）。
pub fn current_locale() -> String {
    rust_i18n::locale().to_string()
}

/// 当前 locale 是否等于 `locale`（**免分配**比较）。
///
/// 供「逐帧检测语言是否变了」的调用点使用（如菜单条每帧同步）：这类调用点
/// 不该为了比较而每帧分配一个 `String`。
pub fn locale_is(locale: &str) -> bool {
    &*rust_i18n::locale() == locale
}

#[cfg(test)]
#[path = "tests/i18n.rs"]
mod tests;
