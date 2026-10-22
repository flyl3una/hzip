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
        chain: None,
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
    d.arm_external_edit(
        "a.txt",
        &temp,
        WriteBack::Enabled {
            archive,
            entry: 0,
            chain: None,
        },
    );
    // 第一次保存：出动作 + 回填在飞（用一个人为 id——任务行不存在 ⇒ reconcile 视为已结束）
    //
    // **每次写入都刻意换长度**：去重口径是 `(mtime_ns, len)`，而 Windows 的文件时间戳
    // 取自系统时钟（约 15.6 ms 一跳）——同长度、同 tick 的两次写入在盘上**无法区分**
    // （模块文档已记该边界）。真机并行跑测试时这条真的红过：`b"v1"`→`b"v2"` 都是 2 字节，
    // 撞上同一 tick 就被当「没变」⇒ 本用例靠长度区分，不靠时钟。
    std::fs::write(&temp, b"v2-first").unwrap();
    let s2 = stat_of(&temp).unwrap();
    assert!(d.on_external_save(&temp, s2).is_some());
    d.note_external_edit_submitted(&temp, s2, 777);

    // 在飞期间又保存：不出动作（记 pending）——长度同样要变（同上理由）
    std::fs::write(&temp, b"v3-longer-inflight").unwrap();
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
        WriteBack::Enabled {
            archive: a,
            entry,
            chain,
        } => {
            assert!(chain.is_none(), "顶层单档会话不带写回链");
            assert_eq!(a, archive);
            assert_eq!(entry, 1);
        }
        other => panic!("顶层 zip 会话应可写回：{other:?}"),
    }

    // ③ 嵌套会话（T-IMG0）：**不再一律拒绝**——链能重建外层就写回（带链），
    //    链建不起来才按理由拒绝（本例夹具：链状态装配不出可写链）。
    d.begin_nested_session(None, "inner.zip");
    match crate::ui::shell::external_edits::write_back_target(&d, &archive, 1) {
        WriteBack::Disabled { reason } => assert!(
            reason.starts_with("edit.writeback.") || reason.starts_with("vol."),
            "拒绝必须带可展示的理由键，实得 {reason}"
        ),
        WriteBack::Enabled { chain, .. } => {
            assert!(chain.is_some(), "嵌套会话写回必须带链（外层逐层冒泡）");
        }
    }
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
            chain: None,
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
    let (notice, submitted_job) =
        crate::ui::shell::external_edits::apply_save_action(&data, Some(action));
    assert!(notice.is_none(), "提交成功不该产生提示：{notice:?}");
    assert!(
        submitted_job.is_some(),
        "提交成功要回带 job id（开进度窗用）"
    );
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
        WriteBack::Enabled {
            archive, entry: e, ..
        } => {
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

            chain: None,
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
    let (notice, _job) = crate::ui::shell::external_edits::apply_save_action(&data, Some(action));
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

/// **多分区镜像**的外部编辑器写回：变更落到**条目所在的分区**，隔壁分区一个字节不动。
///
/// 用户 2026-10-16 实报（原文）：「使用外部编辑器编辑后保存失败，直接在文件浏览界面
/// 重命名/删除这些也不行」——弹窗原文是「该镜像含多个卷（分区），暂不支持在其内部编辑」。
/// 根因在多分区镜像（EFI + root 是常态）此前一律拒写；`hzip-vol` 的写侧已改为按分区归集
/// （见 `hzip-vol/tests/editor.rs`），本用例把 **GUI 这一端**也走完一遍：判定可写回 →
/// 提交 `EditBatch + Replace{entry}` → 条目内容真的变了、另一分区原样。
#[test]
fn save_back_inside_a_two_partition_image_hits_the_right_partition() {
    use hzip_format_testkit::vol::{FixtureFs, mbr_only, write_fs_into};

    // 两分区 MBR 盘：各 1 MiB 对齐的 8 MiB ext4
    let fs0 = FixtureFs::Ext4.image(&[("/p1.txt", b"one" as &[u8])]);
    let fs1 = FixtureFs::Ext4.image(&[("/p2.txt", b"two" as &[u8])]);
    const SECTOR: u64 = 512;
    let start0 = 1u64 << 20;
    let start1 = start0 + (fs0.len() as u64).next_multiple_of(1 << 20);
    let mut disk = mbr_only(
        &[
            (
                0x83,
                (start0 / SECTOR) as u32,
                (fs0.len() as u64 / SECTOR) as u32,
            ),
            (
                0x83,
                (start1 / SECTOR) as u32,
                (fs1.len() as u64 / SECTOR) as u32,
            ),
        ],
        start1 + fs1.len() as u64,
    );
    write_fs_into(&mut disk, start0, FixtureFs::Ext4, &[("/p1.txt", b"one")]);
    write_fs_into(&mut disk, start1, FixtureFs::Ext4, &[("/p2.txt", b"two")]);

    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("two-disks.img");
    std::fs::write(&image, &disk).unwrap();

    let mut d = app();
    d.main.open(&d.engine, &image).expect("打开两分区镜像");
    d.archive = Some(image.clone());
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    d.signal_tx = Some(std::sync::Arc::new(tx));

    // 目标条目属于**分区 2**（读侧投影名带分区行前缀）
    let row = d
        .main
        .entries()
        .iter()
        .find(|r| r.name == "Partition 2/p2.txt")
        .map(|r| (r.id, r.name.clone()))
        .expect("应能列出分区 2 的条目行");
    let (entry, name) = row;
    match crate::ui::shell::external_edits::write_back_target(&d, &image, entry) {
        WriteBack::Enabled {
            archive, entry: e, ..
        } => {
            assert_eq!(archive, image);
            assert_eq!(e, entry, "写回目标 = 界面点的那一条");
        }
        other => panic!("两分区镜像的顶层会话应可写回：{other:?}"),
    }

    let temp = dir.path().join("materialized-p2.txt");
    // 物化时的内容（= 镜像里那条）——后面写入刻意换长度：去重口径是 `(mtime_ns, len)`，
    // 而 Windows 文件时间戳约 15.6 ms 一跳，等长写入可能撞同一 tick 被判「没变」
    std::fs::write(&temp, b"two").unwrap();
    d.arm_external_edit(
        &name,
        &temp,
        WriteBack::Enabled {
            archive: image.clone(),
            entry,

            chain: None,
        },
    );

    let data = std::sync::Arc::new(std::sync::Mutex::new(d));
    std::fs::write(&temp, b"TWO-changed").unwrap(); // 换长度：去重靠 (mtime,len)，不等时钟
    let stat = stat_of(&temp).unwrap();
    let action = data
        .lock()
        .unwrap()
        .on_external_save(&temp, stat)
        .expect("应出提交动作");
    let (notice, _job) = crate::ui::shell::external_edits::apply_save_action(&data, Some(action));
    assert!(notice.is_none(), "提交应成功：{notice:?}");
    let job = {
        let d = data.lock().unwrap();
        d.external_edit_job(&temp).expect("已在飞")
    };
    wait_job(&data, job);

    // 复核：目标分区的条目变了；另一分区原样
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
            .read_entry(pick("Partition 2/p2.txt"), hzip_format::PasswordRef::none())
            .expect("读分区 2 的条目"),
        &mut buf,
    )
    .unwrap();
    assert_eq!(buf, b"TWO-changed", "分区 2 的条目内容应为改后版本");
    let mut keep = Vec::new();
    std::io::Read::read_to_end(
        &mut browser
            .read_entry(pick("Partition 1/p1.txt"), hzip_format::PasswordRef::none())
            .expect("读分区 1 的条目"),
        &mut keep,
    )
    .unwrap();
    assert_eq!(keep, b"one", "分区 1 的条目必须原样（不得被误写）");
}

