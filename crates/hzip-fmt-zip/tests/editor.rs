//! TC-CONF-009 编辑原子性 + rewrite 流式（02 §7.3/§9.9）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;

use hzip_format::{
    Change, ChangeSet, CreateOptions, EntryMeta, FormatHandler, PasswordRef, Sink, WriteCtx,
};

use common::tmp_dir;

fn write_base() -> std::path::PathBuf {
    let dir = tmp_dir("editor");
    let p = dir.join("base.zip");
    let mut w = common::handler()
        .create_writer(Sink::File(p.clone()), &Default::default())
        .unwrap();
    for (name, data) in [
        ("a.txt", b"alpha-content".to_vec()),
        ("b.bin", vec![0x11u8; 500]),
        ("c.txt", b"charlie".to_vec()),
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
    p
}

/// WriteCtx 持有组件。
struct Ctx {
    cancel: hzip_progress::CancellationToken,
    bus: hzip_format::ProgressBus,
    opts: CreateOptions,
}
impl Ctx {
    fn new() -> Self {
        Self {
            cancel: Default::default(),
            bus: Default::default(),
            opts: CreateOptions::default(),
        }
    }
    fn wctx(&self) -> WriteCtx<'_> {
        WriteCtx {
            cancel: &self.cancel,
            progress: &self.bus,
            opts: &self.opts,
        }
    }
}

#[test]
fn edit_atomicity_original_untouched() {
    let p = write_base();
    let before = std::fs::read(&p).unwrap();
    let editor = common::handler()
        .open_editor(hzip_format::Source::File(p.clone()), &Default::default())
        .unwrap();
    let out = p.parent().unwrap().join("out.zip");
    let ctx = Ctx::new();
    let rep = editor.rewrite(ChangeSet::default(), Sink::File(out.clone()), &ctx.wctx());
    assert!(rep.is_ok(), "空变更重写应成功");
    let after = std::fs::read(&p).unwrap();
    assert_eq!(before, after, "原档案字节不变（TC-CONF-009）");
    let infos = common::entries(&out);
    assert_eq!(infos.len(), 3);
    std::fs::remove_dir_all(p.parent().unwrap()).ok();
}

#[test]
fn edit_replace_delete_append() {
    let p = write_base();
    let editor = common::handler()
        .open_editor(hzip_format::Source::File(p.clone()), &Default::default())
        .unwrap();
    let dir = p.parent().unwrap();
    let src = dir.join("new-a.txt");
    std::fs::write(&src, b"NEW-ALPHA").unwrap();
    let app = dir.join("append-src.txt");
    std::fs::write(&app, b"delta-content").unwrap();
    let cs = ChangeSet {
        changes: vec![
            Change::Replace {
                entry: 0,
                src: src.clone(),
            },
            Change::Delete(1),
            Change::Append {
                src: app.clone(),
                name: "d.txt".into(),
            },
        ],
    };
    let out = dir.join("edited.zip");
    let ctx = Ctx::new();
    let rep = editor
        .rewrite(cs, Sink::File(out.clone()), &ctx.wctx())
        .unwrap();
    assert_eq!(rep.entries_written, 3, "改写后条目：a(替换)+c+append");

    let infos = common::entries(&out);
    assert_eq!(infos.len(), 3);
    assert_eq!(common::read_entry_file(&out, 0), b"NEW-ALPHA");
    assert_eq!(infos[1].name.as_str(), "c.txt");
    assert_eq!(common::read_entry_file(&out, 1), b"charlie");
    assert_eq!(infos[2].name.as_str(), "d.txt");
    assert_eq!(common::read_entry_file(&out, 2), b"delta-content");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn edit_sink_split_supported() {
    let p = write_base();
    let editor = common::handler()
        .open_editor(hzip_format::Source::File(p.clone()), &Default::default())
        .unwrap();
    let dir = p.parent().unwrap();
    let prefix = dir.join("split-out.zip");
    let ctx = Ctx::new();
    let rep = editor.rewrite(
        ChangeSet::default(),
        Sink::Split {
            prefix: prefix.clone(),
            bytes_per_volume: 2000,
        },
        &ctx.wctx(),
    );
    assert!(rep.is_ok());
    std::fs::remove_dir_all(dir).ok();
}
