//! 外部编辑写回（`app::external_edit`）的测试。
//!
//! 用户实测缺陷 2026-10-16 的回归位：打开卷内文件用系统编辑器改完保存，**什么都没
//! 发生**。这里守「保存 → 决定 → 提交 `EditBatch + Replace{entry}` → 档案内容变了」
//! 这条链，以及三类「不写回」的边界（嵌套 / 不支持编辑 / 提示只出一次）。

use super::*;
use crate::app::external_edit::{SaveAction, WriteBack, stat_of};
use crate::viewmodel::testutil::{engine, write_zip};

fn app() -> AppData {
    AppData::new(std::sync::Arc::new(engine()))
}

/// 等任务收尾（真实引擎跑在别的线程；测试轮询任务行）。
fn wait_job(data: &std::sync::Arc<std::sync::Mutex<AppData>>, job_id: u64) {
    for _ in 0..400 {
        let done = {
            let Ok(d) = data.lock() else { return };
            let Ok(mut j) = d.jobs.lock() else { return };
            j.sync_states_from_core();
            j.row(job_id).is_none_or(|r| r.finished.is_some())
        };
        if done {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("任务 {job_id} 未在 10s 内收尾");
}

/// 内容没变 → 不提交；变了 → 出 `EditBatch + Replace{entry, src=物化文件}`。
///
/// 第一条尤其重要：**物化那一次写入**会产生迟到的 change 事件，若被当成「用户
/// 保存了」，打开任何文件都会立刻触发一次无意义的编辑任务。
#[test]
fn save_decision_dedupes_and_builds_the_replace_spec() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("doc.zip");
    let temp = dir.path().join("a.txt");
    std::fs::write(&temp, b"old").unwrap();

    let mut d = app();
    let target = WriteBack::Enabled {
        archive: archive.clone(),
        entry: 3,
    };
    d.arm_external_edit("a.txt", &temp, target);

    // ① 登记时的状态（= 物化刚写完）→ 无需动作
    let unchanged = stat_of(&temp).unwrap();
    assert!(
        d.on_external_save(&temp, unchanged).is_none(),
        "物化那一次的迟到事件不该触发写回"
    );

    // ② 内容变了 → 出规格
    std::fs::write(&temp, b"NEW-CONTENT").unwrap();
    let changed = stat_of(&temp).unwrap();
    assert_ne!(changed, unchanged, "mtime/len 应已变（夹具前提）");
    let Some(SaveAction::Submit {
        spec,
        stat,
        temp: t,
    }) = d.on_external_save(&temp, changed)
    else {
        panic!("改变内容后应出提交动作");
    };
    assert_eq!(stat, changed);
    assert_eq!(t, temp);
    assert_eq!(spec.kind, hzip_core::jobs::JobKind::EditBatch);
    assert_eq!(spec.source, archive, "写回目标是登记的那个档案");
    let changes = spec.changes.expect("变更集");
    assert_eq!(changes.changes.len(), 1);
    match &changes.changes[0] {
        hzip_format::Change::Replace { entry, src } => {
            assert_eq!(*entry, 3, "命中的是界面点的那一条（读侧投影 id）");
            assert_eq!(src, &temp);
        }
        other => panic!("应为 Replace，实得 {other:?}"),
    }

    // ③ 提交回填后，同一状态不再重复提交
    d.note_external_edit_submitted(&temp, changed, 42);
    assert!(d.on_external_save(&temp, changed).is_none(), "同状态去重");
    assert_eq!(d.external_edit_job(&temp), Some(42));
}

/// 在飞期间的保存被记下；任务结束后 `reconcile` 补提交（那次 change 事件已经丢了）。
#[test]
fn save_during_inflight_is_replayed_after_the_job_ends() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("doc.zip");
    let temp = dir.path().join("a.txt");
    std::fs::write(&temp, b"v1").unwrap();

    let mut d = app();
    d.arm_external_edit("a.txt", &temp, WriteBack::Enabled { archive, entry: 0 });
    // 第一次保存：出动作 + 回填在飞（用一个人为 id——任务行不存在 ⇒ reconcile 视为已结束）
    std::fs::write(&temp, b"v2").unwrap();
    let s2 = stat_of(&temp).unwrap();
    assert!(d.on_external_save(&temp, s2).is_some());
    d.note_external_edit_submitted(&temp, s2, 777);

    // 在飞期间又保存：不出动作（记 pending）
    // 内容长度刻意变化——去重口径是 (mtime, len)，同长同 tick 的写入在粗粒度
    // 文件系统上无法区分（见模块文档的边界说明）
    std::fs::write(&temp, b"v3-longer").unwrap();
    let s3 = stat_of(&temp).unwrap();
    assert!(
        d.on_external_save(&temp, s3).is_none(),
        "在飞期间只记账，不并发提交（同一档案两个编辑任务会互相踩）"
    );

    // 任务结束（行不存在 = 已结束）→ reconcile 补提交，且规格带回最新内容
    let actions = d.reconcile_external_edits();
    assert_eq!(actions.len(), 1, "在飞期间那次保存要补上");
    let Some(SaveAction::Submit { stat, .. }) = actions.into_iter().next() else {
        panic!("补提交应出规格");
    };
    assert_eq!(stat, s3, "补的是最新那版内容");
    assert!(
        d.external_edit_job(&temp).is_none(),
        "补提交后清位（等回填）"
    );
}

