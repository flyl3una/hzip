/// 密码弹窗只有一个输入框（用户实测反馈 2026-09-23）：目标/尝试不得再渲染成
/// 只读伪输入行——弹窗内不得出现只读伪输入共用件，输入件恰好 1 个。
///
/// 断言串拆分拼接，避免 `include_str!` 把本测试自身算作命中。
#[test]
fn only_one_input_row() {
    // T-STD 外移：本测试随 `password.rs` 的测试模块迁到 `tests/`，`include_str!`
    // 以**本文件所在目录**为基准，故回指上一级的业务文件。
    let source = include_str!("../password.rs");
    let read_only = concat!("read_only", "_field");
    let input_new = concat!("Input::", "new");
    assert!(
        !source.contains(read_only),
        "密码弹窗不得有只读伪输入行（目标/尝试 → 输入框观感缺陷）"
    );
    assert_eq!(
        source.matches(input_new).count(),
        1,
        "密码弹窗应恰好一个输入件"
    );
}
