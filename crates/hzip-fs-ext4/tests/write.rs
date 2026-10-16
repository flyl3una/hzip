//! ext4 写侧契约测试（T-IMG5 §2.1 的 `Change` → `apply_*` 映射表 + §2.2 的可写挂载策略）。
//!
//! 最硬的断言是**写后重挂复核**：只有重挂后还成立，才算真写进了卷里（覆盖写的
//! 内存缓存、忘了 flush、只改了内存里的快照——都能被这一条抓住）。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use hzip_format::{Change, EntryId, EntryName};
use hzip_format_testkit::vol::{MemDevice, ext4_image, ext4_image_dirty};
use hzip_fs_ext4::Ext4Driver;
use hzip_progress::CancellationToken;
use hzip_vol::error::{ReadOnlyReason, VolError};
use hzip_vol::{EntryLimit, EntryTree, FsDriver, FsOptions, FsVolume};

fn cancel() -> CancellationToken {
    CancellationToken::new()
}

fn limit() -> EntryLimit {
    EntryLimit {
        max_entries: 10_000,
        max_depth: 32,
    }
}

fn entries(fs: &dyn FsVolume) -> EntryTree {
    fs.entries(&limit(), &cancel()).unwrap()
}

/// 可写挂载（干净卷）。
fn mount_rw(dev: &Arc<MemDevice>) -> Box<dyn FsVolume> {
    Ext4Driver::new()
        .mount(
            dev.clone(),
            &FsOptions {
                writable: true,
                ..Default::default()
            },
        )
        .unwrap()
}

/// 只读重挂（复核用：写侧结论必须在没有写会话的句柄上也成立）。
fn remount_ro(dev: &Arc<MemDevice>) -> Box<dyn FsVolume> {
    Ext4Driver::new()
        .mount(dev.clone(), &FsOptions::default())
        .unwrap()
}

fn id_of(fs: &dyn FsVolume, name: &str) -> EntryId {
    entries(fs)
        .by_name(name)
        .unwrap_or_else(|| panic!("条目 {name} 不在树里"))
        .id
}

/// 顶层条目名（**含 mkfs 造的 `lost+found`**，夹具卷里它一直在）——排好序便于
/// 断言「没多也没少」。
fn top_names(fs: &dyn FsVolume) -> Vec<String> {
    let mut names: Vec<String> = entries(fs)
        .entries
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    names.sort();
    names
}

/// 全量读出某条目。
fn read_all(fs: &dyn FsVolume, id: EntryId) -> Vec<u8> {
    let size = fs.size_of(id).unwrap();
    let mut out = vec![0u8; usize::try_from(size).unwrap()];
    let n = fs.read_at(id, 0, &mut out).unwrap();
    out.truncate(n);
    out
}

/// 条目的 mtime（unix 秒）。
fn mtime_of(fs: &dyn FsVolume, name: &str) -> i64 {
    entries(fs)
        .by_name(name)
        .unwrap_or_else(|| panic!("条目 {name} 不在树里"))
        .mtime
        .expect("ext4 条目必有 mtime")
        .unix_timestamp()
}

/// 本地临时文件（编辑保存的「本地新内容」）；`mtime` 给定则钉死修改时间。
fn local(dir: &Path, name: &str, data: &[u8], mtime: Option<SystemTime>) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    if let Some(t) = mtime {
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(t))
            .unwrap();
    }
    p
}

