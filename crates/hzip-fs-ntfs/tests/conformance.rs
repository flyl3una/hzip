//! `FsVolume` 契约的逐条钉子（计划里的 `TC-CONF-001..010`）。
//!
//! # 为什么不用 testkit 的 `run_conformance`
//!
//! `hzip_format_testkit::run_conformance` 是给 **`FormatHandler`**（档案格式）用的套件，
//! 喂的是 `Source`/`Sink`；卷驱动不是 `FormatHandler`。这里按同一命名口径（`TC-CONF-*`）
//! 把 `FsVolume` 的契约逐条钉住，用的仍是 testkit 的 NTFS 小卷夹具。
//!
//! 覆盖：能力位自洽、嗅探口径、名字规范化、id 连续性、`read_at` 的惰性语义、
//! `size_of` 一致、条目上限/取消、只读卷的 flush 不写盘、`audit()` 如实。

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use hzip_format::Change;
use hzip_vol::{FsCaps, FsDriver as _, FsKind, ReadOnlyReason, VolError};

/// 夹具：一份够用的树（目录、常驻文件、非驻留文件、中文名各一）。
fn fixture() -> Vec<u8> {
    let big: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
    ntfs_image(&[
        ("plain.txt", b"plain"),
        ("dir/nested.txt", b"nested"),
        ("中文/名字.txt", b"unicode"),
        ("big.bin", &big),
    ])
}

/// TC-CONF-001：可写卷的能力位自洽（`mode` 为假、`name_max` = 255、无只读理由）。
#[test]
fn tc_conf_001_writable_caps_are_coherent() {
    let fs = mount(device(fixture()), &writable()).expect("挂载");
    let caps = fs.caps();
    assert_eq!(fs.fs_id(), "ntfs");
    assert_ne!(caps, FsCaps::read_only(ReadOnlyReason::BySetting));
    assert!(caps.write && caps.delete && caps.rename && caps.mkdir && caps.symlink && caps.times);
    assert!(!caps.mode, "NTFS 的权限模型是 ACL，不是 POSIX mode");
    assert_eq!(caps.name_max, 255, "NTFS 名字上限（UTF-16 码元）");
    assert_eq!(caps.read_only_reason, None);
}

/// TC-CONF-002：只读卷的能力位自洽（三个理由各自的键）。
#[test]
fn tc_conf_002_read_only_caps_carry_a_reason() {
    let cases: Vec<(Vec<u8>, hzip_vol::FsOptions, ReadOnlyReason)> = vec![
        (
            ntfs_image(&[("a.txt", b"a")]),
            read_only(),
            ReadOnlyReason::BySetting,
        ),
        (
            ntfs_image_dirty(&[("a.txt", b"a")]),
            writable(),
            ReadOnlyReason::Dirty,
        ),
        (
            ntfs_image_hibernated(&[("a.txt", b"a")]),
            writable(),
            ReadOnlyReason::Hibernated,
        ),
        (
            ntfs_image_pending_log(&[("a.txt", b"a")]),
            writable(),
            ReadOnlyReason::JournalPending,
        ),
    ];
    for (image, opts, want) in cases {
        let fs = mount(device(image), &opts).expect("挂载");
        let caps = fs.caps();
        assert_eq!(caps.read_only_reason, Some(want), "理由键");
        assert!(!caps.write && !caps.delete && !caps.rename && !caps.mkdir);
        assert!(!caps.mode && caps.name_max == 255);
        assert!(
            fs.entries(&EntryLimit::default(), &cancel()).is_ok(),
            "只读也能列"
        );
    }
}

