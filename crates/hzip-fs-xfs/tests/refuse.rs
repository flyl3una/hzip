//! 拒写与坏盘：坏魔数/坏校验和/位翻转不 panic、只读设备只读、脏日志只读但读得到、
//! 未知 id 与太小/全零的卷按名拒绝。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use std::sync::Arc;

use common::*;
use fs_core::{BlockDevice, ReadOnlyDevice};
use hzip_format::Change;
use hzip_format_testkit::vol::MemDevice;
use hzip_fs_xfs::XfsDriver;
use hzip_vol::{FsDriver, FsKind, FsOptions, ReadOnlyReason, VolError};

/// 挂载任意设备（测试要自己拼坏盘）。
fn mount(
    dev: Arc<dyn hzip_vol::BlockDevice>,
    opts: &FsOptions,
) -> Result<Box<dyn hzip_vol::FsVolume>, VolError> {
    XfsDriver::new().mount(dev, opts)
}

/// 改写镜像里的一段字节（坏盘用例：只动我们指定的那几个字节）。
fn corrupted(image: &SparseDevice, at: u64, patch: &[u8]) -> SparseDevice {
    let broken = image.snapshot();
    BlockDevice::write_at(&broken, at, patch).expect("夹具：改坏字节");
    broken
}

/// 坏魔数：`supports` 不点头，挂载按结构错误拒绝（`NotXfs`，不 panic）。
#[test]
fn bad_magic_is_refused() {
    let img = image(&[Node::File("a.txt", b"a")]);
    let broken = corrupted(&img, 0, b"YFSB");
    assert!(!XfsDriver::new().supports(FsKind::Xfs, &broken.head()));
    let err = mount(Arc::new(broken), &FsOptions::default())
        .err()
        .unwrap();
    assert!(matches!(err, VolError::Image(_)), "{err:?}");
}

/// 坏校验和：v5 的超级块有 CRC32C（**小端**存放），翻一位就该被拒——且是
/// `Image`（结构不合法），不是 panic、也不是「当成好卷继续读」。
#[test]
fn flipped_bit_in_the_superblock_is_refused_not_panicked() {
    let img = image(&[Node::File("a.txt", b"a")]);
    // 120 = `sb_blocklog`（在超级块结构里，且在被 CRC 覆盖的那个扇区里）。
    let broken = corrupted(&img, 120, &[0x7f]);
    let err = mount(Arc::new(broken), &FsOptions::default())
        .err()
        .unwrap();
    assert!(matches!(err, VolError::Image(_)), "{err:?}");
}

/// 位翻转落在**数据块**上：挂载必须成功（结构没坏），读出来的字节不对是镜像的事；
/// 列树与自检都不许 panic。
#[test]
fn flipped_bit_in_a_data_block_does_not_panic() {
    let payload = b"payload payload payload";
    let img = image(&[Node::File("a.txt", payload)]);
    // 文件数据在盘上的位置由上游的 extent 表直接给出来（别去猜分配器把块放哪了）。
    let at = {
        let dev: Arc<dyn hzip_vol::BlockDevice> = Arc::new(img.clone());
        let xfs = fs_xfs::Filesystem::mount(dev).unwrap();
        let file = xfs.open("a.txt").unwrap();
        let extents = xfs.data_extents(file.inode(), file.raw()).unwrap();
        assert_eq!(extents.len(), 1, "小文件应当只有一条 extent");
        extents[0].startblock * u64::from(xfs.superblock().blocksize)
    };
    let mut page = img.read_bytes(at, 4096);
    assert_eq!(
        &page[..payload.len()],
        payload,
        "extent 首块开头就是文件内容"
    );
    page[3] ^= 0xff;
    let broken = corrupted(&img, at, &page[..]);
    assert!(XfsDriver::new().supports(FsKind::Xfs, &broken.head()));

    let fs = mount(Arc::new(broken), &FsOptions::default()).expect("结构没坏就该挂得上");
    let t = fs.entries(&limit(), &cancel()).unwrap();
    assert_eq!(t.entries.len(), 1);
    let data = read_all(&*fs, t.entries[0].id);
    assert_eq!(data.len(), payload.len());
    assert_ne!(data, payload.to_vec(), "翻过的位当然读得出不同");
    // 自检可能发现（也可能不发现）这处数据改动——**不许 panic**，结果如实给出。
    let report = fs.audit().unwrap();
    let _ = report.is_clean();
}

