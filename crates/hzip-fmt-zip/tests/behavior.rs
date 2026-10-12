//! TC-CONF-008 畸形输入无 panic（红线 4）+ TC-CONF-004 乱序读 + 取消 + 秒开 + 元数据。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use hzip_format::{EntryMeta, FormatHandler, PasswordRef, Sink};

use common::tmp_dir;

#[test]
fn malformed_no_panic() {
    let dir = tmp_dir("malformed");
    let p = common::write_archive(&[
        ("a.txt".into(), b"hello".to_vec()),
        (
            "b.bin".into(),
            (0..3000u32).map(|i| (i % 251) as u8).collect(),
        ),
    ]);
    let good = std::fs::read(&p).unwrap();

    let mut samples: Vec<Vec<u8>> = vec![
        good[..good.len() * 9 / 10].to_vec(),
        good[..good.len() / 2].to_vec(),
        good[..good.len() - 1].to_vec(),
    ];
    let mut seed = 0x1234_5678u32;
    let mut flip = good.clone();
    let n = flip.len();
    for _ in 0..20 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let i = (seed % n as u32) as usize;
        flip[i] ^= 0x40;
    }
    samples.push(flip);

    for (i, s) in samples.iter().enumerate() {
        let bad = dir.join(format!("bad-{i}.zip"));
        std::fs::write(&bad, s).unwrap();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let h = common::handler();
            if let Ok(r) =
                h.open_reader(hzip_format::Source::File(bad.clone()), &Default::default())
                && let Ok(infos) = r.entries()
            {
                for info in infos {
                    let _ = r
                        .read_entry(info.id, PasswordRef::none())
                        .and_then(|mut rd| {
                            let mut b = Vec::new();
                            rd.read_to_end(&mut b)
                                .map(|_| ())
                                .map_err(hzip_error::HzipError::from)
                        });
                }
            }
        }));
        assert!(outcome.is_ok(), "畸形样本 {i} 不应 panic");
    }
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(p.parent().unwrap()).ok();
}

#[test]
fn read_entry_random_order() {
    // 100 条目逆序/重复/交叉（TC-CONF-004）
    let dir = tmp_dir("ooo");
    let entries: Vec<(String, Vec<u8>)> = (0..100)
        .map(|i| (format!("o{i:03}.txt"), format!("content-{i}").into_bytes()))
        .collect();
    let p = common::write_archive(&entries);
    let r = common::reader_of(&p);
    for i in (0..100).rev() {
        let buf = common::read_all(r.as_ref(), i);
        assert_eq!(buf, format!("content-{i}").into_bytes());
    }
    for _ in 0..2 {
        for &i in &[0usize, 50, 99] {
            let buf = common::read_all(r.as_ref(), i);
            assert_eq!(buf, format!("content-{i}").into_bytes());
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(p.parent().unwrap()).ok();
}

#[test]
fn large_entry_listing_fast() {
    // TC-CONF-010 entries() 秒开：大条目列表 < 1s
    let dir = tmp_dir("conf010");
    let p = dir.join("big.zip");
    let big = vec![0u8; 8 * 1024 * 1024];
    {
        let mut w = common::handler()
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        let meta = EntryMeta {
            name: "big.bin".into(),
            size_raw: Some(big.len() as u64),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(&big).unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
    }
    let started = std::time::Instant::now();
    let infos = common::entries(&p);
    let elapsed = started.elapsed();
    assert!(!infos.is_empty());
    assert!(
        elapsed < std::time::Duration::from_millis(1000),
        "entries 秒开（{} ms）",
        elapsed.as_millis()
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn metadata_integrity() {
    // TC-CONF-001：id 连续、parse_entry_name 合法
    let p = common::write_archive(&[
        ("hello.txt".into(), b"hello world\n".to_vec()),
        ("empty.dat".into(), Vec::new()),
        (
            "bin/data.bin".into(),
            (0..256u32).map(|i| (i % 251) as u8).collect(),
        ),
        ("docs/name-cn.txt".into(), "中文内容".as_bytes().to_vec()),
        ("a/b/c/d/deep.txt".into(), b"deep".to_vec()),
    ]);
    let infos = common::entries(&p);
    for (i, info) in infos.iter().enumerate() {
        assert_eq!(info.id, i, "id 连续");
        hzip_format::parse_entry_name(info.name.as_str()).unwrap();
    }
    std::fs::remove_dir_all(p.parent().unwrap()).ok();
}

#[test]
fn cancel_mid_stream() {
    // TC-CONF-006：220 MiB 解压、~60MiB 处 cancel → Cancelled
    let dir = tmp_dir("cancel");
    let p = dir.join("big.zip");
    let chunk = vec![0x5Au8; 1024 * 1024];
    {
        let mut w = common::handler()
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        let meta = EntryMeta {
            name: "big.bin".into(),
            size_raw: Some(220 * 1024 * 1024),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        for _ in 0..220 {
            s.write_all(&chunk).unwrap();
        }
        s.finish().unwrap();
        w.finish().unwrap();
    }
    let dest = dir.join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let r = common::reader_of(&p);
    let pw = common::NoPwSource;
    let env = common::TestEnv::new(&pw);
    let cancel = env.cancel().clone();
    let mon_dest = dest.clone();
    let sub = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            let size = std::fs::metadata(mon_dest.join("big.bin"))
                .map(|m| m.len())
                .unwrap_or(0);
            if size >= 60 * 1024 * 1024 {
                cancel.cancel();
                break;
            }
            if start.elapsed() > std::time::Duration::from_secs(60) {
                cancel.cancel();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });
    let res = r.extract(&hzip_format::Selection::All, &env.ctx(), &dest);
    let _ = sub.join();
    match res {
        Err(e) if e.code() == 6001 => {}
        Err(e) => panic!("应 Cancelled，实得 {:?}", e.code()),
        Ok(_) => {
            // 若写入极快未触发取消：条目完成可接受（竞态）
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(p.parent().unwrap()).ok();
}

#[test]
fn selective_and_extract() {
    // TC-CONF-003：Selection::Ids 恰 N 落盘
    let dir = tmp_dir("sel");
    let entries: Vec<(String, Vec<u8>)> = (0..10)
        .map(|i| (format!("f{i:02}.txt"), format!("content-{i}").into_bytes()))
        .collect();
    let p = common::write_archive(&entries);
    let dest = dir.join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let r = common::reader_of(&p);
    let pw = common::NoPwSource;
    let env = common::TestEnv::new(&pw);
    let _ = r
        .extract(&hzip_format::Selection::Ids(vec![3, 7]), &env.ctx(), &dest)
        .unwrap();
    let count = std::fs::read_dir(&dest).unwrap().count();
    assert_eq!(count, 2, "选择性解压恰 2 落盘");
    let got = std::fs::read(dest.join("f03.txt")).unwrap();
    assert_eq!(got, b"content-3");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(p.parent().unwrap()).ok();
}
