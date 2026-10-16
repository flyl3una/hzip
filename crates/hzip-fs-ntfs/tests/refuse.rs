//! 拒写用例（计划 `T-IMG11` 步骤 1 的 `tests/refuse.rs`）。
//!
//! 覆盖计划 §2.3 的拒写判定序：①休眠 ②日志含事务 ③dirty ④目录规模 ⑤变换过内容，
//! 外加「只读设备 / 只读设置」两档。每一条都同时钉住**理由键**与**盘上没被改动**。

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use hzip_format::Change;
use hzip_vol::{ReadOnlyReason, VolError};

#[test]
fn hibernated_volume_is_read_only() {
    let dir = tempfile::tempdir().expect("临时目录");
    let src = local_file(dir.path(), "x.txt", b"x");
    // 夹具在根目录放了头部以 `hibr` 开头的 hiberfil.sys（Windows 休眠/快速启动残留）。
    let image = ntfs_image_hibernated(&[("data.txt", b"data")]);
    let dev = device(image.clone());

    let mut fs = mount(dev.clone(), &writable()).expect("休眠卷仍可只读挂载");
    assert_eq!(fs.caps().read_only_reason, Some(ReadOnlyReason::Hibernated));
    assert_eq!(
        fs.caps().read_only_reason.unwrap().key(),
        "vol.readonly.hibernated"
    );
    assert!(!fs.caps().write && !fs.caps().delete && !fs.caps().mkdir);

    // 但列/读照旧（只读不是打不开）。
    assert_eq!(read_all(&fs, id_of(&fs, "data.txt"), 4), b"data");

    let err = fs
        .apply(
            &Change::Append {
                src,
                name: name("nope.txt"),
            },
            None,
            &cancel(),
        )
        .expect_err("休眠卷不许写");
    assert!(
        matches!(err, VolError::ReadOnly(ReadOnlyReason::Hibernated)),
        "得到 {err:?}"
    );

    // 挂载（含写挂载尝试）**绝不清标志**：设备字节一个都没变。
    fs.flush().expect("只读 flush");
    assert_eq!(
        bytes_of(&dev),
        image,
        "拒写路径不许改盘（尤其不许清休眠标志）"
    );
}

#[test]
fn log_holding_work_requires_explicit_replay() {
    let dir = tempfile::tempdir().expect("临时目录");
    let src = local_file(dir.path(), "x.txt", b"x");
    let image = ntfs_image_pending_log(&[("data.txt", b"data")]);
    let dev = device(image.clone());

    // 默认（`replay_journal == false`）：只读 + JournalPending。
    let mut fs = mount(dev.clone(), &writable()).expect("挂载");
    assert_eq!(
        fs.caps().read_only_reason,
        Some(ReadOnlyReason::JournalPending)
    );
    assert_eq!(
        fs.caps().read_only_reason.unwrap().key(),
        "vol.readonly.journal"
    );
    let err = fs
        .apply(
            &Change::Append {
                src: src.clone(),
                name: name("nope.txt"),
            },
            None,
            &cancel(),
        )
        .expect_err("日志含事务时默认拒写");
    assert!(matches!(err, VolError::ReadOnly(_)), "得到 {err:?}");
    assert_eq!(bytes_of(&dev), image, "默认拒写不许动盘");

    // 显式允许重放：本夹具的日志**解析不出可重放的客户记录**（把首页清零就是模拟
    // 「Windows 没干净卸载」），故这次写挂载会被上游拒 ⇒ 驱动降级为只读 + 同一理由。
    // 契约要的正是「拿不到可写就明确说不出为什么」，而不是硬写或假装成功。
    let fs2 =
        mount(dev.clone(), &writable_replaying()).expect("显式允许重放后仍可挂载（只读降级）");
    assert!(!fs2.caps().write, "不可重放的日志 ⇒ 仍然只读");
    assert_eq!(
        fs2.caps().read_only_reason,
        Some(ReadOnlyReason::JournalPending),
        "降级理由必须是 JournalPending"
    );
    assert_eq!(bytes_of(&dev), image, "重放失败必须「什么都没写」");
}

#[test]
fn dirty_flag_is_reported() {
    let image = ntfs_image_dirty(&[("data.txt", b"data")]);
    let dev = device(image.clone());

    // 上游 `prepare_rw_mount` 对 dirty 卷直接拒（dirty = 要求一次全卷检查）⇒ 驱动如实
    // 给只读 + Dirty（计划 §2.3 ③ 写的是「可写 + 提示」，实测上游口径是拒：见 crate 文档）。
    let fs = mount(dev.clone(), &writable()).expect("挂载");
    assert_eq!(fs.caps().read_only_reason, Some(ReadOnlyReason::Dirty));
    assert_eq!(
        fs.caps().read_only_reason.unwrap().key(),
        "vol.readonly.dirty"
    );
    assert!(!fs.caps().write);
    // 只读也不影响列/读。
    assert_eq!(read_all(&fs, id_of(&fs, "data.txt"), 4), b"data");
    // 提示语要真的出现在 audit() 里（不然用户只看到一个「只读」，不知道去哪修）。
    let summary = fs.audit().expect("audit");
    assert!(
        summary
            .findings
            .iter()
            .any(|f| f.contains("vol.readonly.dirty")),
        "dirty 提示必须在 findings 里：{:?}",
        summary.findings
    );
    assert_eq!(bytes_of(&dev), image, "拒写路径不许清 dirty 标志");
}

