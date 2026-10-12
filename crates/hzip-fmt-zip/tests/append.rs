//! TC-CONF-005 追加：旧条目本地头+数据字节不变（diff）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;

use hzip_format::{CreateOptions, EntryMeta, FormatHandler, PasswordRef, Sink};

use common::tmp_dir;

fn find_sig(haystack: &[u8], sig: &[u8]) -> Option<usize> {
    haystack.windows(sig.len()).position(|w| w == sig)
}

#[test]
fn append_preserves_old_bytes() {
    let dir = tmp_dir("append");
    let p = dir.join("app.zip");
    {
        let mut w = common::handler()
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        for (name, data) in [
            ("old-a.txt", b"old-a-content-".to_vec()),
            ("sub/old-b.txt", vec![0xABu8; 3000]),
        ] {
            let meta = EntryMeta {
                name: name.into(),
                size_raw: Some(data.len() as u64),
                ..Default::default()
            };
            let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
            s.write_all(&data).unwrap();
            s.finish().unwrap();
        }
        w.finish().unwrap();
    }
    let before = std::fs::read(&p).unwrap();

    {
        let mut w = common::handler()
            .create_writer(
                Sink::File(p.clone()),
                &CreateOptions {
                    append: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let meta = EntryMeta {
            name: "new-c.txt".into(),
            size_raw: Some(9),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(b"new-c-new").unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
    }
    let after = std::fs::read(&p).unwrap();

    // 旧档案数据区（首个中央目录 PK\x01\x02 之前）追加后必须前缀保持
    let cd_pos = find_sig(&before, b"PK\x01\x02").unwrap_or(before.len());
    let data_prefix = &before[..cd_pos];
    assert!(
        after.starts_with(data_prefix),
        "追加后旧条目数据区应前缀不变"
    );

    let infos = common::entries(&p);
    assert_eq!(infos.len(), 3, "追加后条目数 +1");
    assert_eq!(common::read_entry_file(&p, 0), b"old-a-content-");
    assert_eq!(common::read_entry_file(&p, 1), vec![0xABu8; 3000]);
    assert_eq!(common::read_entry_file(&p, 2), b"new-c-new");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn append_missing_file_creates_new() {
    // append 目标不存在 → 退化新建（>> 语义，TC-CONF-005 表面契约）
    let dir = tmp_dir("append-new");
    let p = dir.join("nope.zip");
    let mut w = common::handler()
        .create_writer(
            Sink::File(p.clone()),
            &CreateOptions {
                append: true,
                ..Default::default()
            },
        )
        .unwrap();
    let meta = EntryMeta {
        name: "a.txt".into(),
        size_raw: Some(1),
        ..Default::default()
    };
    let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
    s.write_all(b"x").unwrap();
    s.finish().unwrap();
    w.finish().unwrap();
    let infos = common::entries(&p);
    assert_eq!(infos.len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}