/// 手动写回模式（设置-通用「改动后立即写回」= 关）：外部编辑器保存**不提交任务**，
/// 而是挂起——`applied` 照常回填（同一内容不重复入批），批次里留一条 Replace，
/// 预览取到的也是这份新内容。
#[test]
fn manual_mode_stages_the_save_instead_of_submitting() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("doc.zip");
    write_zip(&archive, &[("a.txt", b"old")]);
    let temp = dir.path().join("a.txt");
    std::fs::write(&temp, b"old").unwrap();

    let mut d = app();
    d.settings.write_back_immediate = false;
    d.main.open(&d.engine, &archive).expect("open zip");
    d.archive = Some(archive.clone());
    let entry = d
        .main
        .entries()
        .iter()
        .find(|e| e.name == "a.txt")
        .map(|e| e.id)
        .expect("a.txt 行");
    d.arm_external_edit(
        "a.txt",
        &temp,
        WriteBack::Enabled {
            archive: archive.clone(),
            entry,
            chain: None,
        },
    );

    let data = std::sync::Arc::new(std::sync::Mutex::new(d));
    std::fs::write(&temp, b"NEW").unwrap();
    let stat = stat_of(&temp).unwrap();
    let action = data
        .lock()
        .unwrap()
        .on_external_save(&temp, stat)
        .expect("应出提交动作");
    let (notice, job) = crate::ui::shell::external_edits::apply_save_action(&data, Some(action));
    assert!(notice.is_some(), "挂起要有一条提示");
    assert!(job.is_none(), "手动模式不得提交任务");
    let d = data.lock().unwrap();
    assert_eq!(d.pending_count(), 1, "改动应挂起一条");
    assert!(
        d.pending_preview(entry).is_some(),
        "挂起后预览必须取到新内容"
    );
    assert!(d.external_edit_job(&temp).is_none(), "不得有在飞任务");
}

