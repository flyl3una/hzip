use super::*;

/// schema 往返：list 结果可反序列化且字段全（TC-CLI-003 锁定）。
#[test]
fn list_result_roundtrip() {
    let v = ListResultJson {
        format: "zip".into(),
        entry_count: 1,
        entries: vec![EntryJson {
            id: 0,
            name: "dir/a.txt".into(),
            is_dir: false,
            size_raw: 3,
            size_packed: 5,
            mtime: Some("2026-01-01T00:00:00Z".into()),
            encrypted: false,
            crc32: Some("d87f7e0c".into()),
            name_encoding: "utf-8".into(),
        }],
        truncated: false,
    };
    let s = serde_json::to_string(&v).unwrap();
    let back: ListResultJson = serde_json::from_str(&s).unwrap();
    assert_eq!(back, v);
    assert_eq!(back.entries[0].crc32.as_deref(), Some("d87f7e0c"));
}

/// test 结果双变体往返（TC-CLI-005）。
#[test]
fn test_result_roundtrip() {
    let ok = TestResultJson::Ok {
        archive: "a.zip".into(),
        entries: 2,
        bytes: 10,
        crc_ok: 2,
    };
    let s = serde_json::to_string(&ok).unwrap();
    assert!(s.contains("\"type\":\"ok\""));
    let back: TestResultJson = serde_json::from_str(&s).unwrap();
    assert_eq!(back, ok);

    let bad = TestResultJson::Corrupted {
        archive: "a.zip".into(),
        failed: vec![FailureJson {
            entry: "x.bin".into(),
            code: 3001,
            message: "校验失败".into(),
        }],
    };
    let s2 = serde_json::to_string(&bad).unwrap();
    let back2: TestResultJson = serde_json::from_str(&s2).unwrap();
    assert_eq!(back2, bad);
}

/// NDJSON 进度行：serde flatten 变体锁定（TC-CLI-010）。
#[test]
fn progress_line_roundtrip() {
    let line = ProgressLineJson {
        ts: "2026-01-01T00:00:00Z".into(),
        job_id: 1,
        event: ProgressEventJson::Bytes { delta: 1024 },
    };
    let s = serde_json::to_string(&line).unwrap();
    let obj: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(obj["event"], "bytes");
    assert_eq!(obj["delta"], 1024);
    let back: ProgressLineJson = serde_json::from_str(&s).unwrap();
    assert_eq!(back.event, ProgressEventJson::Bytes { delta: 1024 });
}