#[test]
fn read_only_device_is_refused_with_device_reason() {
    // 只读设备链（产品的 `WriteMode::ReadOnly`：`is_writable() == false`）。
    let (chain, dev) = read_only_chain(ntfs_image(&[("data.txt", b"data")]));
    assert!(!dev.is_writable(), "夹具必须是只读设备");
    let mut fs = mount(dev, &writable()).expect("挂载");
    assert_eq!(
        fs.caps().read_only_reason,
        Some(ReadOnlyReason::DeviceReadOnly)
    );
    assert_eq!(
        fs.caps().read_only_reason.unwrap().key(),
        "vol.readonly.device"
    );
    assert!(!fs.caps().write);
    assert_eq!(read_all(&fs, id_of(&fs, "data.txt"), 4), b"data");
    let err = fs
        .apply(&Change::Delete(id_of(&fs, "data.txt")), None, &cancel())
        .expect_err("只读设备不许删");
    assert!(
        matches!(err, VolError::ReadOnly(ReadOnlyReason::DeviceReadOnly)),
        "得到 {err:?}"
    );
    fs.flush().expect("只读 flush");
    drop(chain);
}

#[test]
fn read_only_setting_is_reported_as_by_setting() {
    let dev = device(ntfs_image(&[("data.txt", b"data")]));
    assert!(dev.is_writable(), "设备本身可写");
    let fs = mount(dev, &read_only()).expect("挂载");
    assert_eq!(fs.caps().read_only_reason, Some(ReadOnlyReason::BySetting));
    assert!(!fs.caps().write);
}

/// ④ 目录规模：把目录索引顶到上游上限（每目录 64 块）后，**写前**拒绝。
///
/// 名字取长（200 个 UTF-16 码元）是为了让 4 KiB 的索引块只装得下几条 ⇒ 几百个条目
/// 就能把 64 块用满（用短名要上万条，测试跑不动）。
#[test]
fn directory_beyond_index_block_limit_is_refused() {
    let dir = tempfile::tempdir().expect("临时目录");
    let dev = device(ntfs_image(&[("big/seed.txt", b"seed")]));
    let mut fs = mount(dev.clone(), &writable()).expect("挂载");

    let long_name = |index: usize| -> String {
        let mut pad = "长".repeat(196);
        pad.push_str(&format!("{index:04}"));
        format!("big/{pad}")
    };

    let mut created = 0usize;
    let mut refused_at = None;
    // 上限 2000 只是为了「跑不完就失败」，不是预期值：正常在几百条时被拒。
    for index in 0..2000 {
        let target = long_name(index);
        let path = local_file(dir.path(), &format!("p{index}.bin"), b"p");
        match fs.apply(
            &Change::Append {
                src: path,
                name: name(&target),
            },
            None,
            &cancel(),
        ) {
            Ok(()) => created += 1,
            Err(VolError::Unsupported("vol.unsupported.dir-too-large")) => {
                refused_at = Some(index);
                break;
            }
            Err(e) => panic!("第 {index} 次 Append 出人意料地失败：{e:?}"),
        }
    }
    let refused_at = refused_at.expect("目录索引顶满后必须出现 dir-too-large 拒绝");
    assert!(
        created > 100,
        "应当先真的长大了目录索引再被拒（实际只建了 {created} 个）"
    );

    // 再写一次：仍是同一个理由键（且不是上游的 no-space —— 上游会说盘满，我们提前拒）。
    let again = fs
        .apply(
            &Change::Append {
                src: local_file(dir.path(), "again.bin", b"again"),
                name: name(&long_name(9999)),
            },
            None,
            &cancel(),
        )
        .expect_err("满目录的写入必须稳定被拒");
    assert!(
        matches!(
            again,
            VolError::Unsupported("vol.unsupported.dir-too-large")
        ),
        "得到 {again:?}"
    );
    assert_ne!(refused_at, 0, "不该在第一次写就拒（那就说明判据错了）");

    // 拒写不半写：满目录下**删除**仍然可用（这是用户自救的出口），卷仍然列得完整。
    let victim = long_name(0);
    let victim_id = id_of(&fs, &victim);
    fs.apply(&Change::Delete(victim_id), None, &cancel())
        .expect("满目录下删除必须可用");
    assert!(entries(&fs).by_name(&victim).is_none());

    let tree = fs
        .entries(&EntryLimit::default(), &cancel())
        .expect("列目录");
    assert!(
        tree.entries.len() > created,
        "长名条目一个都不能丢（{} 条）",
        tree.entries.len()
    );
    let summary = fs.audit().expect("audit");
    assert!(!summary.skipped, "只读 fsck 子集应当跑完");
}

