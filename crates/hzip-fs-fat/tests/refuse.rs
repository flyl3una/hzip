//! 拒写与坏盘：坏 BPB/位翻转不 panic、只读设备只读、三种 FAT 宽度都被接住。
//!
//! 对应 T-IMG12 计划 §步骤 1 的 `tests/refuse.rs`。

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::*;
use hzip_format_testkit::vol::MemDevice;
use hzip_fs_fat::FatDriver;
use hzip_vol::{
    BlockDevice, FsDriver, FsKind, FsOptions as VolFsOptions, ReadOnlyReason, VolError,
};

/// 头部窗口（与 `hzip-vol` 的复核窗口一致：4 KiB）。
fn head(image: &[u8]) -> &[u8] {
    &image[..image.len().min(4096)]
}

/// 三种 FAT 宽度都**被接住**：`supports` 点头、能挂载、能列能读。
#[test]
fn fat12_and_fat16_and_fat32_are_supported() {
    let cases: [(&str, Vec<u8>); 3] = [
        ("FAT12", fat12(&[("A.TXT", b"12")])),
        ("FAT16", fat16(&[("A.TXT", b"16")])),
        ("FAT32", fat32(&[("A.TXT", b"32")])),
    ];
    let driver = FatDriver::new();
    for (width, image) in cases {
        assert!(
            driver.supports(FsKind::Fat, head(&image)),
            "{width}：嗅探结论 Fat + 头部复核都应点头"
        );
        // 嗅探结论不对 ⇒ 不接（注册表的契约是「两份证据都点头」）。
        assert!(
            !driver.supports(FsKind::Ntfs, head(&image)),
            "{width}：kind 不符不接"
        );
        let m = mount_image(image);
        assert!(
            tree(m.fs.as_ref()).by_name("A.TXT").is_some(),
            "{width} 应能列"
        );
        assert_eq!(
            String::from_utf8(read_name(m.fs.as_ref(), "A.TXT")).unwrap(),
            &width[3..],
            "{width} 应能读"
        );
    }
}

/// exFAT 是**另一个**文件系统：绝不用 FAT 驱动去读它（连 `supports` 都不许点头）。
#[test]
fn exfat_is_never_mounted_as_fat() {
    // exFAT 与 FAT 共享 0x55AA 引导扇区与偏移 3 的 OEM 名布局。
    let mut exfat = vec![0u8; 4096];
    exfat[3..11].copy_from_slice(b"EXFAT   ");
    exfat[510] = 0x55;
    exfat[511] = 0xAA;
    assert!(!FatDriver::new().supports(FsKind::Fat, &exfat));
    assert!(!FatDriver::new().supports(FsKind::ExFat, &exfat));

    // NTFS 同布局，同样不接。
    let mut ntfs = exfat.clone();
    ntfs[3..11].copy_from_slice(b"NTFS    ");
    assert!(!FatDriver::new().supports(FsKind::Fat, &ntfs));
}

/// 坏 BPB：挂载**报错**（不是 panic、不是空树）。每一项都是一处真实的字段损坏。
#[test]
fn corrupt_bpb_is_an_error_not_a_panic() {
    /// 一处字段损坏（就地改镜像字节）。
    type Patch = fn(&mut Vec<u8>);

    let good = fat32(&[("A.TXT", b"a")]);
    let patches: [(&str, Patch); 8] = [
        ("引导扇区签名丢失", |i: &mut Vec<u8>| {
            i[510] = 0;
            i[511] = 0;
        }),
        ("bytes_per_sector = 0", |i: &mut Vec<u8>| {
            i[11..13].copy_from_slice(&[0, 0]);
        }),
        ("bytes_per_sector = 12345", |i: &mut Vec<u8>| {
            i[11..13].copy_from_slice(&12345u16.to_le_bytes());
        }),
        ("sectors_per_cluster = 0", |i: &mut Vec<u8>| i[13] = 0),
        (
            "sectors_per_cluster = 3（非 2 的幂）",
            |i: &mut Vec<u8>| i[13] = 3,
        ),
        ("保留扇区 = 0", |i: &mut Vec<u8>| {
            i[14..16].copy_from_slice(&[0, 0]);
        }),
        ("FAT 份数 = 0", |i: &mut Vec<u8>| i[16] = 0),
        ("总扇区数 = 0", |i: &mut Vec<u8>| {
            i[19..21].copy_from_slice(&[0, 0]);
            i[32..36].copy_from_slice(&[0, 0, 0, 0]);
        }),
    ];
    for (label, patch) in patches {
        let mut broken = good.clone();
        patch(&mut broken);
        assert!(
            !FatDriver::new().supports(FsKind::Fat, head(&broken)),
            "{label}：头部复核应当拒绝"
        );
        let err = FatDriver::new()
            .mount(
                Arc::new(MemDevice::from_bytes(broken)) as Arc<dyn BlockDevice>,
                &VolFsOptions::default(),
            )
            .err()
            .unwrap_or_else(|| panic!("{label}：应挂载失败"));
        assert!(
            matches!(err, VolError::Image(_) | VolError::Io(_)),
            "{label}：应是结构/IO 错误，实际 {err:?}"
        );
    }

    // 全零、全 FF、随机短读：一样只报错不 panic。
    for (label, bytes) in [
        ("全零", vec![0u8; 4096]),
        ("全 FF", vec![0xFFu8; 4096]),
        ("短于一个扇区", vec![0u8; 100]),
    ] {
        assert!(!FatDriver::new().supports(FsKind::Fat, &bytes), "{label}");
        assert!(
            FatDriver::new()
                .mount(
                    Arc::new(MemDevice::from_bytes(bytes)) as Arc<dyn BlockDevice>,
                    &VolFsOptions::default()
                )
                .is_err(),
            "{label}：应挂载失败"
        );
    }
}

