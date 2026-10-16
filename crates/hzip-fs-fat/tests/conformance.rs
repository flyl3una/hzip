//! 契约一致性（TC-CONF 口径）：`FsVolume` 的硬红线逐条钉住。
//!
//! 计划的 §验收清单把 `TC-CONF-001..010` 挂在 testkit 的 `run_conformance` 上，
//! 但本期 testkit 还没有 vol 侧的 conformance 跑手（且本波次不允许改 testkit），
//! 故这十条**契约不变式**在本文件里直接写死——等跑手落地时转移即可。
//!
//! | 用例 | 契约 |
//! |---|---|
//! | [`ids_are_indices_and_sizes_match_the_tree`] | `EntryId` = 列表下标；`size_of` 与树一致 |
//! | [`reads_are_lazy_order_independent_and_repeatable`] | 任意偏移/乱序/重复可用 |
//! | [`reading_never_modifies_the_device`] | **读不能改档案**（含 `update_accessed_date(false)`） |
//! | [`limit_truncates_honestly`] | 超上限 ⇒ `truncated`，不悄悄截断 |
//! | [`cancel_is_reported_not_ignored`] | 取消 ⇒ `VolError::Cancelled` |
//! | [`ids_stay_stable_across_changes`] | 变更后旧 id 仍指向同一条目 |
//! | [`out_of_range_ids_are_refused`] | 越界/墓碑 id ⇒ 稳定键错误，不 panic |
//! | [`caps_match_the_documented_shape`] | 写侧能力细粒度如实 |
//! | [`audit_never_pretends_to_have_checked`] | 无 fsck ⇒ `skipped` + `vol.warn.no-journal` |
//! | [`listings_are_deterministic`] | 同一盘两次列表逐项相同（id 也相同） |

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use hzip_format::{Change, EntryName};
use hzip_vol::{EntryLimit, VolError};

/// ① `EntryId` 是列表下标；`size_of` 与树里的尺寸一致；文件内容长度也对得上。
#[test]
fn ids_are_indices_and_sizes_match_the_tree() {
    let m = mount_image(fat32(&[
        ("a.txt", b"aaa"),
        ("dir/b.bin", &[7u8; 5000]),
        ("dir/sub/c.txt", b"cc"),
    ]));
    let list = tree(m.fs.as_ref());
    for (index, e) in list.entries.iter().enumerate() {
        assert_eq!(e.id, index, "id 必须是列表下标：{}", e.name);
        assert_eq!(
            m.fs.size_of(e.id).unwrap(),
            e.size_raw,
            "size_of 应与树里的 size_raw 一致：{}",
            e.name
        );
        if !e.is_dir {
            assert_eq!(
                read_all(m.fs.as_ref(), e.id).len() as u64,
                e.size_raw,
                "可读字节数应等于目录项里的 size：{}",
                e.name
            );
        }
    }
}

/// ② 惰性读：乱序、重复、跨界、越界（EOF）都成立，且同一偏移两次结果相同。
#[test]
fn reads_are_lazy_order_independent_and_repeatable() {
    let data: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 256) as u8).collect();
    let m = mount_image(fat32(&[("blob.bin", &data)]));
    let id = id_of(m.fs.as_ref(), "blob.bin");

    let mut buf = [0u8; 128];
    let mut order = Vec::new();
    for offset in [4096u64, 0, 2048, 1, 4096, 4999, 5000] {
        let n = m.fs.read_at(id, offset, &mut buf).unwrap();
        let want = usize::try_from(5000u64.saturating_sub(offset).min(128)).unwrap();
        assert_eq!(n, want, "偏移 {offset} 的读数不符");
        assert_eq!(&buf[..n], &data[offset as usize..offset as usize + n]);
        order.push((offset, n));
    }
    assert_eq!(order[0], order[4], "同一偏移两次结果必须一致");

    // 越过文件尾 ⇒ 0 字节（不是错误）；越过头部的 u32 空间 ⇒ 也是 0。
    assert_eq!(m.fs.read_at(id, 5000, &mut buf).unwrap(), 0);
    assert_eq!(m.fs.read_at(id, 1 << 33, &mut buf).unwrap(), 0);
    // 零长度缓冲 ⇒ 0，且不碰设备。
    assert_eq!(m.fs.read_at(id, 0, &mut []).unwrap(), 0);
}