#[test]
fn add_replace_delete_rename_mkdir_round_trip_and_survive_remount() {
    let dir = tempfile::tempdir().unwrap();
    // 夹具造的卷（干净、可写）
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/a.txt", b"old")])));
    {
        let mut fs = mount_rw(&dev);
        assert!(fs.caps().write, "干净卷请求可写 ⇒ 可写");
        assert_eq!(fs.caps().read_only_reason, None);
        let caps = fs.caps();
        assert!(
            caps.delete && caps.rename && caps.mkdir && caps.times && caps.mode,
            "写侧能力全开：{caps:?}"
        );

        // ① Append：父目录 dir 不存在 ⇒ 逐级建
        let new_txt = local(dir.path(), "new.txt", b"NEW", None);
        fs.apply(
            &Change::Append {
                src: new_txt.clone(),
                name: "dir/new.txt".into(),
            },
            None,
            &cancel(),
        )
        .unwrap();
        assert!(
            entries(&*fs).by_name("dir").is_some(),
            "Append 自动建了父目录"
        );

        // ② Replace：覆盖既有条目
        let a_new = local(dir.path(), "a.txt", b"new-content", None);
        fs.apply(
            &Change::Replace {
                entry: id_of(&*fs, "a.txt"),
                src: a_new.clone(),
            },
            Some(&a_new),
            &cancel(),
        )
        .unwrap();

        // ③ MakeDir
        fs.apply(
            &Change::MakeDir {
                name: "empty-dir".into(),
            },
            None,
            &cancel(),
        )
        .unwrap();

        // ④ Rename（文件）
        fs.apply(
            &Change::Rename {
                from: "a.txt".into(),
                to: "b.txt".into(),
            },
            None,
            &cancel(),
        )
        .unwrap();

        // ⑤ Delete（刚追加的那个）
        fs.apply(&Change::Delete(id_of(&*fs, "dir/new.txt")), None, &cancel())
            .unwrap();

        assert!(fs.audit().unwrap().is_clean(), "写后自检必须干净");
        fs.flush().unwrap();
    }

    // 重挂后才算真写完
    let fs = remount_ro(&dev);
    let tree = entries(&*fs);
    assert_eq!(
        read_all(&*fs, tree.by_name("b.txt").unwrap().id),
        b"new-content".to_vec()
    );
    assert!(
        tree.by_name("dir").is_some(),
        "空了的目录仍在（Append 时自动建的父目录）"
    );
    assert!(tree.by_name("empty-dir").is_some());
    assert!(tree.by_name("a.txt").is_none() && tree.by_name("dir/new.txt").is_none());
    assert!(fs.audit().unwrap().is_clean(), "重挂后自检也干净");
}

#[test]
fn replace_preserves_local_mtime() {
    const STAMP: i64 = 1_234_567_890;
    let dir = tempfile::tempdir().unwrap();
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/a.txt", b"old")])));
    let p = local(
        dir.path(),
        "a.txt",
        b"new",
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(STAMP as u64)),
    );
    {
        let mut fs = mount_rw(&dev);
        assert_ne!(mtime_of(&*fs, "a.txt"), STAMP, "夹具的时间戳不是这个");
        fs.apply(
            &Change::Replace {
                entry: id_of(&*fs, "a.txt"),
                src: p.clone(),
            },
            None,
            &cancel(),
        )
        .unwrap();
        assert_eq!(mtime_of(&*fs, "a.txt"), STAMP, "Replace 后取本地文件 mtime");
        fs.flush().unwrap();
    }
    let fs = remount_ro(&dev);
    assert_eq!(mtime_of(&*fs, "a.txt"), STAMP, "mtime 必须真的落在盘上");
}

#[test]
fn rename_directory_moves_the_whole_subtree() {
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[
        ("/dir/a.txt", b"A"),
        ("/dir/sub/b.txt", b"B"),
    ])));
    {
        let mut fs = mount_rw(&dev);
        // 裸名（无 `/`）⇒ 以 `from` 的父目录为准
        fs.apply(
            &Change::Rename {
                from: "dir".into(),
                to: "dir2".into(),
            },
            None,
            &cancel(),
        )
        .unwrap();
        fs.flush().unwrap();
    }
    let fs = remount_ro(&dev);
    let tree = entries(&*fs);
    assert_eq!(
        read_all(&*fs, tree.by_name("dir2/a.txt").unwrap().id),
        b"A".to_vec()
    );
    assert_eq!(
        read_all(&*fs, tree.by_name("dir2/sub/b.txt").unwrap().id),
        b"B".to_vec(),
        "整棵子树跟着走"
    );
    assert!(tree.by_name("dir").is_none() && tree.by_name("dir/a.txt").is_none());
    assert!(fs.audit().unwrap().is_clean());
}

#[test]
fn rename_with_a_full_path_target_is_used_as_is() {
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/dir/a.txt", b"A")])));
    {
        let mut fs = mount_rw(&dev);
        // GUI 的改名传的是完整路径（`join_entry_path(dir, name)`）⇒ 原样使用
        fs.apply(
            &Change::MakeDir {
                name: "dir/sub".into(),
            },
            None,
            &cancel(),
        )
        .unwrap();
        fs.apply(
            &Change::Rename {
                from: "dir/a.txt".into(),
                to: "dir/sub/moved.txt".into(),
            },
            None,
            &cancel(),
        )
        .unwrap();
        fs.flush().unwrap();
    }
    let fs = remount_ro(&dev);
    let tree = entries(&*fs);
    assert_eq!(
        read_all(&*fs, tree.by_name("dir/sub/moved.txt").unwrap().id),
        b"A".to_vec()
    );
    assert!(tree.by_name("dir/a.txt").is_none());
}