/// 位翻转：确定性的一批翻转点，**绝不 panic**；能挂上的就继续用读侧跑一遍。
///
/// 这里的价值不在「一定报错」（有些位翻转确实无害），而在「无论翻到哪一位，
/// 驱动都要么给出可用视图、要么给出错误——不是 panic，也不是假数据」。
///
/// 夹具用 **FAT12**（1.44 MiB，12 位表项最易错）做密集翻转，FAT32 再做一组抽样
/// 翻转覆盖「FAT 表区 + 数据区」——两档加起来不到一秒，密集且不慢。
#[test]
fn bit_flips_never_panic() {
    let round = |bytes: Vec<u8>| {
        // 挂载 → 列树 → 逐个条目读一遍 → 自检；任何一步 Err 都算合法结局。
        let Ok(fs) = FatDriver::new().mount(
            Arc::new(MemDevice::from_bytes(bytes)) as Arc<dyn BlockDevice>,
            &writable_opts(),
        ) else {
            return;
        };
        if let Ok(list) = fs.entries(&hzip_vol::EntryLimit::default(), &cancel()) {
            for e in &list.entries {
                if e.is_dir {
                    continue;
                }
                let mut buf = [0u8; 64];
                let _ = fs.read_at(e.id, 0, &mut buf);
            }
        }
        let _ = fs.audit();
    };

    // ① FAT12：引导扇区 + BPB 每 4 字节翻一次，其余区域每 4 KiB 翻一次。
    let small = fat12(&[
        ("A.TXT", b"aaaa"),
        ("dir/B.TXT", b"bbbb"),
        ("dir/C.TXT", &[0xCDu8; 3000]),
    ]);
    for offset in (0..512).step_by(4) {
        let mut broken = small.clone();
        broken[offset] ^= 0x5A;
        round(broken);
    }
    for offset in (4096..small.len()).step_by(4096) {
        let mut broken = small.clone();
        broken[offset] ^= 0xFF;
        round(broken);
    }

    // ② FAT32：表区 + 数据区抽样（步长 256 KiB，34 MiB ⇒ 约 130 次）。
    let big = fat32(&[
        ("A.TXT", b"aaaa"),
        ("dir/B.TXT", b"bbbb"),
        ("dir/C.TXT", &[0xCDu8; 3000]),
    ]);
    for offset in (512..big.len()).step_by(256 * 1024) {
        let mut broken = big.clone();
        broken[offset] ^= 0xFF;
        round(broken);
    }
}

/// 只读设备 ⇒ 只读能力 + `DeviceReadOnly` 理由；`apply` 被拒；**读侧完全不受影响**。
#[test]
fn read_only_device_yields_read_only_caps() {
    let image = fat32(&[("A.TXT", b"readable")]);
    let path = temp_dir().join("fat-readonly.img");
    std::fs::write(&path, &image).unwrap();
    let (mut fs, _chain) = mount_read_only(&path);

    let caps = fs.caps();
    assert!(!caps.write && !caps.delete && !caps.rename && !caps.mkdir);
    assert_eq!(caps.read_only_reason, Some(ReadOnlyReason::DeviceReadOnly));
    assert!(!caps.symlink && !caps.mode);
    assert_eq!(caps.name_max, 255);

    // 读照常。
    let list = fs
        .entries(&hzip_vol::EntryLimit::default(), &cancel())
        .unwrap();
    let id = list.by_name("A.TXT").expect("只读设备也要能列").id;
    let mut buf = [0u8; 16];
    let n = fs.read_at(id, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"readable");
    fs.audit().expect("只读卷也要能自检");

    // 写：五类变更一条都别想（理由如实）。
    let src = local_file("fat-ro.bin", b"nope");
    let err = fs
        .apply(
            &hzip_format::Change::Append {
                src: src.clone(),
                name: hzip_format::EntryName::new("NEW.TXT"),
            },
            Some(&src),
            &cancel(),
        )
        .expect_err("只读设备上的写必须被拒");
    match err {
        VolError::ReadOnly(reason) => assert_eq!(reason, ReadOnlyReason::DeviceReadOnly),
        other => panic!("只读设备应报 ReadOnly(DeviceReadOnly)，得到 {other:?}"),
    }

    // 设备层也没被写过一个字节。
    let after = std::fs::read(&path).unwrap();
    assert_eq!(after, image, "只读设备上不该有任何字节变化");
    let _ = std::fs::remove_file(&path);
}

/// 按设置只读（`FsOptions::writable = false`）：设备可写，但写侧能力仍全关 + `BySetting`。
#[test]
fn read_only_by_setting() {
    let image = fat32(&[("A.TXT", b"a")]);
    let opts = VolFsOptions {
        writable: false,
        ..VolFsOptions::default()
    };
    let fs = FatDriver::new()
        .mount(
            Arc::new(MemDevice::from_bytes(image)) as Arc<dyn BlockDevice>,
            &opts,
        )
        .expect("按设置只读也能挂载");
    let caps: hzip_vol::FsCaps = fs.caps();
    assert!(!caps.write);
    assert_eq!(caps.read_only_reason, Some(ReadOnlyReason::BySetting));
    // 只读挂载下 flush 是空操作（不是错误）。
    let mut fs = fs;
    fs.flush().expect("只读挂载的 flush 应是空操作");
}