/// 位翻转落在 **AG 头部**（元数据）：要么挂载就拒，要么挂上了但自检报出问题；
/// 两条路都不许 panic。
#[test]
fn flipped_bit_in_ag_metadata_is_refused_or_reported() {
    let img = image(&[Node::File("a.txt", b"a")]);
    // 第二个 AG 的头部：320 MiB / 4 个 AG ⇒ AG1 从 80 MiB 起，它的第 0 块是超级块副本
    // + AGF/AGI/AGFL。
    let at = IMAGE_BYTES / 4;
    let mut page = img.read_bytes(at, 4096);
    page[8] ^= 0xff;
    let broken = corrupted(&img, at, &page[..]);

    match mount(Arc::new(broken.clone()), &FsOptions::default()) {
        Err(e) => assert!(matches!(e, VolError::Image(_)), "{e:?}"),
        Ok(fs) => {
            let report = fs.audit().unwrap();
            assert!(
                !report.is_clean(),
                "坏掉一个 AG 头，自检不该说卷是干净的：{report:?}"
            );
        }
    }
    // 列树也不许 panic（可能报错、可能少列些东西）。
    if let Ok(fs) = mount(Arc::new(broken), &FsOptions::default()) {
        let _ = fs.entries(&limit(), &cancel());
    }
}

/// 全零盘 / 太小的盘：按结构错误拒绝，读头部之前先查容量。
#[test]
fn zeroed_and_tiny_devices_are_refused() {
    let zero: Arc<dyn hzip_vol::BlockDevice> = Arc::new(MemDevice::new(4096));
    let err = mount(zero, &FsOptions::default()).err().unwrap();
    assert!(matches!(err, VolError::Image(_)), "{err:?}");

    let tiny: Arc<dyn hzip_vol::BlockDevice> = Arc::new(MemDevice::new(512));
    let err = mount(tiny, &FsOptions::default()).err().unwrap();
    assert!(matches!(err, VolError::Image(_)), "{err:?}");
}

/// 只读设备 ⇒ 即使请求可写也只能只读，理由是 `DeviceReadOnly`（写起来才知道的错不算）。
#[test]
fn read_only_device_yields_read_only_caps() {
    let img = image(&[Node::File("a.txt", b"a")]);
    let inner: Arc<dyn fs_core::BlockRead> = Arc::new(img.clone());
    let dev: Arc<dyn hzip_vol::BlockDevice> = Arc::new(ReadOnlyDevice::new(inner));

    let mut fs = mount(
        dev,
        &FsOptions {
            writable: true,
            ..FsOptions::default()
        },
    )
    .unwrap();
    let caps = fs.caps();
    assert!(!caps.write);
    assert_eq!(caps.read_only_reason, Some(ReadOnlyReason::DeviceReadOnly));
    assert_eq!(
        read_name(&*fs, "a.txt"),
        b"a".to_vec(),
        "只读设备照样读得到"
    );
    assert!(fs.audit().unwrap().is_clean());

    let err = fs
        .apply(
            &Change::MakeDir {
                name: "nope".into(),
            },
            None,
            &cancel(),
        )
        .err()
        .unwrap();
    assert!(
        matches!(err, VolError::ReadOnly(ReadOnlyReason::DeviceReadOnly)),
        "拒写要带同一个理由：{err:?}"
    );
    assert_eq!(img.writes(), 0, "只读设备上一个字节都不许写");
}