/// ③ **读不能改档案**：挂载 + 列树 + 读遍所有文件 + 自检之后，设备字节逐一相同。
///
/// 这条是 hzip 的红线，也是 FAT 最容易踩的坑：`fatfs` 的「访问日期更新」会把每次
/// 读变成一次目录项写。故挂载必须显式 `update_accessed_date(false)`。
#[test]
fn reading_never_modifies_the_device() {
    let image = fat32(&[
        ("a.txt", b"aaaa"),
        ("dir/b.bin", &[3u8; 9000]),
        ("中文/名字.txt", "内容".as_bytes()),
    ]);
    let before = image.clone();
    let m = mount_image(image);
    let list = tree(m.fs.as_ref());
    for e in &list.entries {
        if !e.is_dir {
            let _ = read_all(m.fs.as_ref(), e.id);
        }
    }
    let _ = m.fs.audit();
    m.fs.entries(&EntryLimit::default(), &cancel()).unwrap();

    assert_eq!(
        m.dev.to_bytes(),
        before,
        "只读访问不该改动设备的任何一个字节"
    );
}

/// ④ 上限如实：`max_entries` 到顶 ⇒ `truncated` 且条数正好到顶；`max_depth` 同理。
#[test]
fn limit_truncates_honestly() {
    let m = mount_image(fat32(&[
        ("a.txt", b"a"),
        ("b.txt", b"b"),
        ("c.txt", b"c"),
        ("d.txt", b"d"),
        ("dir/e.txt", b"e"),
    ]));
    let limited = tree_with(
        m.fs.as_ref(),
        &EntryLimit {
            max_entries: 2,
            ..EntryLimit::default()
        },
    );
    assert_eq!(limited.entries.len(), 2, "到顶就停");
    assert!(limited.truncated, "被截断必须如实标注");

    let shallow = tree_with(
        m.fs.as_ref(),
        &EntryLimit {
            max_entries: 1_000,
            max_depth: 0,
        },
    );
    assert!(shallow.truncated, "深度到顶也要如实标注");
    assert!(
        shallow.by_name("dir").is_some() && shallow.by_name("dir/e.txt").is_none(),
        "max_depth = 0 时只列根这一层：{:?}",
        names(&shallow)
    );

    let full = tree(m.fs.as_ref());
    assert!(
        !full.truncated && full.entries.len() == 6,
        "{:?}",
        names(&full)
    );
}

/// ⑤ 取消 ⇒ `Cancelled`（不是被静默忽略、也不是半棵树）。
#[test]
fn cancel_is_reported_not_ignored() {
    let m = mount_image(fat32(&[("a.txt", b"a")]));
    let token = cancel();
    token.cancel();
    match m.fs.entries(&EntryLimit::default(), &token) {
        Err(VolError::Cancelled) => {}
        other => panic!("取消除应报 Cancelled，得到 {other:?}"),
    }
}

/// ⑥ id 稳定：改名前拿到的 id，改名后仍指向同一个文件（`Reader` 就是这么用的）。
#[test]
fn ids_stay_stable_across_changes() {
    let mut m = mount_image(fat32(&[("old.txt", b"content"), ("gone.txt", b"x")]));
    let id = id_of(m.fs.as_ref(), "old.txt");
    let id_gone = id_of(m.fs.as_ref(), "gone.txt");

    m.fs.apply(
        &Change::Rename {
            from: EntryName::new("old.txt"),
            to: EntryName::new("new.txt"),
        },
        None,
        &cancel(),
    )
    .unwrap();
    m.fs.apply(&Change::Delete(id_gone), None, &cancel())
        .unwrap();

    assert_eq!(read_all(m.fs.as_ref(), id), b"content", "旧 id 必须仍可用");
    assert_eq!(m.fs.size_of(id).unwrap(), 7);
    assert_eq!(
        read_name(m.fs.as_ref(), "new.txt"),
        b"content",
        "改名后的名字也要能读"
    );
}

