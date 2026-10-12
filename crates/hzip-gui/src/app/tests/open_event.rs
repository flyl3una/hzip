use super::*;

/// macOS 打开事件给的是 `file://` **URL**（百分号转义），不是路径。
/// 直接当路径用会得到字面量 `%20`——用户看到「窗口起来但压缩包没打开」
/// 的第二种成因。
#[test]
fn decodes_file_url_with_escapes() {
    let urls = vec![
        "file:///Users/me/My%20Archive.zip".to_string(),
        "file:///Users/me/%E5%8E%8B%E7%BC%A9.zip".to_string(),
    ];
    assert_eq!(
        paths_from_urls(&urls),
        vec!["/Users/me/My Archive.zip", "/Users/me/压缩.zip"]
    );
}

/// `file://localhost/…` 是同一台机器上文件的合法写法；其他主机名是共享卷，
/// 不是本地文件，必须丢弃（打开它只会得到一句系统错误）。
#[test]
fn accepts_localhost_and_rejects_remote_hosts() {
    let urls = vec![
        "file://localhost/Users/me/a.zip".to_string(),
        "file://server/share/a.zip".to_string(),
    ];
    assert_eq!(paths_from_urls(&urls), vec!["/Users/me/a.zip"]);
}

/// 非 file:// 或畸形条目丢弃而**不**中断整批：事件是外部输入，一个坏条目
/// 不该让用户选中的其余档案也打不开。
#[test]
fn drops_non_file_urls_and_keeps_order() {
    let urls = vec![
        "https://example.com/a.zip".to_string(),
        "file:///first.zip".to_string(),
        "not a url".to_string(),
        "file:///second.zip".to_string(),
    ];
    assert_eq!(paths_from_urls(&urls), vec!["/first.zip", "/second.zip"]);
}