/// 脏日志（crash 留下的未重放记录）：
///
/// 1. 只读挂载**照样读得到**——上游把日志在**内存里**重放，于是看到的是「内核恢复
///    之后」的样子，而设备**一个字节不动**；
/// 2. 请求可写 ⇒ 仍然只读 + `JournalPending`（上游没有「重放写进设备」的入口）；
/// 3. `replay_journal: true` 也**不能**改变这一点（不假装做到了，见 crate 文档）。
#[test]
fn dirty_log_is_read_only_but_replays_in_memory() {
    let img = dirty_image(&[Node::File("a.txt", b"aaa"), Node::File("dir/b.txt", b"bbb")]);
    let before = img.snapshot();
    img.reset_writes();

    // ① 只读挂载：读得到，设备不动。
    let mut fs = mount(
        Arc::new(img.clone()),
        &FsOptions {
            writable: true,
            replay_journal: false,
            ..FsOptions::default()
        },
    )
    .unwrap();
    let caps = fs.caps();
    assert!(!caps.write, "脏卷默认拒写");
    assert_eq!(caps.read_only_reason, Some(ReadOnlyReason::JournalPending));
    assert_eq!(read_name(&*fs, "a.txt"), b"aaa".to_vec());
    assert_eq!(read_name(&*fs, "dir/b.txt"), b"bbb".to_vec());
    let err = fs
        .apply(
            &Change::MakeDir {
                name: "nope".into(),
            },
            None,
            &cancel(),
        )
        .err()
        .unwrap();
    assert!(
        matches!(err, VolError::ReadOnly(ReadOnlyReason::JournalPending)),
        "拒写要带同一个理由：{err:?}"
    );
    drop(fs);
    assert_eq!(img.writes(), 0, "读侧内存重放不许碰设备");
    assert!(
        img.differing_pages(&before).is_empty(),
        "重放只在内存里，设备逐字节不变"
    );

    // ② 显式允许重放：仍然只读 + 同一个理由（上游只重放进内存）。
    let fs = mount(
        Arc::new(img.clone()),
        &FsOptions {
            writable: true,
            replay_journal: true,
            ..FsOptions::default()
        },
    )
    .unwrap();
    assert!(!fs.caps().write, "上游没有「重放写进设备」的入口");
    assert_eq!(
        fs.caps().read_only_reason,
        Some(ReadOnlyReason::JournalPending)
    );
    assert_eq!(read_name(&*fs, "a.txt"), b"aaa".to_vec());
    drop(fs);
    assert_eq!(img.writes(), 0, "「允许重放」也不许写脏卷");
    assert!(img.differing_pages(&before).is_empty());

    // ③ 不请求可写：仍然是 `JournalPending`——卷本身有待重放的东西，跟「按设置只读」
    //    不是一回事（与 ext4 侧同一口径）。
    let fs = mount(Arc::new(img.clone()), &FsOptions::default()).unwrap();
    assert!(!fs.caps().write);
    assert_eq!(
        fs.caps().read_only_reason,
        Some(ReadOnlyReason::JournalPending)
    );
    assert_eq!(read_name(&*fs, "a.txt"), b"aaa".to_vec());
}

/// 干净卷按 `BySetting` 只读（不请求可写），且带 `cache_blocks` 也照样能挂能读。
#[test]
fn clean_volume_read_only_by_setting_and_cache_blocks_are_honoured() {
    let img = image(&[Node::File("a.txt", b"a")]);
    let fs = mount(
        Arc::new(img),
        &FsOptions {
            writable: false,
            replay_journal: false,
            cache_blocks: 32,
        },
    )
    .unwrap();
    assert!(!fs.caps().write);
    assert_eq!(fs.caps().read_only_reason, Some(ReadOnlyReason::BySetting));
    assert_eq!(read_name(&*fs, "a.txt"), b"a".to_vec());
}