/// ⑤ WOF（Compact OS）压缩文件：条目可见但只读，读/写都被拒且有理由。
#[test]
fn compressed_or_windows_filtered_files_are_read_only_entries() {
    let dir = tempfile::tempdir().expect("临时目录");
    let replacement = local_file(dir.path(), "new.txt", b"new");
    let image = ntfs_image(&[("plain.txt", b"plain content"), ("woffed.bin", b"stale")]);
    let image = mark_wof(image, "woffed.bin");
    let dev = device(image);
    let mut fs = mount(dev, &writable()).expect("挂载");

    let tree = entries(&fs);
    let wof = tree
        .by_name("woffed.bin")
        .expect("WOF 条目仍然可见（不丢条目）");
    assert!(wof.attributes.read_only, "变换过的条目在列表里标只读");
    assert_eq!(wof.codec.as_deref(), Some("wof"), "codec 如实标 WOF");

    // 读：拒（上游对 WOF 的 `$DATA` 也会拒，但我们要给稳定键，而不是让上游的
    // Io 错误透出去）。
    let err = fs
        .read_at(wof.id, 0, &mut [0u8; 8])
        .expect_err("WOF 内容读不出");
    assert!(
        matches!(err, VolError::Unsupported("vol.unsupported.compressed")),
        "得到 {err:?}"
    );

    // 写：拒（不半写）。
    let err = fs
        .apply(
            &Change::Replace {
                entry: wof.id,
                src: replacement.clone(),
            },
            Some(&replacement),
            &cancel(),
        )
        .expect_err("WOF 内容不许写");
    assert!(
        matches!(err, VolError::Unsupported("vol.unsupported.compressed")),
        "得到 {err:?}"
    );

    // 不整卷失败：普通条目照读照写。
    let plain = id_of(&fs, "plain.txt");
    assert_eq!(read_all(&fs, plain, 13), b"plain content");
    fs.apply(
        &Change::Replace {
            entry: plain,
            src: replacement.clone(),
        },
        Some(&replacement),
        &cancel(),
    )
    .expect("普通文件照旧可写");
    assert_eq!(read_all(&fs, plain, 3), b"new");
}

/// ⑤ EFS 加密文件：同上（上游没有「造 EFS 文件」的写接口 ⇒ 夹具直接改位）。
#[test]
fn encrypted_files_are_read_only_entries() {
    let dir = tempfile::tempdir().expect("临时目录");
    let replacement = local_file(dir.path(), "new.txt", b"new");
    let image = ntfs_image(&[("plain.txt", b"plain"), ("efs.bin", b"secret")]);
    let image = mark_efs(image, "efs.bin");
    let dev = device(image);
    let mut fs = mount(dev, &writable()).expect("挂载");

    let tree = entries(&fs);
    let efs = tree.by_name("efs.bin").expect("EFS 条目仍然可见");
    assert!(efs.attributes.read_only, "EFS 条目在列表里标只读");
    assert!(efs.encrypted, "encrypted 位如实置位");
    assert_eq!(efs.encryption, Some("EFS"));

    let err = fs
        .read_at(efs.id, 0, &mut [0u8; 8])
        .expect_err("EFS 内容读不出（没有用户密钥）");
    assert!(
        matches!(err, VolError::Unsupported("vol.unsupported.compressed")),
        "得到 {err:?}"
    );
    let err = fs
        .apply(
            &Change::Replace {
                entry: efs.id,
                src: replacement.clone(),
            },
            Some(&replacement),
            &cancel(),
        )
        .expect_err("EFS 内容不许写");
    assert!(
        matches!(err, VolError::Unsupported("vol.unsupported.compressed")),
        "得到 {err:?}"
    );
    // 普通条目照常。
    let plain = id_of(&fs, "plain.txt");
    assert_eq!(read_all(&fs, plain, 5), b"plain");
}

#[test]
fn illegal_names_are_refused_before_touching_the_volume() {
    let dir = tempfile::tempdir().expect("临时目录");
    let src = local_file(dir.path(), "x.txt", b"x");
    let image = ntfs_image(&[]);
    let dev = device(image.clone());
    let mut fs = mount(dev.clone(), &writable()).expect("挂载");

    for bad in ["", ".", "..", "dir/name.", "dir/name ", "nul\u{0}"] {
        let err = fs
            .apply(
                &Change::Append {
                    src: src.clone(),
                    name: name(bad),
                },
                None,
                &cancel(),
            )
            .expect_err("非法名必须被拒");
        assert!(
            matches!(
                err,
                VolError::Unsupported("vol.err.invalid-name")
                    | VolError::Unsupported("vol.err.name-too-long")
            ),
            "{bad:?} ⇒ 得到 {err:?}"
        );
    }
    // 超长名（>255 码元）单独一条。
    let err = fs
        .apply(
            &Change::Append {
                src: src.clone(),
                name: name(&"汉".repeat(256)),
            },
            None,
            &cancel(),
        )
        .expect_err("超长名必须被拒");
    assert!(
        matches!(err, VolError::Unsupported("vol.err.name-too-long")),
        "得到 {err:?}"
    );
    assert_eq!(bytes_of(&dev), image, "被拒的写不许动盘");
}
