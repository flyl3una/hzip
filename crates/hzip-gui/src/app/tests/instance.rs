use super::*;

/// 沙箱队列：绝不碰真实数据目录（多个用例共用一个真实文件会互相消费）。
fn sandbox() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let queue = dir.path().join("hzip-forward-queue");
    (dir, queue)
}

// 脏行容忍：一条坏记录不得让整批意图陪葬（跨进程追加文件，脏行总会遇到）
#[test]
fn queue_parse_drops_only_the_bad_line() {
    let text = r#"["--open","C:/a.zip"]
not-json
["--auto-extract","C:/b.7z"]
"#;
    assert_eq!(
        parse_queue_lines(text),
        vec![
            vec!["--open".to_string(), "C:/a.zip".to_string()],
            vec!["--auto-extract".to_string(), "C:/b.7z".to_string()],
        ]
    );
    // 空行剔除
    assert_eq!(parse_queue_lines("\n\n"), Vec::<Vec<String>>::new());
}

// 队列往返：动词必须原样保留（只转发路径 = 丢动词，本模块的红线）
#[test]
fn forward_roundtrip_preserves_verb() {
    let (_dir, queue) = sandbox();
    forward_argv_at(&queue, &["--auto-extract".into(), "C:/t1.zip".into()]).unwrap();
    forward_argv_at(
        &queue,
        &[
            "--compress".into(),
            "C:/dir".into(),
            "--format".into(),
            "7z".into(),
        ],
    )
    .unwrap();

    let got = drain_queue_at(&queue);
    assert_eq!(
        got,
        vec![
            vec!["--auto-extract".to_string(), "C:/t1.zip".to_string()],
            vec![
                "--compress".to_string(),
                "C:/dir".to_string(),
                "--format".to_string(),
                "7z".to_string()
            ],
        ],
        "动词与顺序必须原样保留"
    );
    // 消费后队列为空但**文件仍在**（运行期 watch 依附该路径）
    assert!(queue.exists(), "消费不得删除队列文件（watch 依赖它存在）");
    assert_eq!(fs::read_to_string(&queue).unwrap(), "", "消费后应清空");
    assert!(drain_queue_at(&queue).is_empty(), "重复消费应为空（幂等）");
}

// 旧版裸路径队列必须被丢弃而非误解析
#[test]
fn legacy_bare_path_queue_is_discarded() {
    let (_dir, queue) = sandbox();
    let legacy = queue.with_file_name("hzip-open-queue");
    fs::write(&legacy, "C:/stale.zip\n").unwrap();
    discard_legacy_file(&legacy);
    assert!(!legacy.exists(), "旧版队列应被丢弃");
}
