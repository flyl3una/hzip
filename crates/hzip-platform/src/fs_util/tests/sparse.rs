use super::*;

#[test]
fn set_sparse_mark_and_detect() {
    let dir = crate::testutil::temp_dir("fs-util-set-sparse");
    let p = dir.path().join("hole.bin");
    // 洞取 64 MiB：APFS 对**小于约 16 MiB** 的洞会在关闭文件时重新密集化
    // （实测：16 MiB 洞 → blocks≈size，16.25 MiB 起保留），而 Windows NTFS /
    // Linux ext4 对任意大小洞均保留。用大洞才能在三个平台都断言「确为稀疏」，
    // 而不是把 APFS 的分配策略误判成实现缺陷。
    const HOLE: u64 = 64 << 20;
    let mut f = std::fs::File::create(&p).unwrap();
    // 正确顺序：先标记稀疏，再写区间（write_sparse 同序）。对已存在的密集
    // 分配事后标记不会回收簇——NTFS 只对标记后新增的洞生效。
    let r = set_sparse(&f).unwrap_or(false);
    // 写入两个远端区间，中间留洞
    use std::io::{Seek, SeekFrom, Write};
    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(b"AAAA").unwrap();
    f.seek(SeekFrom::Start(HOLE)).unwrap();
    f.write_all(b"BBBB").unwrap();
    f.flush().unwrap();
    drop(f);

    // 逻辑内容不受稀疏性影响：洞区读零、两区间内容正确
    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(&bytes[..4], b"AAAA");
    assert_eq!(&bytes[HOLE as usize..HOLE as usize + 4], b"BBBB");
    assert_eq!(bytes.len(), HOLE as usize + 4);

    // 稀疏性判定在**关闭写入句柄之后**查询（生产路径亦如此：写完 drop 再自验）。
    // 句柄未关时 APFS 可能尚未回写分配状态，此时断言会因时序偶然通过。
    if r {
        let rf = std::fs::File::open(&p).unwrap();
        assert!(
            is_sparse_file(&rf).unwrap_or(false),
            "标记成功后文件应稀疏（洞 {HOLE} 字节）"
        );
    }
}
