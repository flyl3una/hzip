use super::*;

#[test]
fn body_semantics_map_to_types() {
    assert!(matches!(
        notice_kind("任务已完成"),
        NotificationType::Success
    ));
    assert!(matches!(notice_kind("解压失败"), NotificationType::Error));
    assert!(matches!(notice_kind("已取消"), NotificationType::Warning));
}
