use super::*;
use std::io::Read;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hzip-7z-split-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 分卷字节 == 单卷顺序拼接；各卷 ≤ vol_bytes（末卷除外）；卷文件齐全。
fn run_matrix(vol_bytes: u64, data: &[u8]) {
    let dir = tmpdir("matrix");
    let mut sp = SplitSink::new(&dir, "arc", vol_bytes).unwrap();
    sp.write_all(data).unwrap();
    let vols = sp.finalize().unwrap();
    assert!(!vols.is_empty(), "至少一卷");
    assert_eq!(vols[0].file_name().unwrap().to_str().unwrap(), "arc.7z.001");
    let mut joined = Vec::new();
    for (i, p) in vols.iter().enumerate() {
        let bytes = std::fs::read(p).unwrap();
        assert!(
            bytes.len() as u64 <= vol_bytes || i + 1 == vols.len(),
            "非末卷 ≤ vol_bytes"
        );
        joined.extend_from_slice(&bytes);
    }
    assert_eq!(joined, data, "卷拼接 == 单卷字节");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn split_write_matrix() {
    // 三档：vol < 单文件；介于条目间；> 总量（单卷）
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    run_matrix(1024, &data);
    run_matrix(100_000, &data);
    run_matrix(1_000_000, &data);
    run_matrix(64, b"7z\xBC\xAF\x27\x1C-tiny");
}

#[test]
fn split_min_volume_guard() {
    for bad in [0u64, 1, 31] {
        let dir = tmpdir("guard");
        let e = SplitSink::new(&dir, "arc", bad).expect_err("vol<32 应报错");
        assert_eq!(e.code(), 5001, "防护错误码（Unsupported 类）");
        assert_eq!(
            std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0),
            0,
            "不得产出被切穿签名头的残卷"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn split_volumes_readable_as_set() {
    // 读侧装配：拼出的卷集按 VolumeSetReader 语义逐卷读 == 原文
    let dir = tmpdir("read");
    let data: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let mut sp = SplitSink::new(&dir, "arc", 7000).unwrap();
    sp.write_all(&data).unwrap();
    let vols = sp.finalize().unwrap();
    let mut got = Vec::new();
    for p in &vols {
        let mut f = File::open(p).unwrap();
        f.read_to_end(&mut got).unwrap();
    }
    assert_eq!(got, data);
    let _ = std::fs::remove_dir_all(&dir);
}
