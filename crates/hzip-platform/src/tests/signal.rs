use super::*;

/// 占位测试：T10 合入后补全（02 §5.3.9 二次信号直通等平台机制单测）。
#[test]
#[ignore = "T10 hzip-progress 合入后激活（任务卡 §2；阶段 B/C）"]
#[allow(clippy::todo)]
fn second_signal_passthrough_placeholder() {
    let _uninstall = install_cancel_handler(());
    todo!("T10 合入后：mock 令牌首信号触发 cancel、二次信号走恢复默认分支（TC-WIN-009）")
}