/// TC-CONF-003：嗅探与头部**两份独立证据**都要对才接（`FsDriver::supports`）。
#[test]
fn tc_conf_003_supports_requires_both_signals() {
    let driver = NtfsDriver::new();
    assert_eq!(driver.id(), "ntfs");
    let boot = fixture();
    let head = &boot[..4096];

    assert!(driver.supports(FsKind::Ntfs, head), "真引导扇区");
    assert!(!driver.supports(FsKind::Ext4, head), "嗅探说不是 NTFS");
    assert!(!driver.supports(FsKind::Fat, head), "嗅探说不是 NTFS");
    assert!(
        !driver.supports(FsKind::Ntfs, &head[..511]),
        "头部不足 512 字节"
    );
    let mut oem = head.to_vec();
    oem[3..11].copy_from_slice(b"XXXX    ");
    assert!(!driver.supports(FsKind::Ntfs, &oem), "OEM 串不符");
    let mut sig = head.to_vec();
    sig[0x1FE] = 0x00;
    assert!(!driver.supports(FsKind::Ntfs, &sig), "缺 0x55AA");
}

/// TC-CONF-004：条目名规范化（相对、无前导 `./`、无空段、目录不带尾 `/`）。
#[test]
fn tc_conf_004_entry_names_are_normalized() {
    let fs = mount(device(fixture()), &writable()).expect("挂载");
    let tree = entries(&fs);
    assert!(!tree.entries.is_empty());
    for entry in &tree.entries {
        let name = entry.name.as_str();
        assert!(!name.is_empty(), "空名不合法");
        assert!(!name.starts_with('/'), "不许有前导 /：{name}");
        assert!(!name.starts_with("./"), "不许有前导 ./：{name}");
        assert!(!name.contains("//"), "不许有空段：{name}");
        assert!(
            !name.ends_with('/'),
            "目录性由 is_dir 表达，不靠尾 /：{name}"
        );
        assert!(!name.contains('\\'), "分隔符只有 /：{name}");
    }
    assert!(tree.by_name("dir").unwrap().is_dir);
    assert!(!tree.by_name("dir/nested.txt").unwrap().is_dir);
}

/// TC-CONF-005：`EntryId` 0 起连续，且与名字一一对应。
#[test]
fn tc_conf_005_entry_ids_are_contiguous() {
    let fs = mount(device(fixture()), &writable()).expect("挂载");
    let tree = entries(&fs);
    let mut seen = std::collections::HashSet::new();
    for (index, entry) in tree.entries.iter().enumerate() {
        assert_eq!(entry.id, index, "id 必须等于下标");
        assert!(seen.insert(entry.name.as_str().to_string()), "名字不许重复");
        match fs.size_of(entry.id) {
            Ok(size) => assert_eq!(size, entry.size_raw, "size_of 必须与列表一致"),
            Err(e) => panic!("size_of({}) 失败: {e}", entry.id),
        }
    }
    // 重列一次：同一棵树 ⇒ 同一批 id。
    let again = entries(&fs);
    assert_eq!(names(&tree), names(&again), "两次列表必须一致（确定性）");
}

/// TC-CONF-006：`read_at` 的惰性语义（乱序、重复、重叠、越界、空缓冲）。
#[test]
fn tc_conf_006_read_at_is_lazy_and_repeatable() {
    let fs = mount(device(fixture()), &writable()).expect("挂载");
    let id = id_of(&fs, "big.bin");
    let size = fs.size_of(id).unwrap();
    let whole = read_all(&fs, id, size);
    assert_eq!(whole.len() as u64, size);

    // 乱序 + 重叠 + 重复：每个窗口都要与整读一致。
    let windows: [(u64, usize); 8] = [
        (0, 1),
        (4095, 4097),
        (65536 - 3, 10),
        (12345, 4321),
        (0, 100_000),
        (size - 1, 1),
        (12345, 4321),
        (7, 9),
    ];
    for (offset, len) in windows {
        let mut buf = vec![0u8; len];
        let n = fs.read_at(id, offset, &mut buf).unwrap();
        let want = usize::try_from(size.saturating_sub(offset))
            .unwrap()
            .min(len);
        assert_eq!(n, want, "窗口 {offset}+{len} 的读出长度");
        assert_eq!(
            &buf[..n],
            &whole[offset as usize..offset as usize + n],
            "窗口内容"
        );
    }
    // 越界与空缓冲。
    assert_eq!(fs.read_at(id, size, &mut [0u8; 16]).unwrap(), 0);
    assert_eq!(fs.read_at(id, size + 4096, &mut [0u8; 16]).unwrap(), 0);
    assert_eq!(fs.read_at(id, 0, &mut []).unwrap(), 0);
}