#[test]
fn append_onto_an_existing_name_overwrites_its_content() {
    let dir = tempfile::tempdir().unwrap();
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/a.txt", b"old")])));
    {
        let mut fs = mount_rw(&dev);
        let p = local(dir.path(), "a.txt", b"NEW-BYTES", None);
        fs.apply(
            &Change::Append {
                src: p.clone(),
                name: "a.txt".into(),
            },
            None,
            &cancel(),
        )
        .unwrap();
        fs.flush().unwrap();
    }
    let fs = remount_ro(&dev);
    assert_eq!(read_all(&*fs, id_of(&*fs, "a.txt")), b"NEW-BYTES".to_vec());
    assert_eq!(
        top_names(&*fs),
        vec!["a.txt".to_string(), "lost+found".to_string()],
        "不是多了一条同名条目"
    );
}

#[test]
fn append_with_a_trailing_slash_only_makes_a_directory() {
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/a.txt", b"a")])));
    let mut fs = mount_rw(&dev);
    fs.apply(
        &Change::Append {
            src: PathBuf::from("does-not-exist"),
            name: EntryName::new("only-dir/"),
        },
        None,
        &cancel(),
    )
    .unwrap();
    let tree = entries(&*fs);
    assert!(tree.by_name("only-dir").unwrap().is_dir);
    assert_eq!(
        top_names(&*fs),
        vec![
            "a.txt".to_string(),
            "lost+found".to_string(),
            "only-dir".to_string()
        ],
        "只多了那一个目录"
    );
    fs.flush().unwrap();
}

#[test]
fn replace_of_a_missing_entry_creates_it() {
    let dir = tempfile::tempdir().unwrap();
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/a.txt", b"old")])));
    let p = local(dir.path(), "a.txt", b"resurrected", None);
    {
        let mut fs = mount_rw(&dev);
        // 同一条路径先删后替换：删除后这个 id 指向的路径已不在卷里。
        // ⚠ 这里**不能**重新列树——`entries()` 会重建 id 映射（id 属于最近一次列树的
        // 快照），而本用例要考的正是「快照里的路径已经不在了」。
        let id = id_of(&*fs, "a.txt");
        fs.apply(&Change::Delete(id), None, &cancel()).unwrap();
        fs.apply(
            &Change::Replace {
                entry: id,
                src: p.clone(),
            },
            None,
            &cancel(),
        )
        .unwrap();
        // 重列（新快照）复核：目标被重新建出来
        assert_eq!(
            read_all(&*fs, id_of(&*fs, "a.txt")),
            b"resurrected".to_vec()
        );
        fs.flush().unwrap();
    }
    let fs = remount_ro(&dev);
    assert_eq!(
        read_all(&*fs, id_of(&*fs, "a.txt")),
        b"resurrected".to_vec()
    );
}

#[test]
fn deleting_a_non_empty_directory_is_refused_with_a_readable_reason() {
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/dir/a.txt", b"A")])));
    {
        let mut fs = mount_rw(&dev);
        let err = fs
            .apply(&Change::Delete(id_of(&*fs, "dir")), None, &cancel())
            .unwrap_err();
        match err {
            VolError::Unsupported(key) => assert_eq!(key, "vol.err.dir-not-empty"),
            other => panic!("实得 {other:?}"),
        }
        // 不递归删：一个条目都没少
        assert!(entries(&*fs).by_name("dir/a.txt").is_some());
        fs.flush().unwrap();
    }

    // 空目录删得掉
    let mut fs = mount_rw(&dev);
    fs.apply(
        &Change::MakeDir {
            name: "gone".into(),
        },
        None,
        &cancel(),
    )
    .unwrap();
    fs.apply(&Change::Delete(id_of(&*fs, "gone")), None, &cancel())
        .unwrap();
    assert!(entries(&*fs).by_name("gone").is_none());
    assert!(fs.audit().unwrap().is_clean());
    fs.flush().unwrap();
}