/// 「不可写回」：保存时如实告知用户，且**同一会话只提示一次**（反复保存不刷屏）。
#[test]
fn disabled_target_refuses_once_and_never_submits() {
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("nested.txt");
    std::fs::write(&temp, b"x").unwrap();

    let mut d = app();
    d.arm_external_edit(
        "nested.txt",
        &temp,
        WriteBack::Disabled {
            reason: "edit.writeback.nested",
        },
    );
    std::fs::write(&temp, b"y").unwrap();
    let stat = stat_of(&temp).unwrap();
    match d.on_external_save(&temp, stat) {
        Some(SaveAction::Refused { reason, name }) => {
            assert_eq!(reason, "edit.writeback.nested");
            assert_eq!(name, "nested.txt");
        }
        other => panic!("嵌套会话保存应「拒绝写回」而不是提交：{other:?}"),
    }
    assert!(d.on_external_save(&temp, stat).is_none(), "只提示一次");
    assert!(d.external_edit_job(&temp).is_none(), "绝不提交任务");
}

/// 写回目标判定：嵌套会话 / 不支持编辑的格式 ⇒ 拒绝；顶层可编辑档案 ⇒ 写回。
#[test]
fn write_back_target_follows_session_shape() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("doc.zip");
    write_zip(&archive, &[("a.txt", b"old")]);

    // ① 没打开任何档案（caps_edit 假）⇒ 不支持编辑
    let d = app();
    assert!(matches!(
        crate::ui::shell::external_edits::write_back_target(&d, &archive, 0),
        WriteBack::Disabled {
            reason: "edit.writeback.unsupported"
        }
    ));

    // ② 顶层 zip 会话 ⇒ 可写回（zip 的 caps.edit = true）
    let mut d = app();
    d.main.open(&d.engine, &archive).expect("open zip");
    d.archive = Some(archive.clone());
    match crate::ui::shell::external_edits::write_back_target(&d, &archive, 1) {
        WriteBack::Enabled { archive: a, entry } => {
            assert_eq!(a, archive);
            assert_eq!(entry, 1);
        }
        other => panic!("顶层 zip 会话应可写回：{other:?}"),
    }

    // ③ 进了内层容器（嵌套会话）⇒ 拒绝（临时文件属于内层条目空间，写外层会写错东西）
    d.open_stack.push(crate::app::state::OpenFrame {
        crumbs: Vec::new(),
        container_path: "inner.zip".into(),
        vm: crate::viewmodel::MainViewModel::new(),
        temp: None,
    });
    assert!(matches!(
        crate::ui::shell::external_edits::write_back_target(&d, &archive, 1),
        WriteBack::Disabled {
            reason: "edit.writeback.nested"
        }
    ));
}

/// 端到端：保存 → 提交 `EditBatch` → **档案里条目内容真的变了**（其余条目原样）。
#[test]
fn save_back_rewrites_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("doc.zip");
    write_zip(&archive, &[("a.txt", &b"old"[..]), ("b.txt", &b"keep"[..])]);

    let mut d = app();
    d.main.open(&d.engine, &archive).expect("open zip");
    d.archive = Some(archive.clone());
    // 提交出口需要信号通道（任务泵回投用）；测试塞一个丢弃端
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    d.signal_tx = Some(std::sync::Arc::new(tx));
    let temp = dir.path().join("materialized-a.txt");
    std::fs::write(&temp, b"old").unwrap();
    d.arm_external_edit(
        "a.txt",
        &temp,
        WriteBack::Enabled {
            archive: archive.clone(),
            entry: 0,
        },
    );

    let data = std::sync::Arc::new(std::sync::Mutex::new(d));
    // 用户改动保存 → 走与 UI 同一条投递链
    std::fs::write(&temp, b"NEW-CONTENT").unwrap();
    let stat = stat_of(&temp).unwrap();
    let action = data
        .lock()
        .unwrap()
        .on_external_save(&temp, stat)
        .expect("应出提交动作");
    let notice = crate::ui::shell::external_edits::apply_save_action(&data, Some(action));
    assert!(notice.is_none(), "提交成功不该产生提示：{notice:?}");
    let job = {
        let d = data.lock().unwrap();
        d.external_edit_job(&temp).expect("已在飞")
    };
    wait_job(&data, job);

    // 档案字节已变 + 条目内容 == 改后版本 + 未改动条目原样
    let browser = data
        .lock()
        .unwrap()
        .engine
        .browse(
            hzip_format::Source::File(archive.clone()),
            &Default::default(),
        )
        .expect("重开档案");
    let entries = browser.entries();
    assert_eq!(entries.len(), 2, "条目数不变");
    let mut buf = Vec::new();
    std::io::Read::read_to_end(
        &mut browser
            .read_entry(0, hzip_format::PasswordRef::none())
            .expect("读 a.txt"),
        &mut buf,
    )
    .unwrap();
    assert_eq!(buf, b"NEW-CONTENT", "写回后的内容");
    let mut keep = Vec::new();
    std::io::Read::read_to_end(
        &mut browser
            .read_entry(1, hzip_format::PasswordRef::none())
            .expect("读 b.txt"),
        &mut keep,
    )
    .unwrap();
    assert_eq!(keep, b"keep", "未改动条目原样保留");
}

