use super::*;

fn json(v: &str) -> Handler {
    match serde_json::from_str::<serde_json::Value>(v).expect("测试 JSON") {
        serde_json::Value::Object(m) => m,
        _ => panic!("应为对象"),
    }
}

/// bundle 探测：从可执行文件路径回溯到 `.app` 根。
#[test]
fn bundle_detection() {
    assert_eq!(
        bundle_of(Path::new("/Applications/hzip.app/Contents/MacOS/hzip-gui")),
        Some(PathBuf::from("/Applications/hzip.app"))
    );
    // 裸二进制（无包）→ None（apply 据此给出「只能为 App 包注册」的说明）
    assert_eq!(bundle_of(Path::new("/Users/a/target/debug/hzip-gui")), None);
}

/// 合并：认领扩展名归我们，**别人的条目一条不丢**（这是本文件最容易出人命处）。
#[test]
fn merge_replaces_only_claimed_extensions() {
    let existing = vec![
        json(
            r#"{"LSHandlerContentTagClass":"public.filename-extension","LSHandlerContentTag":"zip","LSHandlerRoleAll":"com.bandizip"}"#,
        ),
        json(
            r#"{"LSHandlerContentTagClass":"public.filename-extension","LSHandlerContentTag":"pdf","LSHandlerRoleAll":"com.adobe.Reader"}"#,
        ),
        json(r#"{"LSHandlerContentType":"public.jpeg","LSHandlerRoleAll":"com.apple.Preview"}"#),
    ];
    let out = merge_handlers(&existing, &["zip".into(), "7z".into()], "com.hzip.mac");
    assert_eq!(out.len(), 4, "3 旧 - 1 被顶 + 2 新 = 4：{out:?}");
    // 我们认领的（且**排在前面**：LSHandlers 先命中者为准）
    assert_eq!(handler_tag(&out[0]), Some("zip"));
    assert_eq!(handler_role(&out[0]), Some("com.hzip.mac"));
    assert_eq!(handler_tag(&out[1]), Some("7z"));
    // 别人的原样保留
    assert!(
        out.iter()
            .any(|h| handler_role(h) == Some("com.adobe.Reader"))
    );
    assert!(
        out.iter()
            .any(|h| handler_role(h) == Some("com.apple.Preview"))
    );
    assert!(
        !out.iter().any(|h| handler_role(h) == Some("com.bandizip")),
        "被顶掉的旧条目不该留：{out:?}"
    );
}

/// 摘除：只摘指向我们的；`exts` 为空 = 凡指向我们的都摘。
#[test]
fn strip_only_touches_ours() {
    let existing = vec![
        json(
            r#"{"LSHandlerContentTagClass":"public.filename-extension","LSHandlerContentTag":"zip","LSHandlerRoleAll":"com.hzip.mac"}"#,
        ),
        json(
            r#"{"LSHandlerContentTagClass":"public.filename-extension","LSHandlerContentTag":"pdf","LSHandlerRoleAll":"com.hzip.mac"}"#,
        ),
        json(
            r#"{"LSHandlerContentTagClass":"public.filename-extension","LSHandlerContentTag":"rar","LSHandlerRoleAll":"com.rarlab"}"#,
        ),
    ];
    let out = strip_handlers(&existing, &["zip".into()], "com.hzip.mac");
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(
        out.iter().any(|h| handler_tag(h) == Some("pdf")),
        "未点名的我们条目保留"
    );
    assert!(out.iter().any(|h| handler_role(h) == Some("com.rarlab")));

    let all = strip_handlers(&existing, &[], "com.hzip.mac");
    assert_eq!(all.len(), 1, "空 exts = 摘掉全部我们的条目：{all:?}");
}

/// 回读判定：ours（指向我们）/ 非 ours（指向别人，且**改得动**）。
#[test]
fn state_reads_handlers() {
    let ours = vec![json(
        r#"{"LSHandlerContentTagClass":"public.filename-extension","LSHandlerContentTag":"zip","LSHandlerRoleAll":"com.hzip.mac"}"#,
    )];
    let st = state_of(&ours, "zip", "com.hzip.mac");
    assert!(st.is_effective() && !st.pinned);

    let theirs = vec![json(
        r#"{"LSHandlerContentTagClass":"public.filename-extension","LSHandlerContentTag":"zip","LSHandlerRoleAll":"com.bandizip"}"#,
    )];
    let st = state_of(&theirs, "zip", "com.hzip.mac");
    assert_eq!(st.effective.as_deref(), Some("com.bandizip"));
    assert!(!st.is_effective());
    assert!(
        !st.blocked_by_other(),
        "macOS 上改 LSHandlers 即可夺回：{st:?}"
    );
    assert_eq!(st.owner_name(), Some("com.bandizip"));

    // 未认领 → 无默认
    assert_eq!(state_of(&theirs, "tar", "com.hzip.mac").effective, None);
}

/// `defaults write` 的参数拼装：空表 → `delete`（不留空数组）。
#[test]
fn write_args_shape() {
    let h = vec![our_handler("zip", "com.hzip.mac")];
    let args = write_args(&h);
    assert_eq!(
        args[..4],
        [
            "write".to_string(),
            DOMAIN.to_string(),
            KEY_HANDLERS.to_string(),
            "-array".to_string()
        ]
    );
    assert_eq!(
        args[4],
        r#"{LSHandlerContentTag="zip";LSHandlerContentTagClass="public.filename-extension";LSHandlerRoleAll="com.hzip.mac";}"#
    );
    assert_eq!(
        write_args(&[]),
        vec![
            "delete".to_string(),
            DOMAIN.to_string(),
            KEY_HANDLERS.to_string()
        ]
    );
}
