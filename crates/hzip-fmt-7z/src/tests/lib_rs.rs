use super::TempArea;

/// TC-CONF-009 同型：临时区随 drop 清理（两遍法 finish 成功/失败两条路径的
/// 临时文件物理清理由 writer 持有 TempArea 的 Drop 保证——此处断言 Drop 行为）。
#[test]
fn temp_area_cleans_on_drop() {
    let dir = {
        let area = TempArea::new("drop-test").expect("temp area 创建");
        // 写文件占位（模拟两遍法产物）
        let p = area.join("out.7z");
        std::fs::write(&p, b"bytes").expect("写占位文件");
        assert!(p.exists());
        let d = area.dir.clone();
        assert!(d.exists());
        d
    };
    assert!(!dir.exists(), "TempArea drop 后目录应被清理");
}
