//! RenameAuto 并发安全 proptest（TC-CORE-004 三属性；02 §9.8 幂等）。
//!
//! 属性（任务卡 §4.4）：
//! ① `rename_distinct`：并发 2×N 线程各自 `allocate` → 分配名互异
//!    （共享 in-flight 台账原子分配，不撞既有文件含大小写折叠）；
//! ② `rename_idempotent`：串行重复分配（新台账）恒等；
//! ③ `format_canonical`：名匹配 `{stem} ({n}){ext}` 且 n 单调。
//!
//! 并发安全作用域 = 单任务内（in-flight 台账）；跨任务不承诺（rustdoc 见
//! 父模块文档）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use proptest::prelude::*;

use crate::conflict::{AllocatedNames, insert_suffix};

/// 路径生成器：tmp 下任意合法文件名字符串。
fn filename_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z]{1,6}",                           // 无扩展名
        "[a-z]{1,6}\\.[a-z]{1,3}",              // 带扩展名
        "\\.[a-z]{1,5}",                        // 点文件
        "[a-z]{1,4}\\.[a-z]{1,3}\\.[a-z]{1,2}", // 多后缀
    ]
}

// 属性 ①：并发 2×N 线程分配 → 名互异（共享 in-flight 台账原子分配）。
proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn rename_distinct(
        base_name in filename_strategy(),
        n in 1..8usize,
    ) {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let base = tmp.path().join(&base_name);
        // 既有文件：a.txt 本体占位（逼分配从 (2) 起）
        std::fs::write(&base, b"x").expect("写既有");
        let allocated = Arc::new(AllocatedNames::new());
        let threads = n * 2;
        let barrier = Arc::new(Barrier::new(threads));
        let results: Vec<std::thread::JoinHandle<PathBuf>> = (0..threads)
            .map(|_| {
                let base = base.clone();
                let allocated = Arc::clone(&allocated);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    // 原子分配：探测 + 台账登记同一临界区 → 零重名
                    allocated.allocate(&base, |p| p.exists())
                })
            })
            .collect();
        let mut names: Vec<PathBuf> = results
            .into_iter()
            .map(|h| h.join().expect("线程"))
            .collect();
        names.sort();
        let before = names.len();
        names.dedup();
        prop_assert_eq!(names.len(), before, "并发分配出现重名");
        // 全部分配名不撞既有文件
        for name in &names {
            prop_assert!(!name.exists(), "分配名撞既有文件: {}", name.display());
            prop_assert!(
                name != &base && !name.to_string_lossy().eq_ignore_ascii_case(&base_name),
                "分配名撞既有 base: {}",
                name.display()
            );
        }
    }
}

/// 属性 ②：串行重复分配（新台账）恒等。
#[test]
fn rename_idempotent_prop() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let base = tmp.path().join("a.txt");
    std::fs::write(tmp.path().join("a (2).txt"), b"x").expect("写 a (2)");
    let r1 = AllocatedNames::new().allocate(&base, |p| p.exists());
    let r2 = AllocatedNames::new().allocate(&base, |p| p.exists());
    assert_eq!(r1, r2, "串行重复分配应恒等");
    assert_eq!(r1, tmp.path().join("a (3).txt"));
}

// 属性 ③：命名格式规范（`{stem} ({n}){ext}` 且 n 单调）——proptest 全空间。
proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn format_canonical(
        name in filename_strategy(),
        n in 2u32..10_000,
    ) {
        let p = Path::new(&name);
        let cand = insert_suffix(p, n);
        let cand_s = cand.to_string_lossy().into_owned();
        // 形态：{stem} ({n}){ext}
        let expect_n = format!(" ({n})");
        prop_assert!(cand_s.contains(&expect_n), "候选 {} 缺 n={n}", cand_s);
        // 无扩展名与点文件：... (n) 结尾；带扩展名：... (n).ext 结尾
        let before: String = name
            .rsplit_once('.')
            .filter(|(s, _)| !s.is_empty() && !s.starts_with('.'))
            .map(|(s, _)| s.to_string())
            .unwrap_or_else(|| name.clone());
        if name.starts_with('.') {
            prop_assert!(!cand_s.ends_with(&format!(".{name}")), "点文件扩展名处理");
        } else {
            prop_assert!(cand_s.starts_with(&before), "stem 前缀保留: {} vs {}", cand_s, before);
        }
        // n 单调：insert_suffix(n) != insert_suffix(n+1) 且后续更大
        let cand2 = insert_suffix(p, n + 1);
        prop_assert_ne!(&cand, &cand2);
        let cand3 = insert_suffix(p, n + 2);
        prop_assert_ne!(&cand2, &cand3);
    }
}