#[test]
fn dirty_volume_is_refused_without_replay_and_writes_with_replay() {
    let dirty = ext4_image_dirty(&[("/a.txt", b"x")]);

    // ① 默认（`replay_journal = false`）⇒ 只读 + 理由，读仍然可用
    let dev = Arc::new(MemDevice::from_bytes(dirty.clone()));
    let mut fs = Ext4Driver::new()
        .mount(
            dev.clone(),
            &FsOptions {
                writable: true,
                replay_journal: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!fs.caps().write, "脏卷默认拒写");
    assert_eq!(
        fs.caps().read_only_reason,
        Some(ReadOnlyReason::JournalPending)
    );
    assert_eq!(read_all(&*fs, id_of(&*fs, "a.txt")), b"x".to_vec());
    let err = fs
        .apply(
            &Change::MakeDir {
                name: "nope".into(),
            },
            None,
            &cancel(),
        )
        .unwrap_err();
    assert!(
        matches!(err, VolError::ReadOnly(ReadOnlyReason::JournalPending)),
        "拒写必须带同一个理由：{err:?}"
    );
    drop(fs);

    // ② 显式允许重放 ⇒ 可写，且写出来的东西重挂可见
    let dev2 = Arc::new(MemDevice::from_bytes(dirty));
    {
        let mut fs2 = Ext4Driver::new()
            .mount(
                dev2.clone(),
                &FsOptions {
                    writable: true,
                    replay_journal: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(fs2.caps().write, "显式允许重放后可写");
        fs2.apply(
            &Change::MakeDir {
                name: "after-replay".into(),
            },
            None,
            &cancel(),
        )
        .unwrap();
        fs2.flush().unwrap();
    }
    let fs3 = remount_ro(&dev2);
    assert!(entries(&*fs3).by_name("after-replay").is_some());
}

#[test]
fn a_payload_that_does_not_fit_fails_with_a_vol_error_not_a_panic() {
    // 8 MiB 的卷里塞 32 MiB：**上游**对「没有这么大的连续空闲段」报的是
    // `Corrupt("no group has a contiguous free run of this size")`，而不是 ENOSPC
    // ——故这里断言的口径是「不 panic、给出可读的 VolError，且既有数据不受损」。
    // （`NoSpaceLeftOnDevice → vol.err.no-space` 的映射仍在 `error.rs` 里：上游真有
    // 报 ENOSPC 的路径（位图/xattr 用尽），只是大文件这条不走它；细节落日志。）
    let dir = tempfile::tempdir().unwrap();
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/a.txt", b"a")])));
    let big = local(dir.path(), "big.bin", &vec![0xABu8; 32 << 20], None);
    let mut fs = mount_rw(&dev);
    let err = fs
        .apply(
            &Change::Append {
                src: big,
                name: "big.bin".into(),
            },
            None,
            &cancel(),
        )
        .unwrap_err();
    assert!(
        matches!(err, VolError::Image(_) | VolError::Quota(_)),
        "实得 {err:?}"
    );
    // 既有条目不受损（失败的变更不该带走别人的数据）
    assert_eq!(read_all(&*fs, id_of(&*fs, "a.txt")), b"a".to_vec());
}

#[test]
fn cancel_stops_apply_before_touching_the_volume() {
    let dir = tempfile::tempdir().unwrap();
    let dev = Arc::new(MemDevice::from_bytes(ext4_image(&[("/a.txt", b"old")])));
    let p = local(dir.path(), "x", b"NEW", None);
    {
        let mut fs = mount_rw(&dev);
        let token = CancellationToken::new();
        token.cancel();
        let err = fs
            .apply(
                &Change::MakeDir {
                    name: "nope".into(),
                },
                None,
                &token,
            )
            .unwrap_err();
        assert!(matches!(err, VolError::Cancelled), "实得 {err:?}");
        let err = fs
            .apply(
                &Change::Append {
                    src: p.clone(),
                    name: "x".into(),
                },
                None,
                &token,
            )
            .unwrap_err();
        assert!(matches!(err, VolError::Cancelled), "实得 {err:?}");
        // 取消的变更一条都没落地
        let tree = entries(&*fs);
        assert!(tree.by_name("nope").is_none() && tree.by_name("x").is_none());
        fs.flush().unwrap();
    }
    let fs = remount_ro(&dev);
    assert_eq!(
        top_names(&*fs),
        vec!["a.txt".to_string(), "lost+found".to_string()],
        "卷里仍然只有原来那些东西"
    );
    assert_eq!(read_all(&*fs, id_of(&*fs, "a.txt")), b"old".to_vec());
}