/// ⑦ 越界/已删的 id：报稳定键错误，**不 panic、不读错文件**。
#[test]
fn out_of_range_ids_are_refused() {
    let mut m = mount_image(fat32(&[("a.txt", b"a")]));
    let _ = tree(m.fs.as_ref());
    let mut buf = [0u8; 8];

    for id in [9_999usize, 12] {
        match m.fs.read_at(id, 0, &mut buf) {
            Err(VolError::Unsupported(key)) => assert_eq!(key, hzip_fs_fat::ERR_UNKNOWN_ENTRY),
            other => panic!("越界 id {id} 应报未知条目，得到 {other:?}"),
        }
        match m.fs.size_of(id) {
            Err(VolError::Unsupported(key)) => assert_eq!(key, hzip_fs_fat::ERR_UNKNOWN_ENTRY),
            other => panic!("越界 id {id} 应报未知条目，得到 {other:?}"),
        }
        match m.fs.apply(&Change::Delete(id), None, &cancel()) {
            Err(VolError::Unsupported(key)) => assert_eq!(key, hzip_fs_fat::ERR_UNKNOWN_ENTRY),
            other => panic!("越界 id {id} 应报未知条目，得到 {other:?}"),
        }
    }

    // 墓碑 id：删掉之后再用同一个 id ⇒ entry-gone（而不是去读别人的数据）。
    let id = id_of(m.fs.as_ref(), "a.txt");
    m.fs.apply(&Change::Delete(id), None, &cancel()).unwrap();
    match m.fs.read_at(id, 0, &mut buf) {
        Err(VolError::Unsupported(key)) => assert_eq!(key, hzip_fs_fat::ERR_ENTRY_GONE),
        other => panic!("已删条目应报 entry-gone，得到 {other:?}"),
    }
}

/// ⑧ 能力形状：写侧细粒度如实（FAT 无符号链接/权限位、名字上限来自 LFN）。
#[test]
fn caps_match_the_documented_shape() {
    let m = mount_image(fat32(&[]));
    let caps = m.fs.caps();
    assert!(caps.write && caps.delete && caps.rename && caps.mkdir);
    assert!(caps.times, "FAT 有 mtime（2 s 精度）");
    assert!(!caps.symlink, "FAT 无符号链接");
    assert!(!caps.mode, "FAT 无权限位");
    assert_eq!(caps.name_max, 255, "LFN 上限");
    assert_eq!(caps.read_only_reason, None);
    assert_eq!(m.fs.fs_id(), "fat");
}

/// ⑨ 自检**不撒谎**：FAT 没有 fsck ⇒ `skipped` 为真 + 如实给出 `vol.warn.no-journal`。
#[test]
fn audit_never_pretends_to_have_checked() {
    let m = mount_image(fat32(&[("a.txt", b"a")]));
    let audit = m.fs.audit().unwrap();
    assert!(audit.skipped, "没有 fsck 就不能说「检查过了」");
    assert!(!audit.is_clean(), "skipped 不算干净（is_clean 的语义）");
    assert!(
        audit.findings.iter().any(|f| f == "vol.warn.no-journal"),
        "应给出无日志风险键：{:?}",
        audit.findings
    );
}

/// ⑩ 列表确定性：同一份字节连列两次，名字、id、类型、大小逐项相同。
#[test]
fn listings_are_deterministic() {
    let m = mount_image(fat32(&[
        ("z.txt", b"z"),
        ("a/1.txt", b"1"),
        ("a/2.txt", b"22"),
        ("b.txt", b""),
    ]));
    let first = tree(m.fs.as_ref());
    let second = tree(m.fs.as_ref());
    let key = |t: &hzip_vol::EntryTree| {
        t.entries
            .iter()
            .map(|e| (e.id, e.name.as_str().to_string(), e.is_dir, e.size_raw))
            .collect::<Vec<_>>()
    };
    assert_eq!(key(&first), key(&second));
    assert_eq!(first.entries.len(), 5, "{:?}", names(&first));
}