/// 手动保存**全链路**：挂起 → flush（与点「保存」同一条路）→ 一个写回任务 →
/// **外层档案里的内层档案真的变了**。
///
/// 用户实测 2026-10-19：点保存提示成功，但重开 `qcow2.zip` 里的影像还是旧的。
/// 这里用「zip 里套 zip」等价复现同一条链路（EntryReplace + 写回链 + 原子替换）。
#[test]
fn manual_flush_writes_the_nested_container_back_into_the_outer_archive() {
    use crate::ui::contracts::{SignalRx, SignalTx};
    let dir = tempfile::tempdir().unwrap();
    // 内层 zip（被包住的「容器」）：a.txt = old
    let inner_src = dir.path().join("inner_src.zip");
    write_zip(&inner_src, &[("a.txt", b"old")]);
    let inner_bytes = std::fs::read(&inner_src).unwrap();
    // 外层 zip：inner.zip + keep.txt
    let outer = dir.path().join("outer.zip");
    write_zip(
        &outer,
        &[("inner.zip", &inner_bytes), ("keep.txt", b"keep")],
    );

    let mut d = app();
    d.settings.write_back_immediate = false; // 手动写回
    let (tx, _rx): (SignalTx, SignalRx) = smol::channel::bounded(8);
    d.signal_tx = Some(std::sync::Arc::new(tx));
    d.main.open(&d.engine, &outer).expect("open outer");
    d.archive = Some(outer.clone());
    let inner_entry = d
        .main
        .entries()
        .iter()
        .find(|e| e.name == "inner.zip")
        .map(|e| e.id)
        .expect("inner.zip 行");

    // 会话源 = 内层 zip 的物化档；变更 = 内层 a.txt（内层投影第 0 条）→ NEW
    let inner_temp = dir.path().join("materialized-inner.zip");
    std::fs::write(&inner_temp, &inner_bytes).unwrap();
    let new_content = dir.path().join("a-new.txt");
    std::fs::write(&new_content, b"NEW").unwrap();
    let mut spec =
        hzip_core::jobs::JobSpec::new(hzip_core::jobs::JobKind::EditBatch, inner_temp.clone());
    spec.changes = Some(hzip_format::ChangeSet {
        changes: vec![hzip_format::Change::Replace {
            entry: 0,
            src: new_content.clone(),
        }],
    });
    spec.write_back = Some(hzip_core::services::WriteBackChain {
        outer: outer.clone(),
        layers: vec![hzip_core::services::WriteBackLayer::EntryReplace {
            entry: inner_entry,
            entry_name: "inner.zip".into(),
        }],
    });

    assert_eq!(d.stage_pending_from_spec(&spec).unwrap(), 1);
    let job = d.flush_pending().unwrap().expect("应提交写回任务");
    let data = std::sync::Arc::new(std::sync::Mutex::new(d));
    wait_job(&data, job);

    // 外层档案里的 inner.zip 必须含 a.txt = NEW
    let d = data.lock().unwrap();
    let b = d
        .engine
        .browse(
            hzip_format::Source::File(outer.clone()),
            &Default::default(),
        )
        .unwrap();
    let infos = b.entries();
    let ix = infos
        .iter()
        .position(|e| e.name.as_str() == "inner.zip")
        .expect("外层仍有 inner.zip");
    let mut buf = Vec::new();
    b.read_entry(infos[ix].id, hzip_format::PasswordRef::none())
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    let inner_after = dir.path().join("inner-after.zip");
    std::fs::write(&inner_after, &buf).unwrap();
    let ib = d
        .engine
        .browse(hzip_format::Source::File(inner_after), &Default::default())
        .unwrap();
    let iinfos = ib.entries();
    let ax = iinfos
        .iter()
        .position(|e| e.name.as_str() == "a.txt")
        .expect("内层仍有 a.txt");
    let mut abuf = Vec::new();
    ib.read_entry(iinfos[ax].id, hzip_format::PasswordRef::none())
        .unwrap()
        .read_to_end(&mut abuf)
        .unwrap();
    assert_eq!(
        abuf, b"NEW",
        "外层档案里的内层档案必须被写回（用户实测：重开后被还原）"
    );
}
