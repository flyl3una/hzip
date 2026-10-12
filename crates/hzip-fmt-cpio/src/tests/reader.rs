use super::*;
use hzip_format_testkit::cpio::{self, CpioVariant as V, EntrySpec};

/// 读计数包络：证明索引期**不读数据区**（红线 1 / TC-CONF-010）。
struct Counting<R> {
    inner: R,
    bytes: u64,
}
impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }
}
impl<R: Seek> Seek for Counting<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

fn reader_for(path: &Path) -> CpioReader {
    CpioReader {
        path: path.to_path_buf(),
        format: FormatId::from("cpio"),
        cache: std::sync::Mutex::new(None),
        _spool: None,
    }
}

/// 索引只读「头 + 名字」：数据区 8 MiB 的档案索引期 I/O 也必须远小于它。
#[test]
fn index_reads_headers_only() {
    let data = vec![0xABu8; 8 * 1024 * 1024];
    let bytes = cpio::build(
        V::Newc,
        &[
            EntrySpec::file("big.bin", data),
            EntrySpec::file("a.txt", b"hello"),
        ],
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("big.cpio");
    std::fs::write(&path, &bytes).expect("write fixture");
    let r = reader_for(&path);

    let mut counting = Counting {
        inner: std::io::BufReader::new(std::fs::File::open(&path).expect("open")),
        bytes: 0,
    };
    let idx = r.index_from(&mut counting).expect("建索引");
    let read_bytes = counting.bytes;
    assert_eq!(idx.len(), 2, "两条目（无根条目）");
    assert!(
        read_bytes < 64 * 1024,
        "索引只读头与名字：实际 {read_bytes} 字节"
    );
    assert_eq!(idx[0].info.size_raw, 8 * 1024 * 1024, "声明大小仍如实上报");
}

/// 各变体的索引几何一致（同一逻辑档案 → 同一条目集）。
#[test]
fn all_variants_index_identically() {
    let specs = [
        EntrySpec::dir("dir"),
        EntrySpec::file("dir/a.txt", b"hello"),
        EntrySpec::symlink("link", b"dir/a.txt"),
    ];
    let dir = tempfile::tempdir().expect("tempdir");
    let mut seen: Option<Vec<(String, bool, bool, u64)>> = None;
    for (v, tag) in [
        (V::Newc, "newc"),
        (V::Crc, "crc"),
        (V::Odc, "odc"),
        (V::BinLe, "binle"),
        (V::BinBe, "binbe"),
    ] {
        let path = dir.path().join(format!("{tag}.cpio"));
        std::fs::write(&path, cpio::build(v, &specs)).expect("write");
        let r = reader_for(&path);
        let got: Vec<(String, bool, bool, u64)> = r
            .entries()
            .expect("entries")
            .into_iter()
            .map(|e| {
                (
                    e.name.as_str().to_string(),
                    e.is_dir,
                    e.is_symlink,
                    e.size_raw,
                )
            })
            .collect();
        match &seen {
            None => seen = Some(got),
            Some(want) => assert_eq!(&got, want, "{tag} 变体索引结果应与 newc 一致"),
        }
    }
    let want = seen.expect("至少跑过一个变体");
    assert_eq!(want.len(), 3);
    assert_eq!(want[0].0, "dir");
    assert!(
        !want[1].1 && want[2].2,
        "文件与链接的 is_dir/is_symlink 标志"
    );
}