/// 端到端（**用户的场景**）：打开磁盘镜像 → 分区 → 改卷内文件 → 保存写回。
///
/// 走的是 GUI 的判定（`write_back_target`）+ 投递链（`apply_save_action`）+ core 的
/// **就地写**路径选择；断言到镜像里那一条内容真的变了、其余条目原样。
///
/// 用户实测的那份是 `.qcow2.gz`（组合链 ⇒ 进的是**嵌套**会话 ⇒ 按设计不写回、保存时
/// 如实告知）；本用例覆盖的是||**未压缩**顶层镜像||——那是本期真正支持的那一段。
#[test]
fn save_back_inside_a_volume_writes_in_place() {
    use hzip_format_testkit::vol::{FixtureFs, raw_with_mbr};

    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.img");
    std::fs::write(
        &image,
        raw_with_mbr(
            FixtureFs::Ext4,
            &[("/a.txt", &b"old"[..]), ("/b.txt", &b"keep"[..])],
        ),
    )
    .unwrap();

    let mut d = app();
    d.main.open(&d.engine, &image).expect("open raw image");
    d.archive = Some(image.clone());
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    d.signal_tx = Some(std::sync::Arc::new(tx));

    // 卷内条目行：读侧投影的名字带分区前缀（`Partition 1/...`）
    let row = d
        .main
        .entries()
        .iter()
        .find(|r| r.name == "Partition 1/a.txt")
        .map(|r| (r.id, r.name.clone()))
        .expect("应能列出卷内条目行");
    let (entry, name) = row;

    // 判定：顶层卷会话 ⇒ 可写回，且目标就是这一条
    match crate::ui::shell::external_edits::write_back_target(&d, &image, entry) {
        WriteBack::Enabled { archive, entry: e } => {
            assert_eq!(archive, image);
            assert_eq!(e, entry);
        }
        other => panic!("卷容器顶层会话应可写回：{other:?}"),
    }

    let temp = dir.path().join("materialized-a.txt");
    std::fs::write(&temp, b"old").unwrap();
    d.arm_external_edit(
        &name,
        &temp,
        WriteBack::Enabled {
            archive: image.clone(),
            entry,
        },
    );

    let data = std::sync::Arc::new(std::sync::Mutex::new(d));
    std::fs::write(&temp, b"NEW-CONTENT").unwrap();
    let stat = stat_of(&temp).unwrap();
    let action = data
        .lock()
        .unwrap()
        .on_external_save(&temp, stat)
        .expect("应出提交动作");
    let notice = crate::ui::shell::external_edits::apply_save_action(&data, Some(action));
    assert!(notice.is_none(), "提交应成功：{notice:?}");
    let job = {
        let d = data.lock().unwrap();
        d.external_edit_job(&temp).expect("已在飞")
    };
    wait_job(&data, job);

    // 重开镜像：卷内条目内容 == 改后版本；未改动条目原样
    let browser = data
        .lock()
        .unwrap()
        .engine
        .browse(
            hzip_format::Source::File(image.clone()),
            &Default::default(),
        )
        .expect("重开镜像");
    let entries: Vec<(usize, String)> = browser
        .entries()
        .iter()
        .map(|e| (e.id, e.name.as_str().to_string()))
        .collect();
    let pick = |want: &str| {
        entries
            .iter()
            .find(|(_, n)| n == want)
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("重开后应有 {want}：{entries:?}"))
    };
    let mut buf = Vec::new();
    std::io::Read::read_to_end(
        &mut browser
            .read_entry(pick("Partition 1/a.txt"), hzip_format::PasswordRef::none())
            .expect("读改后条目"),
        &mut buf,
    )
    .unwrap();
    assert_eq!(buf, b"NEW-CONTENT", "卷内条目内容应为改后版本");
    let mut keep = Vec::new();
    std::io::Read::read_to_end(
        &mut browser
            .read_entry(pick("Partition 1/b.txt"), hzip_format::PasswordRef::none())
            .expect("读未改动条目"),
        &mut keep,
    )
    .unwrap();
    assert_eq!(keep, b"keep", "未改动条目原样");
}