/// TC-CONF-007：`size_of` 与真实读出长度一致（含目录 = 0、空文件 = 0）。
#[test]
fn tc_conf_007_size_of_matches_content() {
    let fs = mount(
        device(ntfs_image(&[("empty.bin", b""), ("one.bin", b"x")])),
        &writable(),
    )
    .expect("挂载");
    assert_eq!(fs.size_of(id_of(&fs, "empty.bin")).unwrap(), 0);
    assert_eq!(fs.size_of(id_of(&fs, "one.bin")).unwrap(), 1);
    assert_eq!(read_all(&fs, id_of(&fs, "one.bin"), 1), b"x");
    assert_eq!(
        fs.size_of(id_of(&fs, "empty.bin")).unwrap(),
        0,
        "重复调用一致"
    );
}

/// TC-CONF-008：条目上限与取消。
#[test]
fn tc_conf_008_limits_and_cancellation() {
    let fs = mount(device(fixture()), &writable()).expect("挂载");
    let full = entries(&fs);

    let cut = entries_with(
        &fs,
        &EntryLimit {
            max_entries: 2,
            ..EntryLimit::default()
        },
    );
    assert_eq!(cut.entries.len(), 2);
    assert!(cut.truncated);
    assert!(!full.truncated);

    let shallow = entries_with(
        &fs,
        &EntryLimit {
            max_entries: usize::MAX,
            max_depth: 1,
        },
    );
    assert!(
        shallow
            .entries
            .iter()
            .all(|e| !e.name.as_str().contains('/')),
        "深度 1 只列根下条目"
    );

    assert!(matches!(
        fs.entries(&EntryLimit::default(), &cancelled()),
        Err(VolError::Cancelled)
    ));
}

/// TC-CONF-009：只读卷的 `flush` 是空操作（一个字节都不写）。
#[test]
fn tc_conf_009_read_only_flush_writes_nothing() {
    let (chain, dev) = read_only_chain(fixture());
    let before = bytes_of(&dev);
    let mut fs = mount(dev.clone(), &read_only()).expect("挂载");
    assert!(fs.entries(&EntryLimit::default(), &cancel()).is_ok());
    fs.flush().expect("flush");
    assert_eq!(bytes_of(&dev), before, "只读卷 flush 不许写盘");
    let err = fs
        .apply(&Change::MakeDir { name: name("nope") }, None, &cancel())
        .expect_err("只读卷拒写");
    assert!(matches!(err, VolError::ReadOnly(_)), "得到 {err:?}");
    drop(chain);
}

/// TC-CONF-010：`audit()` 如实——干净卷不 skipped；脏卷 findings 带理由键。
#[test]
fn tc_conf_010_audit_is_truthful() {
    let fs = mount(device(fixture()), &writable()).expect("挂载");
    let clean = fs.audit().expect("audit");
    assert!(!clean.skipped, "上游 fsck 只读子集可用 ⇒ 不该记「未实现」");
    assert_eq!(
        clean.findings,
        Vec::<String>::new(),
        "干净卷不该有 findings"
    );
    assert!(clean.is_clean());

    let dirty = mount(device(ntfs_image_dirty(&[("a.txt", b"a")])), &writable()).expect("挂载");
    let report = dirty.audit().expect("audit");
    assert!(!report.skipped);
    assert!(!report.is_clean(), "带 dirty 标志的卷不算干净");
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.contains("vol.readonly.dirty")),
        "findings 要带上稳定键：{:?}",
        report.findings
    );
}
