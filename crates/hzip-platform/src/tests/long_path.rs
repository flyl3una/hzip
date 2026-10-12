use super::*;

#[test]
fn roundtrip_windows_style() {
    // 平台无关的纯字符串级注入/还原验证：
    // to_extended 在非 win 平台恒等，以下用例在 Windows 上跑全量分支。
    let p = Path::new(r"C:\very\long\dir\x.txt");
    let ext = to_extended(p);
    assert_eq!(from_extended(&ext), p);
    let p2 = Path::new(r"\\server\share\f\g.txt");
    let ext2 = to_extended(p2);
    assert_eq!(from_extended(&ext2), p2);
    // 已扩展路径幂等
    let ext3 = to_extended(&ext);
    assert_eq!(ext3, ext);
}

#[test]
#[cfg(all(windows, feature = "win-long-path"))]
fn unc_mapping_win() {
    let p = Path::new(r"\\server\share\f.txt");
    let ext = to_extended(p);
    let s = ext.to_string_lossy();
    assert!(s.starts_with(r"\\?\UNC\server\share\f.txt"), "got {s}");
}

#[test]
#[cfg(all(windows, not(feature = "win-long-path")))]
fn feature_off_identity_win() {
    // feature 关闭（默认）：to_extended 恒等（02 §5.3.1 off 路径），
    // UNC 与盘符绝对路径均不注入——由上层 API 明确报错。
    let p = Path::new(r"\\server\share\f.txt");
    assert_eq!(to_extended(p), p);
    let c = Path::new(r"C:\a\b.txt");
    assert_eq!(to_extended(c), c);
}

#[test]
#[cfg(all(windows, feature = "win-long-path"))]
fn display_no_prefix_win() {
    let p = Path::new(r"C:\a\b.txt");
    let ext = to_extended(p);
    let back = from_extended(&ext);
    let shown = back.to_string_lossy();
    assert!(!shown.contains(r"\\?\"));
}

#[test]
fn limit() {
    assert!(max_path_len() > 0);
    #[cfg(windows)]
    {
        if cfg!(feature = "win-long-path") {
            assert_eq!(max_path_len(), 32767);
        } else {
            assert_eq!(max_path_len(), 260);
        }
    }
}
