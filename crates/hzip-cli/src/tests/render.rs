use super::*;

#[test]
fn human_bytes_units() {
    assert_eq!(human_bytes(0), "0 B");
    assert_eq!(human_bytes(1023), "1023 B");
    assert_eq!(human_bytes(1024), "1.0 KiB");
    assert_eq!(human_bytes(1536), "1.5 KiB");
    assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
}

#[test]
fn progress_line_schema_fields() {
    let ev = ProgressEvent::JobStarted {
        job_id: 1,
        total_bytes: 100,
        total_entries: 2,
    };
    let line = ndjson_line(1, &ev).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed["event"], "started");
    assert_eq!(parsed["job_id"], 1);
    assert_eq!(parsed["total_entries"], 2);
    assert!(parsed["ts"].as_str().unwrap().contains('T'));
}
