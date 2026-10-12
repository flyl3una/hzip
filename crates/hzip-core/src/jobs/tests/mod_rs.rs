use super::*;
use std::collections::HashSet;
use std::path::PathBuf;

fn key(s: &str) -> Vec<String> {
    vec![s.to_string()]
}

fn busy_of(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn same_key_waits_while_other_key_runs() {
    let queue = vec![key("a.zip"), key("b.zip")];
    // a.zip 在跑 → 跳过它，派发 b.zip（异文件并行）
    assert_eq!(pick_dispatchable(&queue, &busy_of(&["a.zip"])), Some(1));
    // 没人占 → 取队头（保序）
    assert_eq!(pick_dispatchable(&queue, &HashSet::new()), Some(0));
    // 全被占 → 本轮不派发
    assert_eq!(
        pick_dispatchable(&queue, &busy_of(&["a.zip", "b.zip"])),
        None
    );
}

#[test]
fn keys_cover_source_and_out_normalized() {
    let mut spec = JobSpec::new(JobKind::Compress, PathBuf::from("A.TXT"));
    spec.out = Some(PathBuf::from("B.ZIP"));
    let keys = resource_keys(&spec);
    assert_eq!(keys.len(), 2, "源与输出各占一键：{keys:?}");
    assert!(
        keys.iter().all(|k| *k == k.to_ascii_lowercase()),
        "键须小写归一（Windows 大小写不敏感）：{keys:?}"
    );
}

#[test]
fn job_spec_debug_redacts_password() {
    let mut spec = JobSpec::new(JobKind::EditBatch, PathBuf::from("secret.zip"));
    let none = format!("{spec:?}");
    assert!(none.contains("password: None"), "{none}");
    spec.password = Some(zeroize::Zeroizing::new("hunter2".to_string()));
    let some = format!("{spec:?}");
    assert!(
        some.contains("password: Some(<redacted>)"),
        "Debug 须脱敏：{some}"
    );
    assert!(!some.contains("hunter2"), "明文密码不得进 Debug：{some}");
}
