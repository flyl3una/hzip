use super::*;

fn app() -> AppData {
    AppData::new(std::sync::Arc::new(crate::viewmodel::testutil::engine()))
}

/// 打开一个真实 zip 会话（浏览数据面就绪 + archive 路径落位）。
fn app_with_zip(path: &std::path::Path) -> AppData {
    let mut d = app();
    d.main.open(&d.engine, path).expect("open zip");
    d.archive = Some(path.to_path_buf());
    d
}

/// 行内改名/新建文件夹：校验（非法名/重名/未改名）+ 提交 EditBatch
/// （用户口径 2026-09-28）。
#[test]
fn name_edit_commit_validates_and_submits() {
    let dir = tempfile::TempDir::new().unwrap();
    let zip = dir.path().join("t.zip");
    crate::viewmodel::testutil::write_zip(&zip, &[("a.txt", b"a"), ("b.txt", b"b")]);
    let mut d = app_with_zip(&zip);
    // 提交出口需要信号通道（任务泵回投用）；测试塞一个丢弃端
    let (tx, rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    d.signal_tx = Some(std::sync::Arc::new(tx));
    d.main.tree(); // 懒构建目录树（重名判定/行集来源）
    let same_names: Vec<String> = d
        .main
        .dir_children()
        .iter()
        .map(|r| r.name.clone())
        .collect();
    assert_eq!(same_names, ["a.txt", "b.txt"], "目录行就绪");

    // ① 非法名（带分隔符/点段）→ Err 且**保持编辑态**
    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: Some("a.txt".into()),
        text: "a.txt".into(),
        session_epoch: d.session_epoch(),
    });
    assert!(d.commit_name_edit("x/y").is_err());
    assert!(d.name_edit.is_some(), "校验失败必须保持编辑态");
    assert!(d.commit_name_edit("..").is_err());
    assert!(name_is_valid_segment("x:y"), "冒号不是路径分隔符（合法）");

    // ② 重名 → Err
    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: Some("a.txt".into()),
        text: "a.txt".into(),
        session_epoch: d.session_epoch(),
    });
    assert!(d.commit_name_edit("b.txt").is_err());
    assert!(d.name_edit.is_some());

    // ③ 名字没变 → 无动作（不提交任务、退出编辑态）
    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: Some("a.txt".into()),
        text: "a.txt".into(),
        session_epoch: d.session_epoch(),
    });
    assert_eq!(d.commit_name_edit("a.txt").unwrap(), None);
    assert!(d.name_edit.is_none());
    // 任务中心不得多出任何行
    assert!(
        d.jobs.lock().unwrap().rows().is_empty(),
        "未改名不得提交任务"
    );

    // ④ 合法改名 → 提交 EditBatch（返回 job_id；变更集已消费）
    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: Some("a.txt".into()),
        text: "a.txt".into(),
        session_epoch: d.session_epoch(),
    });
    let job_id = d.commit_name_edit("c.txt").expect("提交").expect("job");
    assert!(job_id > 0);
    assert!(d.name_edit.is_none());
    assert!(d.pending_edit.is_none(), "提交即取走变更集");
    assert!(
        d.jobs
            .lock()
            .unwrap()
            .rows()
            .iter()
            .any(|r| r.kind == "edit"),
        "任务中心出现编辑行: {:?}",
        d.jobs
            .lock()
            .unwrap()
            .rows()
            .iter()
            .map(|r| r.kind)
            .collect::<Vec<_>>()
    );

    // ⑤ 新建文件夹 → MakeDir 提交
    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: None,
        text: "新建文件夹".into(),
        session_epoch: d.session_epoch(),
    });
    assert!(d.commit_name_edit("nd").expect("提交").is_some());

    // ⑥ 会话已换（代数不一致）→ 放弃，绝不落到新档案
    d.begin_session_switch(crate::app::state::SwitchTarget {
        key: "other".into(),
        label: "other.zip".into(),
    });
    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: Some("b.txt".into()),
        text: "b.txt".into(),
        session_epoch: 0,
    });
    assert_eq!(d.commit_name_edit("z.txt").unwrap(), None);
    assert!(d.name_edit.is_none());

    // ⑦ 非当前目录（防御分支）不查重名
    d.begin_name_edit(NameEdit {
        dir: "other".into(),
        from: Some("b.txt".into()),
        text: "b.txt".into(),
        session_epoch: d.session_epoch(),
    });
    assert!(d.commit_name_edit("z.txt").unwrap().is_some());
    drop(rx);
}

/// 用户口径 2026-09-29 现场缺陷回归：**加密档重命名保存失败**
/// （`IO 错误: 拒绝访问。 (os error 5)`）。
///
/// 根因：编辑任务提交时会释放浏览会话（否则 Windows 无法原子替换被打开的
/// 文件），而泵会在**编辑任务在飞期间**就把会话重开——重开即重新打开源档案
/// ⇒ 任务随后的 `atomic_replace` 必失败。此用例断言：
/// ① 任务在飞（含等密码）期间**不重开**；
/// ② 任务终局后重开生效（会话恢复）；
/// ③ 档案真的被重写了（改名落到磁盘）。
#[test]
fn encrypted_rename_keeps_session_closed_until_edit_finishes() {
    let dir = tempfile::TempDir::new().unwrap();
    let zip = dir.path().join("enc.zip");
    crate::viewmodel::testutil::write_zip_encrypted(
        &zip,
        &[("a.txt", b"secret-a"), ("b.txt", b"b")],
        "pw",
    );
    let mut d = app();
    d.main.open(&d.engine, &zip).expect("open");
    d.archive = Some(zip.clone());
    d.main.tree();
    d.dir_rows = std::sync::Arc::new(d.main.dir_children());
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    d.signal_tx = Some(std::sync::Arc::new(tx));
    assert!(d.cached_password(&zip).is_none(), "现场：尚无会话密码");

    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: Some("a.txt".into()),
        text: "a.txt".into(),
        session_epoch: d.session_epoch(),
    });
    let job = d
        .commit_name_edit("renamed.txt")
        .expect("提交")
        .expect("job");
    assert!(job > 0);
    // 任务在飞（等密码）→ 会话必须保持关闭：重开会占住源档案，替换必 os 5
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Ok(dd) = d.router.lock()
            && !matches!(
                dd.password_state(),
                crate::viewmodel::router::PasswordDialogState::Hidden
            )
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        d.browse_session_reopen_request().is_none(),
        "编辑任务在飞期间不得重开浏览会话（否则原子替换 os error 5）"
    );
    // 应答密码 → 等终局
    {
        let job_id = d.router.lock().unwrap().active_job();
        let jobs = d.jobs.lock().unwrap();
        if let Some(h) = job_id.and_then(|id| jobs.handle(id)) {
            d.router
                .lock()
                .unwrap()
                .submit_password(h, zeroize::Zeroizing::new("pw".into()))
                .expect("应答密码");
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let done = {
            let jobs = d.jobs.lock().unwrap();
            !jobs.rows().is_empty()
                && jobs.rows().iter().all(|r| {
                    matches!(
                        r.state,
                        crate::viewmodel::JobRowState::Done
                            | crate::viewmodel::JobRowState::Failed
                            | crate::viewmodel::JobRowState::PartialDone
                            | crate::viewmodel::JobRowState::Cancelled
                    )
                })
        };
        if done {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "编辑任务 20s 内未终局"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    {
        let jobs = d.jobs.lock().unwrap();
        let row = jobs.rows().first().expect("任务行");
        assert_eq!(
            row.state,
            crate::viewmodel::JobRowState::Done,
            "加密档改名必须成功（实得 {:?}: {:?}）",
            row.state,
            row.final_line.as_ref().map(|f| f.message.clone())
        );
    }
    // 终局后重开恢复
    assert_eq!(
        d.browse_session_reopen_request().as_deref(),
        Some(zip.as_path()),
        "任务终局后会话应恢复重开"
    );
    // 磁盘上真改了名
    let names: Vec<String> = {
        let engine = crate::viewmodel::testutil::engine();
        let r = engine
            .registry()
            .handler("zip")
            .unwrap()
            .open_reader(
                hzip_format::Source::File(zip.clone()),
                &hzip_format::OpenOptions::default(),
            )
            .unwrap();
        r.entries()
            .unwrap()
            .iter()
            .map(|e| e.name.as_str().to_string())
            .collect()
    };
    assert!(
        names.iter().any(|n| n == "renamed.txt"),
        "改名未落盘：{names:?}"
    );
    assert!(!names.iter().any(|n| n == "a.txt"), "旧名未清：{names:?}");
}

/// 用户口径 2026-09-29 #1：重命名/新建（重写档案 + 重开会话）后，浏览目录
/// 必须回到编辑前所在目录（此前每次都被踢回档案根）。
#[test]
fn edit_reopen_restores_current_dir() {
    let dir = tempfile::TempDir::new().unwrap();
    let zip = dir.path().join("t.zip");
    crate::viewmodel::testutil::write_zip(
        &zip,
        &[
            ("dir/a.txt", b"a"),
            ("dir/sub/b.txt", b"b"),
            ("top.txt", b"t"),
        ],
    );
    let mut d = app();
    d.main.open(&d.engine, &zip).expect("open");
    d.archive = Some(zip.clone());
    // 用户进到 `dir` 且展开树
    d.main.enter_dir("dir");
    d.main.expand_to("dir");
    d.main.tree();
    let (before_dir, before_expanded) = d.main.view_state();
    assert_eq!(before_dir, "dir");
    assert!(!before_expanded.is_empty());
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    d.signal_tx = Some(std::sync::Arc::new(tx));

    d.begin_name_edit(NameEdit {
        dir: "dir".into(),
        from: Some("a.txt".into()),
        text: "a.txt".into(),
        session_epoch: d.session_epoch(),
    });
    assert!(d.commit_name_edit("renamed.txt").expect("提交").is_some());
    // 提交后视图状态被记住（重开种回用）
    assert_eq!(
        d.reopen_view_state.as_ref().map(|(p, _)| p.as_str()),
        Some("dir"),
        "提交时必须记住编辑前目录"
    );
    // 等任务终局（未加密档：无弹窗，直接跑完）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let done = {
            let jobs = d.jobs.lock().unwrap();
            !jobs.rows().is_empty()
                && jobs.rows().iter().all(|r| {
                    matches!(
                        r.state,
                        crate::viewmodel::JobRowState::Done
                            | crate::viewmodel::JobRowState::Failed
                            | crate::viewmodel::JobRowState::PartialDone
                            | crate::viewmodel::JobRowState::Cancelled
                    )
                })
        };
        if done {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "编辑任务 20s 未终局");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        d.jobs.lock().unwrap().rows()[0].state,
        crate::viewmodel::JobRowState::Done,
        "改名任务应成功"
    );
    // 模拟保活重开（生产路径：engine.browse → install_browser → finish_browse_reopen）
    let engine = d.engine.clone();
    let browser = engine
        .browse(
            hzip_format::Source::File(zip.clone()),
            &hzip_format::OpenOptions {
                defer_composite: true,
                ..Default::default()
            },
        )
        .expect("reopen browse");
    d.main.install_browser(browser, &zip).expect("install");
    d.finish_browse_reopen(Ok(()));
    assert_eq!(
        d.main.current_dir(),
        "dir",
        "重开后必须回到编辑前目录（不被踢回档案根）"
    );
    let names: Vec<String> = d
        .main
        .dir_children()
        .iter()
        .map(|r| r.name.clone())
        .collect();
    assert!(
        names.iter().any(|n| n == "renamed.txt"),
        "重开后应看到改名结果：{names:?}"
    );
    assert!(d.reopen_view_state.is_none(), "视图状态一次性消费");
}

/// 编辑后原目录已不存在（改名的就是所在路径的祖先）→ 回落档案根，不卡在空目录。
#[test]
fn edit_reopen_falls_back_to_root_when_dir_gone() {
    let dir = tempfile::TempDir::new().unwrap();
    let zip = dir.path().join("t.zip");
    crate::viewmodel::testutil::write_zip(&zip, &[("dir/a.txt", b"a"), ("top.txt", b"t")]);
    let mut d = app();
    d.main.open(&d.engine, &zip).expect("open");
    d.archive = Some(zip.clone());
    d.main.enter_dir("dir");
    d.main.tree();
    // 直接种回一个已不存在的目录
    let (_, expanded) = d.main.view_state();
    assert!(
        !d.main.restore_view_state("gone", expanded),
        "不存在的目录应报告未命中"
    );
    assert_eq!(d.main.current_dir(), "", "回落档案根");
}

/// 不可编辑格式（tar）：提交被 caps.edit 挡下 → Err（UI 侧 toast）。
#[test]
fn name_edit_commit_rejects_non_editable_format() {
    let dir = tempfile::TempDir::new().unwrap();
    let tar = dir.path().join("t.tar");
    crate::viewmodel::testutil::write_ustar(&tar, &[("a.txt", b"a")]);
    let mut d = app();
    d.main.open(&d.engine, &tar).expect("open tar");
    d.archive = Some(tar.clone());
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(8);
    d.signal_tx = Some(std::sync::Arc::new(tx));
    assert!(!d.caps_edit(), "tar 不可编辑");
    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: Some("a.txt".into()),
        text: "a.txt".into(),
        session_epoch: d.session_epoch(),
    });
    assert!(d.commit_name_edit("b.txt").is_err(), "不可编辑格式必须拒绝");
}

// 会话密码缓存（用户口径 2026-09-28）：记住 → 命中 → 丢弃；字面路径即键
// （不做 canonicalize——缓存只服务本次会话的同一路径对象）。
#[test]
fn session_password_cache_remember_and_forget() {
    let mut d = app();
    let a = std::path::Path::new("C:/tmp/a.zip");
    let b = std::path::Path::new("C:/tmp/b.zip");
    assert!(d.cached_password(a).is_none());
    d.remember_password(a, zeroize::Zeroizing::new("pw-a".into()));
    d.remember_password(b, zeroize::Zeroizing::new("pw-b".into()));
    assert_eq!(d.cached_password(a).unwrap().as_str(), "pw-a");
    assert_eq!(d.cached_password(b).unwrap().as_str(), "pw-b");
    d.forget_password(a);
    assert!(d.cached_password(a).is_none(), "丢弃后不再命中");
    assert_eq!(d.cached_password(b).unwrap().as_str(), "pw-b", "互不影响");
}

// 门放行动作从 router 取（`take` 语义：取出即清空——泵重跑一次不重放）。
#[test]
fn pending_gate_action_is_taken_once() {
    use crate::viewmodel::GateAction;
    let d = app();
    assert!(d.take_pending_gate_action().is_none());
    assert!(d.router.lock().unwrap().arm_gate(
        GateAction::OpenSelection {
            id: 3,
            name: "secret.txt".into(),
            form: crate::viewmodel::OpenForm::Configured
        },
        std::path::PathBuf::from("C:/tmp/a.zip")
    ));
    assert!(d.router.lock().unwrap().gate_resolve());
    assert_eq!(
        d.take_pending_gate_action(),
        Some(GateAction::OpenSelection {
            id: 3,
            name: "secret.txt".into(),
            form: crate::viewmodel::OpenForm::Configured
        })
    );
    assert!(d.take_pending_gate_action().is_none());
}

/// 换会话**代数**（用户实测 2026-09-24 跨操作串扰）：
///
/// 「打开了 cpio.gz（没展开里面的 cpio），再打开 qcow2.gz，冒出来的进度窗却还在
/// 打开那个 cpio」——后台流程之间没有次序保证，旧流程可能后落地把新档案盖回去。
/// 代数让「只有最新一次落地」成为硬约束：同目标连点 → Duplicate（去重）；不同
/// 目标 → 顶替（并给出被顶替目标的展示名，供关掉旧进度窗）；旧代数落账比对必为
/// false（丢弃结果）。
#[test]
fn session_switch_epoch_supersedes_stale_flows() {
    let mut d = app();
    let a = SwitchTarget {
        key: "C:/tmp/cpio.gz".into(),
        label: "cpio.gz".into(),
    };
    let b = SwitchTarget {
        key: "C:/tmp/qcow2.gz".into(),
        label: "qcow2.gz".into(),
    };
    let SwitchStart::Started {
        epoch: e1,
        superseded,
    } = d.begin_session_switch(a.clone())
    else {
        panic!("首次发起应获准");
    };
    assert!(superseded.is_none(), "没有可顶替的旧目标");
    assert!(d.session_switch_is_current(e1));

    // 同目标连点：去重（不重复起线程）
    assert_eq!(d.begin_session_switch(a.clone()), SwitchStart::Duplicate);

    // 不同目标：顶替（并报出旧目标的展示名，供关窗）
    let SwitchStart::Started {
        epoch: e2,
        superseded,
    } = d.begin_session_switch(b.clone())
    else {
        panic!("不同目标应获准发起");
    };
    assert_ne!(e1, e2, "代数递增");
    assert_eq!(superseded.as_deref(), Some("cpio.gz"), "旧目标展示名");
    assert!(
        !d.session_switch_is_current(e1),
        "被顶替的旧流程落账比对必须失败（丢弃结果，不盖掉新档案）"
    );
    assert!(d.session_switch_is_current(e2));

    // 旧流程收尾**不得**清掉新流程的在飞目标
    d.end_session_switch(e1);
    assert_eq!(
        d.begin_session_switch(b.clone()),
        SwitchStart::Duplicate,
        "新流程仍在飞（旧流程收尾不能误清）"
    );
    // 新流程自己收尾 → 在飞清空 → 可再次发起
    d.end_session_switch(e2);
    assert!(matches!(
        d.begin_session_switch(b),
        SwitchStart::Started { .. }
    ));
}

/// 顶层打开新档案：清空嵌套会话栈 + 尽力删除各层临时文件
/// （用户口径 2026-09-24：「不应该被老压缩包内容干扰」）。
#[test]
fn reset_nested_sessions_drops_frames_and_temp_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let temp = dir.path().join("inner.tar");
    std::fs::write(&temp, b"x").unwrap();
    let mut d = app();
    d.adopt_temp_source(temp.clone());
    d.begin_nested_session("inner.tar");
    assert!(!d.open_stack.is_empty(), "已压一层");
    assert_eq!(d.open_stack[0].temp.as_deref(), Some(temp.as_path()));
    d.reset_nested_sessions();
    assert!(d.open_stack.is_empty(), "嵌套栈清空");
    assert!(d.current_temp.is_none(), "当前临时源清空");
    assert!(!temp.exists(), "临时文件尽力删除");
}

/// 会话切换闸：一次只放行一个（用户口径 2026-09-24「双击只触发一次」），
/// 闸票 `Drop` 即放行（失败/早退/panic 都不会把会话锁死）。
#[test]
fn session_switch_gate_excludes_and_releases_on_drop() {
    let gate = SessionSwitchGate::default();
    assert!(!gate.is_busy(), "初始空闲");
    let first = gate.try_acquire().expect("首次取闸");
    assert!(gate.is_busy(), "持票期间在飞");
    assert!(
        gate.try_acquire().is_none(),
        "第二次取闸必须被拒（连点只触发一次）"
    );
    drop(first);
    assert!(!gate.is_busy(), "Drop 即放行");
    assert!(gate.try_acquire().is_some(), "放行后可再次取闸");
    // 克隆共享同一闸（AppData 里是同一份；克隆出去也互斥）
    let gate2 = gate.clone();
    let held = gate.try_acquire().expect("再取");
    assert!(gate2.try_acquire().is_none(), "克隆必须指向同一闸");
    drop(held);
}

// 「恢复默认」必须把**外观设置**的 UI 半场一并分派：字体族/字号要重放
// 主题（`font.family`/`font.size` 装在全局 Theme 上）、密度要写间距刻度、
// 文件图标来源要重开系统图标（`file_icon::set_enabled`）。
// 缺信号则数据面已复位、界面还是旧字体/旧间距/旧图标——用户视角就是
// 「恢复默认没生效」（本仓库历史上已在主题/列设置上栽过同款）。
#[test]
fn reset_dispatches_font_and_density_effects() {
    let (tx, rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::unbounded();
    let mut data = app();
    data.signal_tx = Some(std::sync::Arc::new(tx));

    data.apply_reset();

    let mut got = Vec::new();
    while let Ok(sig) = rx.try_recv() {
        if let crate::viewmodel::VmSignal::SettingApplied(id) = sig {
            got.push(id);
        }
    }
    for want in [
        SettingId::Theme,
        SettingId::FontFamily,
        SettingId::FontSize,
        SettingId::CornerRadius,
        SettingId::Density,
        SettingId::FileTypeIcons,
        // T-KEYS：keymap 是 `App` 级全局态（快照不携带）——不补发信号则
        // 「配置复位了、按下仍是旧快捷键」。
        SettingId::Keymap,
    ] {
        assert!(
            got.contains(&want),
            "恢复默认应分派 {want:?} 的 UI 半场，实得 {got:?}"
        );
    }
}

// —— MRU 纯函数（T-MENU4 单测锚点：设计 §6 测试锚点 2）——

#[test]
fn mru_append_dedupes_and_promotes() {
    let list = vec!["a.zip".to_string(), "b.7z".to_string()];
    // 重开 a.zip → 置顶去重
    let out = mru_append(&list, "a.zip");
    assert_eq!(out, vec!["a.zip", "b.7z"]);
    // 新路径 → 头插
    let out = mru_append(&list, "c.rar");
    assert_eq!(out, vec!["c.rar", "a.zip", "b.7z"]);
}

#[test]
fn mru_append_caps_at_ten() {
    let list: Vec<String> = (0..10).map(|i| format!("{i}.zip")).collect();
    let out = mru_append(&list, "new.zip");
    assert_eq!(out.len(), 10);
    assert_eq!(out[0], "new.zip");
    // 列表头 = 最近；cap 裁尾 = 裁掉最旧（尾部的 9.zip）
    assert!(!out.contains(&"9.zip".to_string()));
    assert!(out.contains(&"0.zip".to_string()));
}

#[test]
fn mru_remove_drops_target_only() {
    let list = vec!["a.zip".to_string(), "b.7z".to_string()];
    let out = mru_remove(&list, "a.zip");
    assert_eq!(out, vec!["b.7z"]);
    // 不存在 → 原样
    let out = mru_remove(&list, "x.zip");
    assert_eq!(out, list);
}

/// 注册表取值构造辅助（T-SET：控件上行 `SettingChanged` 的原样载荷）。
fn ix(v: usize) -> settings_registry::SettingValue {
    settings_registry::SettingValue::Index(v)
}
fn yes(v: bool) -> settings_registry::SettingValue {
    settings_registry::SettingValue::Bool(v)
}
fn set<I, S>(items: I) -> settings_registry::SettingValue
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    settings_registry::SettingValue::StrSet(items.into_iter().map(Into::into).collect())
}
fn text(v: &str) -> settings_registry::SettingValue {
    settings_registry::SettingValue::Text(v.to_string())
}
fn keys(v: hzip_config::Keys) -> settings_registry::SettingValue {
    settings_registry::SettingValue::Keys(Box::new(v))
}
/// 非默认 `Keys`：自定义键位（`f2`）+ `"none"` 禁用哨兵（三态里的两种都必须能复位）。
fn keys_sample() -> hzip_config::Keys {
    let mut k = hzip_config::Keys::default();
    *k.field_mut("select_all").expect("select_all 是契约键名") = "f2".into();
    *k.field_mut("quit").expect("quit 是契约键名") = "none".into();
    k
}
fn changed(id: SettingId, value: settings_registry::SettingValue) -> DialogEvent {
    DialogEvent::SettingChanged { id, value }
}

// SettingChanged 事件 → 镜像字段（T-SET 统一流程数据面）
#[test]
fn consume_settings_events_updates_state() {
    let mut data = app();
    assert!(data.consume_dialog_event(&changed(SettingId::Theme, ix(2))));
    assert_eq!(data.settings.theme_ix, 2);
    assert!(data.consume_dialog_event(&changed(SettingId::Lang, ix(1))));
    assert_eq!(data.settings.lang_ix, 1);
    // 越界下标回落（normalize 语义）
    assert!(data.consume_dialog_event(&changed(SettingId::Theme, ix(9))));
    assert_eq!(data.settings.theme_ix, 0);
    // 类型不符 → 整项忽略（不 panic、不改状态）
    assert!(data.consume_dialog_event(&changed(SettingId::Theme, yes(true))));
    assert_eq!(data.settings.theme_ix, 0);
    // 非设置事件不消费
    assert!(!data.consume_dialog_event(&DialogEvent::CompressStart));
}

// SettingChanged(Columns) → 镜像落地 + 快照携带（浏览页唯一列设置来源；
// 旧 `columns_dirty` 旁路已删——冷启动不回放根因）
#[test]
fn consume_columns_event_updates_state() {
    let mut data = app();
    let next = ColumnsConfig {
        hidden: vec![crate::ui::contracts::BrowseColumnId::Packed],
        order: crate::ui::contracts::BrowseColumnId::ALL.to_vec(),
    };
    assert!(data.consume_dialog_event(&changed(
        SettingId::Columns,
        settings_registry::SettingValue::Columns(next.clone())
    )));
    assert_eq!(data.settings.columns, next);
    // 出快照（泵 → ShellState → 浏览页 delegate 的唯一通路）
    assert_eq!(data.export_snapshot().columns, next);
}

// R2-03：常规/关联/上下文/查看器项 → prefs 落账（内存态语义；写盘
// 失败仅 warn，断言面向 settings 字段）
#[test]
fn consume_r2_settings_events_update_prefs() {
    let mut data = app();
    assert!(data.consume_dialog_event(&changed(SettingId::ShowDotdot, yes(false))));
    assert!(!data.settings.show_dotdot);
    assert!(data.consume_dialog_event(&changed(SettingId::AutoUpdate, yes(true))));
    assert!(data.settings.auto_update);
    assert!(data.consume_dialog_event(&changed(SettingId::Assoc, set(["zip", "7z"]))));
    assert_eq!(
        data.settings.assoc,
        vec!["zip".to_string(), "7z".to_string()]
    );
    // T-CM：右键菜单项是 Commit::Manual——**勾选只进草稿，不动已应用值**
    // （写操作系统很贵且用户可见，必须由「应用」按钮显式触发）
    assert!(data.consume_dialog_event(&changed(SettingId::CtxExtract, set(["extract_here"]))));
    assert_eq!(
        data.settings.ctx_extract,
        Vec::<String>::new(),
        "勾选不得直接改已应用值"
    );
    let draft = data.ctx_draft.as_ref().expect("应写入草稿");
    assert_eq!(draft.extract, vec!["extract_here".to_string()]);
    assert!(data.consume_dialog_event(&changed(SettingId::CtxCompress, set(["add_zip"]))));
    assert_eq!(data.settings.ctx_compress, Vec::<String>::new());
    assert_eq!(
        data.ctx_draft.as_ref().expect("草稿应保留").compress,
        vec!["add_zip".to_string()]
    );
    assert!(data.consume_dialog_event(&changed(SettingId::FolderSize, yes(true))));
    assert!(data.settings.show_folder_size);
    assert!(data.consume_dialog_event(&changed(SettingId::ImageViewer, text("C:\\imv.exe"))));
    assert_eq!(data.settings.image_viewer, "C:\\imv.exe");
    assert!(data.consume_dialog_event(&changed(SettingId::Editor, text("C:\\ed.exe"))));
    assert_eq!(data.settings.editor, "C:\\ed.exe");
    // 非白名单勾选 key 丢弃 + 白名单顺序（与 store 同一归一化函数）
    assert!(data.consume_dialog_event(&changed(
        SettingId::CtxExtract,
        set(["bogus", "extract_to", "extract_here"])
    )));
    assert_eq!(
        data.ctx_draft.as_ref().expect("草稿应存在").extract,
        vec!["extract_here".to_string(), "extract_to".to_string()]
    );
}

// T-CM：草稿 → 应用的完整闭环（落盘一次 + 清草稿 + 已应用值更新）
#[test]
fn ctx_draft_commits_to_applied_values_and_clears() {
    let mut data = app();
    assert!(
        !data.commit_ctx_draft(),
        "无草稿时提交应返回 false（调用方据此跳过 OS 写入）"
    );

    assert!(data.consume_dialog_event(&changed(SettingId::CtxExtract, set(["extract_here"]))));
    assert!(data.consume_dialog_event(&changed(SettingId::CtxCompress, set(["add_7z"]))));
    assert!(data.ctx_draft.is_some());

    assert!(data.commit_ctx_draft(), "有草稿时提交应返回 true");
    assert_eq!(data.settings.ctx_extract, vec!["extract_here".to_string()]);
    assert_eq!(data.settings.ctx_compress, vec!["add_7z".to_string()]);
    assert!(data.ctx_draft.is_none(), "提交后草稿必须清空");
    assert!(!data.commit_ctx_draft(), "二次提交不应重复触发");
}

// T-CM：草稿以「已应用值」为基线——改一项不会清掉另一项
#[test]
fn ctx_draft_baseline_preserves_other_group() {
    let mut data = app();
    data.settings.ctx_extract = vec!["extract_here".to_string()];
    data.settings.ctx_compress = vec!["add_zip".to_string()];
    // 只动解压组
    assert!(data.consume_dialog_event(&changed(SettingId::CtxExtract, set(["auto_extract"]))));
    let draft = data.ctx_draft.as_ref().unwrap();
    assert_eq!(draft.extract, vec!["auto_extract".to_string()]);
    assert_eq!(
        draft.compress,
        vec!["add_zip".to_string()],
        "未触碰的组必须以已应用值为基线保留"
    );
}

// T-CM：「清除」把两个集合归零并丢弃草稿（与系统侧 uninstall 配套）
#[test]
fn clearing_ctx_menu_zeroes_both_groups_and_drops_draft() {
    let mut data = app();
    data.settings.ctx_extract = vec!["extract_here".to_string()];
    data.settings.ctx_compress = vec!["add_zip".to_string()];
    data.consume_dialog_event(&changed(SettingId::CtxExtract, set(["auto_extract"])));

    data.clear_ctx_menu_selection();

    assert!(data.settings.ctx_extract.is_empty());
    assert!(data.settings.ctx_compress.is_empty());
    assert!(
        data.ctx_draft.is_none(),
        "清除后草稿不得残留（否则下次「应用」会把旧勾选写回系统）"
    );
}

// T-CM：「恢复默认」必须同时清草稿
#[test]
fn reset_discards_ctx_draft() {
    let mut data = app();
    assert!(data.consume_dialog_event(&changed(SettingId::CtxExtract, set(["extract_here"]))));
    assert!(data.ctx_draft.is_some());
    data.apply_reset();
    assert!(data.ctx_draft.is_none(), "恢复默认后草稿必须清空");
}

// T-CM：草稿优先投影——面板显示未提交的勾选，而不是已应用值
#[test]
fn draft_wins_over_applied_in_settings_view() {
    let mut data = app();
    data.settings.ctx_extract = vec!["extract_here".to_string()];
    assert_eq!(
        data.settings_view(Vec::new())
            .value(SettingId::CtxExtract)
            .and_then(|v| v.as_str_set())
            .map(<[String]>::to_vec),
        Some(vec!["extract_here".to_string()])
    );
    assert!(!data.settings_view(Vec::new()).has_pending_ctx_changes());

    assert!(data.consume_dialog_event(&changed(SettingId::CtxExtract, set(["auto_extract"]))));
    let view = data.settings_view(Vec::new());
    assert_eq!(
        view.value(SettingId::CtxExtract)
            .and_then(|v| v.as_str_set())
            .map(<[String]>::to_vec),
        Some(vec!["auto_extract".to_string()]),
        "有草稿时投影必须显示草稿"
    );
    assert!(view.has_pending_ctx_changes(), "应提示有未应用的更改");
}

// ApplyAssoc/RemoveAssoc：数据面无动作（勾选集合已由 SettingChanged 落账；
// 注册表写归主控后台段——此处不 panic 即语义正确）
#[test]
fn consume_assoc_apply_remove_are_noop_on_data() {
    let mut data = app();
    assert!(data.consume_dialog_event(&DialogEvent::SettingsApplyAssoc));
    assert!(data.consume_dialog_event(&DialogEvent::SettingsRemoveAssoc));
    assert!(data.consume_dialog_event(&DialogEvent::SettingsAssocOpenAppSettings));
    assert!(data.consume_dialog_event(&DialogEvent::SettingsCheckUpdate));
    assert_eq!(data.settings, SettingsPrefs::default());
}

// CTX 白名单完整性：key 唯一（README §2.3——注册表 verb 与 UI 勾选共用）
#[test]
fn ctx_key_whitelists_unique_and_complete() {
    let ext_keys: Vec<&str> = crate::ui::contracts::CTX_EXTRACT_KEYS
        .iter()
        .map(|(k, _)| *k)
        .collect();
    let cmp_keys: Vec<&str> = crate::ui::contracts::CTX_COMPRESS_KEYS
        .iter()
        .map(|(k, _)| *k)
        .collect();
    // 与平台 verb 表**同源对拍**（跨 crate 契约）：顺序即菜单顺序——
    // 少一个 key = 少一个菜单项；多一个 key = 永远勾不上的死条目。
    // 硬编码数字守不住这条（改了 VERBS 忘了改白名单，数字照样通过）。
    use hzip_platform::context_menu::{MenuScope, VERBS};
    let from_table = |scope: MenuScope| -> Vec<&'static str> {
        VERBS
            .iter()
            .filter(|(_, spec)| spec.scopes.contains(&scope))
            .map(|(k, _)| *k)
            .collect()
    };
    // 解压组 = ArchiveFile（含 win_skip 的 open_hzip：Windows 不挂载但仍是
    // 合法勾选项，GUI 文案必须有）
    let mut expect_extract = from_table(MenuScope::ArchiveFile);
    let expect_compress = from_table(MenuScope::Any);
    assert_eq!(ext_keys, expect_extract, "解压白名单须与 VERBS 逐项同序");
    assert_eq!(cmp_keys, expect_compress, "压缩白名单须与 VERBS 逐项同序");
    // 条数下限（表本身被改空也当失败）
    assert_eq!(ext_keys.len(), 7);
    assert_eq!(cmp_keys.len(), 4);
    expect_extract.clear();
    let mut all = ext_keys.clone();
    all.extend_from_slice(&cmp_keys);
    let mut sorted = all.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len(), "key 重复：{all:?}");
    // label 非空（UI 勾选文案）
    for (_, label) in crate::ui::contracts::CTX_EXTRACT_KEYS {
        assert!(!label.is_empty());
    }
    for (_, label) in crate::ui::contracts::CTX_COMPRESS_KEYS {
        assert!(!label.is_empty());
    }
}

// 嵌套会话栈（R-GUI2-06：双击内层压缩包当目录进）——压栈保留父层形态、
// 弹栈恢复 + 删内层临时档案、面包屑跨层带层号。
//
// 守的是「双击压缩包进得去也回得来」：进入内层后若把父层会话丢了（或反之
// 退出后回不到原来的子目录），用户看到的是「点了档案、位置全乱」。
#[test]
fn nested_session_stack_round_trip_and_crumbs() {
    let dir = tempfile::TempDir::new().unwrap();
    // 内层档案：先写一份真实 tar 到磁盘，再把它的字节嵌进外层档案
    let inner_path = dir.path().join("inner.tar");
    crate::viewmodel::testutil::write_ustar(&inner_path, &[("deep/inner.txt", b"x")]);
    let inner_bytes = std::fs::read(&inner_path).unwrap();
    let outer = dir.path().join("outer.tar");
    crate::viewmodel::testutil::write_ustar(
        &outer,
        &[("sub/keep.txt", b"k"), ("sub/child.tar", &inner_bytes)],
    );

    let mut data = app();
    data.main.open(&data.engine, &outer).expect("open");
    assert!(!data.export_snapshot().nested, "顶层会话不是嵌套会话");
    assert!(!data.pop_open_frame(), "顶层会话弹栈应为 no-op");

    // 用户进到 sub/，双击 child.tar（open_container 的落账部分在此手动复现）
    data.main.enter_dir("sub");
    assert!(
        data.main.entry_id_of_path("sub/child.tar").is_some(),
        "容器条目可按档案内路径查到（嵌套打开的入口查询）"
    );
    assert!(
        data.main.entry_id_of_path("sub/keep.txt").is_some(),
        "普通文件同样可查"
    );
    assert!(
        data.main.entry_id_of_path("sub").is_none(),
        "目录不参与（目录没有可读数据流，不该被当成容器）"
    );
    assert!(data.main.entry_id_of_path("nope").is_none());
    // 用**完整档案内路径**进入（外层在档案根，容器在 sub/ 下）
    data.begin_nested_session("sub/child.tar");
    let browser = data
        .engine
        .browse(
            hzip_format::Source::File(inner_path.clone()),
            &hzip_format::OpenOptions::default(),
        )
        .expect("browse 内层");
    let mut child = crate::viewmodel::MainViewModel::new();
    child
        .install_browser(browser, &inner_path)
        .expect("装内层会话");
    data.main = child;
    data.adopt_temp_source(inner_path.clone());

    let snap = data.export_snapshot();
    assert!(snap.nested, "进入容器后快照应标记嵌套");
    assert_eq!(snap.archive_name, "inner.tar", "标题/面包屑根 = 内层档案");
    assert_eq!(
        snap.crumbs
            .iter()
            .map(|c| (c.label.as_str(), c.level))
            .collect::<Vec<_>>(),
        vec![("outer.tar", 1), ("sub", 1), ("inner.tar", 0)],
        "面包屑 = 外层档案 › 父层子目录 › 内层档案（层号跨层递进）"
    );
    // 内层档案根也出现 `..` 行（crumbs.len() > 1）——`..` 在那儿表示「退出该层」
    assert!(crate::ui::binding::show_parent_row(&snap.crumbs));

    // 组合导航树：**根仍是最外层档案**，内层压缩包作为容器节点挂在其目录下
    // （用户口径「子压缩包当子目录」——不能改成以子压缩包为根）
    data.rebuild_browse_rows();
    let nav: Vec<(String, usize, usize, bool, bool)> = data
        .nav_rows
        .iter()
        .map(|r| (r.path.clone(), r.level, r.depth, r.is_dir, r.is_container))
        .collect();
    assert_eq!(
        nav[0],
        (String::new(), 1, 0, true, false),
        "根行 = 最外层档案（层号 1），实得 {nav:?}"
    );
    assert!(
        nav.iter()
            .any(|(p, lvl, _, _, c)| p == "sub" && *lvl == 1 && !*c),
        "外层目录 sub 在树里（层 1）：{nav:?}"
    );
    let container = nav
        .iter()
        .find(|(p, _, _, _, c)| p == "sub/child.tar" && *c)
        .unwrap_or_else(|| panic!("容器节点必须在树里：{nav:?}"));
    assert_eq!(container.1, 1, "容器节点属于外层会话（层 1）");
    assert!(!container.3, "容器不是目录（靠 is_container 判图标/路由）");
    assert!(
        nav.iter()
            .any(|(p, lvl, depth, _, _)| p == "deep" && *lvl == 0 && *depth > container.2),
        "内层会话的目录挂在容器节点下面（层 0、深度更深）：{nav:?}"
    );

    // 退层：父层会话原样恢复（仍在 sub/），内层临时档案随之删除
    assert!(data.pop_open_frame(), "嵌套会话应能退层");
    assert_eq!(data.main.current_dir(), "sub", "父层目录形态必须保留");
    assert_eq!(data.current_archive_name(), "outer.tar");
    assert!(
        !inner_path.exists(),
        "退层后内层临时档案应删除（否则临时目录越攒越多）"
    );
    assert!(!data.export_snapshot().nested);
}

// SettingsReset：注册表驱动全默认（写盘在 persist 内、失败仅 warn——
// 本断言面向内存态语义）
#[test]
fn reset_restores_defaults() {
    let mut data = app();
    assert!(data.consume_dialog_event(&changed(SettingId::Theme, ix(2))));
    assert!(data.consume_dialog_event(&changed(SettingId::Format, ix(4))));
    assert!(data.consume_dialog_event(&changed(SettingId::Level, ix(5))));
    assert!(data.consume_dialog_event(&changed(SettingId::Assoc, set(["zip"]))));
    // 快捷键（T-KEYS）：先摆一个自定义键位 + 一个「禁用」哨兵，恢复默认后
    // 整节必须回到全空（= 全部动作走内置默认键位）——`[keys]` 的复位链是
    // 「REGISTRY 全默认」自动覆盖的，本断言把它钉住，防有人把 Keymap 行漏登记。
    assert!(data.consume_dialog_event(&changed(SettingId::Keymap, keys(keys_sample()))));
    assert_eq!(
        data.settings.keys,
        keys_sample(),
        "改动先落地（否则下面断言空转）"
    );
    assert!(data.consume_dialog_event(&DialogEvent::SettingsReset));
    assert_eq!(data.settings, SettingsPrefs::default());
    assert_eq!(
        data.settings.keys,
        hzip_config::Keys::default(),
        "整页恢复默认必须复位快捷键（含禁用项 → 回默认键位）"
    );
}

// load_settings：从 hzip-config 恢复（含未知值回落；格式/级别读 [defaults] 权威键）
#[test]
fn load_settings_maps_config_values() {
    let mut config = hzip_config::Config::defaults();
    config.ui.theme = "dark".into();
    config.ui.lang = "en".into();
    config.defaults.format = "7z".into();
    config.defaults.level = "ultra".into();
    // 快捷键（T-KEYS）：`[keys]` 整节原样进镜像（本层不校验键位串——非法串
    // 由 `ui::shell::keymap` 装配时回落默认并 warn）
    config.keys.select_all = "f2".into();
    config.keys.quit = "none".into();
    let mut data = app();
    data.load_settings(&config);
    assert_eq!(data.settings.theme_ix, 2);
    assert_eq!(data.settings.lang_ix, 2);
    assert_eq!(data.settings.format_ix, 1);
    assert_eq!(data.settings.level_ix, 5);
    assert_eq!(data.settings.keys, config.keys, "[keys] 整节入镜像");
    assert_eq!(
        data.settings.keys.field("select_all"),
        Some("f2"),
        "自定义键位原样搬入"
    );

    // 未知值回落默认
    config.ui.theme = "sepia".into();
    data.load_settings(&config);
    assert_eq!(data.settings.theme_ix, 0);
}

// ---- R-GUI1-02 §2.1：浏览快照派生 ----

// 直接子项判定纯函数（隐藏文件含在顶层：`.hidden` 是名字、不是目录段）
#[test]
fn is_direct_child_matches_spec_cases() {
    assert!(is_direct_child("", "a.txt"));
    assert!(!is_direct_child("", "d/a.txt"));
    assert!(is_direct_child("d", "d/a.txt"));
    assert!(!is_direct_child("d", "d/sub/b.txt"));
    assert!(is_direct_child("", ".hidden"), "隐藏文件是顶层直接子项");
}

// 未打开档案：refresh 清空双栏 + export_snapshot 默认值
#[test]
fn refresh_browse_rows_closed_archive_clears() {
    let mut data = app();
    std::sync::Arc::make_mut(&mut data.nav_rows).push(TreeRow {
        path: "docs".into(),
        name: "docs".into(),
        depth: 0,
        is_dir: true,
        entry_id: None,
        expanded: false,
        has_children: true,
        is_container: false,
        encrypted: false,
        size_raw: 0,
        size_packed: 0,
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,
        container_format: None,
    });
    std::sync::Arc::make_mut(&mut data.dir_rows).push(TreeRow {
        path: "a.txt".into(),
        name: "a.txt".into(),
        depth: 0,
        is_dir: false,
        entry_id: None,
        expanded: false,
        has_children: false,
        is_container: false,
        encrypted: false,
        size_raw: 3,
        size_packed: 0,
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,
        container_format: None,
    });
    data.rebuild_browse_rows();
    assert!(data.nav_rows.is_empty());
    assert!(data.dir_rows.is_empty());

    let snap = data.export_snapshot();
    assert_eq!(snap.archive_name, "");
    assert_eq!(snap.archive_path, "");
    assert!(
        snap.crumbs.is_empty() || snap.crumbs.len() == 1,
        "空名根段仅 1"
    );
    assert_eq!(snap.entry_count, 0);
    assert!(snap.job_rows.is_empty());
    assert!(snap.tab.is_none() && snap.dialog_open.is_none() && snap.job_pill.is_none());
}

// 目录行聚合大小：show_folder_size 开 → 右栏目录行 = 子孙文件和
// （根因回归锚点：旧 fold_dir_sizes 在"仅一层子项"的行集上求和恒 0，
// 勾选后目录大小恒显示 "—"）。隐式目录（tar 无目录条目）同样命中。
#[test]
fn refresh_browse_rows_fills_dir_sizes_when_enabled() {
    let dir = tempfile::TempDir::new().unwrap();
    let ar = dir.path().join("t.tar");
    crate::viewmodel::testutil::write_ustar(
        &ar,
        &[
            ("root/a.txt", b"aa"),
            ("root/sub/b.txt", b"bbbb"),
            ("top.txt", b"t"),
        ],
    );
    let mut data = app();
    data.main.open(&data.engine, &ar).expect("open");

    // 开关关（默认）：目录行不带聚合（展示层回 "—"）
    data.rebuild_browse_rows();
    let root = data
        .dir_rows
        .iter()
        .find(|r| r.path == "root")
        .expect("顶层目录行");
    assert_eq!(root.size_raw, 0, "开关关 → 不回填");

    // 开关开：root = a.txt(2) + sub/b.txt(4) = 6（tar 目录条目隐式，纯路径命中）
    data.settings.show_folder_size = true;
    data.rebuild_browse_rows();
    let root = data
        .dir_rows
        .iter()
        .find(|r| r.path == "root")
        .expect("顶层目录行");
    assert_eq!(root.size_raw, 6);
    // 文件行不受影响
    let top = data
        .dir_rows
        .iter()
        .find(|r| r.path == "top.txt")
        .expect("顶层文件行");
    assert_eq!(top.size_raw, 1);

    // 进子目录同样命中（逐层浏览的第二层验证）
    data.main.enter_dir("root");
    data.rebuild_browse_rows();
    let sub = data
        .dir_rows
        .iter()
        .find(|r| r.path == "root/sub")
        .expect("子目录行");
    assert_eq!(sub.size_raw, 4);
}

// T-PERF1 会话保活（2026-09-24 审计 B3 改写为**请求式**）：已打开时零成本
// （任务进度 tick 每秒 10 次调用，不能有副作用）；会话释放（编辑提交形态）
// + 档案仍在 → 返回重开请求并置在飞；真活在 shell 的后台线程里跑。
#[test]
fn browse_session_reopen_request_only_when_released_and_deduped() {
    let dir = tempfile::TempDir::new().unwrap();
    let ar = dir.path().join("t.tar");
    crate::viewmodel::testutil::write_ustar(&ar, &[("a.txt", b"a"), ("b.txt", b"b")]);
    let mut data = app();
    data.main.open(&data.engine, &ar).expect("open");
    data.archive = Some(ar.clone());
    data.rebuild_browse_rows();

    // 已打开：不请求（零成本路径）
    assert!(data.browse_session_reopen_request().is_none());

    // 编辑提交形态：会话释放（唯一编辑提交点的前置，含武装）+ 档案路径在 → 请求
    data.main.release_browser();
    data.browse_reopen_armed.set(true);
    assert!(!data.main.is_open());
    assert_eq!(
        data.browse_session_reopen_request().as_deref(),
        Some(ar.as_path()),
        "会话释放 + 已武装 → 保活接管（返回重开路径）"
    );
    assert!(
        data.browse_session_reopen_request().is_none(),
        "在飞期间不得重复请求（否则每 tick 起一个线程）"
    );

    // 后台重开成功收口：装会话 + 重建行 + 清在飞
    let browser = data
        .engine
        .browse(
            hzip_format::Source::File(ar.clone()),
            &hzip_format::OpenOptions::default(),
        )
        .expect("browse");
    data.main.install_browser(browser, &ar).expect("装会话");
    data.finish_browse_reopen(Ok(()));
    assert!(data.main.is_open(), "重开后会话恢复");
    assert!(!data.dir_rows.is_empty(), "收口时重建了行集");
    assert!(data.browse_session_reopen_request().is_none(), "标志已清");

    // 失败收口：清空行集 + 清在飞（否则保活永久停摆）
    data.main.release_browser();
    data.browse_reopen_armed.set(true);
    assert!(data.browse_session_reopen_request().is_some());
    data.finish_browse_reopen(Err(hzip_error::HzipError::Unsupported {
        what: "模拟失败".into(),
    }));
    assert!(
        data.nav_rows.is_empty() && data.dir_rows.is_empty(),
        "失败清空行集"
    );
    data.browse_reopen_armed.set(true);
    assert!(
        data.browse_session_reopen_request().is_some(),
        "失败后仍可再试"
    );
}

// 2026-09-26 用户实测缺陷回归：「打开文件（预览/外部打开）后目录跳回根」。
//
// 根因：打开文件走三段式 `take_browser` → 后台读 → `restore_browser`，摘出
// 间隙 `is_open()==false` 被泵的保活兜底（`browse_session_reopen_request`）
// 误判成「编辑提交已释放会话」→ 重开新会话把 `current_dir`/`expanded` 打回根。
//
// 修复：重开需 **armed**（只有 `commit_edit_batch` 真正 `release_browser` 时
// 武装）。本测试证明：未武装时即使会话恰处摘出间隙，保活兜底也**绝不**重开；
// 会话恢复后 armed 自动复位。
#[test]
fn browse_reopen_never_fires_on_take_browser_gap() {
    let dir = tempfile::TempDir::new().unwrap();
    let ar = dir.path().join("t.tar");
    crate::viewmodel::testutil::write_ustar(&ar, &[("a.txt", b"a")]);
    let mut data = app();
    data.main.open(&data.engine, &ar).expect("open");
    data.archive = Some(ar.clone());

    // 模拟打开文件三段式的**摘出间隙**（未武装——`commit_edit_batch` 没跑过）
    let browser = data.main.take_browser().expect("摘出会话");
    assert!(!data.main.is_open(), "摘出期间呈无浏览态");
    assert!(
        data.browse_session_reopen_request().is_none(),
        "未武装：保活兜底不得把临时摘出当已释放（否则目录跳回根）"
    );

    // ③ 放回（用户实际场景：预览/物化完成）——目录层级原样保留
    data.main.restore_browser(browser);
    assert!(data.main.is_open());
    data.main.enter_dir("sub");
    data.rebuild_browse_rows();
    assert_eq!(data.main.current_dir(), "sub", "目录层级不受打开文件影响");
    assert!(data.browse_session_reopen_request().is_none());
}

// T-TVS 走查缺陷回归：菜单入口上行 SettingValue::Text（config 落盘值）——
// 旧 accepts 门禁比 store 双归一更严，Text 被静默丢弃（菜单「视图 ▸ 工具栏/
// 视图样式」点击无效果的根因）。断言 Text 路径真正落到镜像 + store 归一
#[test]
fn apply_setting_accepts_menu_text_value_for_select_items() {
    let mut data = app();
    data.apply_setting(
        crate::viewmodel::SettingId::ToolbarStyle,
        &settings_registry::SettingValue::Text("icon_only".into()),
    );
    assert_eq!(
        data.settings.toolbar_style_ix, 1,
        "Text 值归一到白名单下标（icon_only = 1）"
    );
    data.apply_setting(
        crate::viewmodel::SettingId::ViewStyle,
        &settings_registry::SettingValue::Text("icon".into()),
    );
    assert_eq!(data.settings.view_style_ix, 1, "view_style: icon = 1");
    // 未知 Text 值 → 归一回落默认下标 0（不 panic、不忽略事件）
    data.apply_setting(
        crate::viewmodel::SettingId::ViewStyle,
        &settings_registry::SettingValue::Text("bogus".into()),
    );
    assert_eq!(data.settings.view_style_ix, 0, "未知值回落默认");
}

// T-TVS3：选中集进快照（icon/list 模式选中高亮回显的数据源；与
// selection_count 同源——行点击上行 select_ids 后两者必须一致）
#[test]
fn export_snapshot_carries_selection_ids() {
    let dir = tempfile::TempDir::new().unwrap();
    let ar = dir.path().join("t.tar");
    crate::viewmodel::testutil::write_ustar(&ar, &[("a.txt", b"a"), ("b.txt", b"b")]);
    let mut data = app();
    data.main.open(&data.engine, &ar).expect("open");

    data.main.select_ids(&[1, 0]);
    let snap = data.export_snapshot();
    assert_eq!(snap.selection, vec![0, 1], "选中 ID 升序进快照");
    assert_eq!(snap.selection_count, 2, "计数与集合同源一致");
    // 清空 → 两面同时归零
    data.main.select_ids(&[]);
    let snap = data.export_snapshot();
    assert!(snap.selection.is_empty());
    assert_eq!(snap.selection_count, 0);
}

// T-SEL2：选中统计进快照（状态栏「已选 N 个文件，M 个文件夹，X（压缩后：
// Y）」数据源）。口径 = 全局选择集 ∩ 当前目录可见行——子目录/导航残留
// id 不计入；目录计入 dirs 且 size_raw 为聚合大小；空选择归零
#[test]
fn export_snapshot_carries_selection_stats() {
    let dir = tempfile::TempDir::new().unwrap();
    let ar = dir.path().join("t.tar");
    crate::viewmodel::testutil::write_ustar(&ar, &[("d/x.txt", b"12345678"), ("y.txt", b"ab")]);
    let mut data = app();
    data.main.open(&data.engine, &ar).expect("open");
    data.archive = Some(ar.clone());
    // 开聚合大小：目录行 size_raw = 子项合计（统计的目录口径断言依赖）
    data.settings.show_folder_size = true;
    data.rebuild_browse_rows();

    // 全选：根层可见行 = 隐式目录 d（tar 无目录条目 → entry_id None，
    // 不可选——不计入统计）+ 文件 y.txt（d/x.txt 在子目录，交集过滤）
    data.main.select_all(false);
    let snap = data.export_snapshot();
    assert_eq!(snap.sel_stats.files, 1, "根层可见可选文件 = y.txt");
    assert_eq!(snap.sel_stats.dirs, 0, "隐式目录行无 entry_id，不计入");
    assert_eq!(snap.sel_stats.size_raw, 2);

    // 注入实体目录行（zip 目录条目形态：is_dir + entry_id + 聚合大小）
    // → dirs 计数与聚合大小合计口径。复用 d/x.txt 的条目 id（其行不在
    // 根层行集——统计只受当前 dir_rows 的行字段驱动）
    let x_id = data.main.entry(0).expect("首条目").id;
    std::sync::Arc::make_mut(&mut data.dir_rows).push(crate::viewmodel::rows::TreeRow {
        path: "zdir".into(),
        name: "zdir".into(),
        depth: 0,
        is_dir: true,
        entry_id: Some(x_id),
        expanded: false,
        has_children: true,
        is_container: false,
        encrypted: false,
        size_raw: 100,
        size_packed: 50,
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,
        container_format: None,
    });
    data.main.select_ids(&[x_id]);
    let snap = data.export_snapshot();
    assert_eq!(snap.sel_stats.dirs, 1, "实体目录行计入 dirs");
    assert_eq!(snap.sel_stats.files, 0);
    assert_eq!(snap.sel_stats.size_raw, 100, "目录行聚合大小计入");
    assert_eq!(snap.sel_stats.size_packed, 50);

    // 进子目录：全局选择集含「当前目录外的 id」→ 交集过滤后只计可见行
    data.main.enter_dir("d");
    data.rebuild_browse_rows(); // dir_rows 镜像随导航重建（泵的常规路径）
    data.main.select_ids(&[0, 1]); // 全档案 id 空间（含根层 y.txt）
    let snap = data.export_snapshot();
    assert_eq!(snap.sel_stats.files, 1, "子目录可见文件 = d/x.txt");
    assert_eq!(snap.sel_stats.dirs, 0);
    assert_eq!(snap.sel_stats.size_raw, 8, "导航残留 id 被交集过滤");

    // 空选择 → 归零
    data.main.select_ids(&[]);
    let snap = data.export_snapshot();
    assert_eq!(snap.sel_stats, Default::default());
}

// R-GUI4：任务行 + 状态胶囊进快照（任务中心恒空回归锚点——jobs vm 登记
// 行后 export_snapshot 必须携带）
#[test]
fn export_snapshot_carries_job_rows_and_pill() {
    let data = app();
    data.jobs
        .lock()
        .unwrap()
        .register(crate::viewmodel::JobRow {
            job_id: 7,
            kind: "extract",
            state: crate::viewmodel::JobRowState::Running,
            progress: Some(0.5),
            speed_bps: 1024.0,
            eta: None,
            done_bytes: 5,
            total_bytes: 10,
            entries_done: 2,
            entries_total: 4,
            failures: Vec::new(),
            current_entry: None,
            final_line: None,
            recent_entries: std::collections::VecDeque::new(),
            finished: None,
            src: "Goose-win32-x64.zip".into(),
            dest: Some("E:/Downloads/test_unzip".into()),
            format: "zip".into(),
            started_at: None,
            elapsed: None,
        });
    let snap = data.export_snapshot();
    assert_eq!(snap.job_rows.len(), 1, "任务行必须进快照");
    assert_eq!(snap.job_rows[0].job_id, 7);
    let pill = snap.job_pill.expect("运行中任务 → 状态胶囊");
    assert_eq!(pill.job_id, 7);
    assert!((pill.progress - 0.5).abs() < 1e-6);
    assert!(
        pill.summary.contains("解压"),
        "胶囊文案含种类：{}",
        pill.summary
    );
    assert!(snap.dialog.is_none(), "无待决请求 → 弹窗投影空");
}
// —— 会话密码缓存：任务链「输一次就够」（用户口径 2026-09-29 #2）——

/// 等任务终局（Done/Partial/Failed/Cancelled）。
fn wait_job_terminal(d: &AppData, job: hzip_progress::JobId) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        {
            let jobs = d.jobs.lock().unwrap();
            if let Some(row) = jobs.row(job)
                && matches!(
                    row.state,
                    crate::viewmodel::JobRowState::Done
                        | crate::viewmodel::JobRowState::Failed
                        | crate::viewmodel::JobRowState::PartialDone
                        | crate::viewmodel::JobRowState::Cancelled
                )
            {
                return;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "任务 {job} 30s 未终局（密码没答上？）"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// 等任务链问密码（泵把 `JobRequest::Password` 灌进 router）。
fn wait_password_request(d: &AppData) -> hzip_progress::JobId {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Some(job) = d.router.lock().unwrap().active_job() {
            return job;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "30s 内任务链没问密码（加密档的重命名应当要密码）"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// 模拟用户应答密码（与 `prompt::answer_prompt` 同路径：jobs → router 锁序）。
fn answer_job_password(d: &mut AppData, job: hzip_progress::JobId, pw: &str) {
    let jobs = d.jobs.clone();
    let guard = jobs.lock().unwrap();
    let handle = guard.handle(job).expect("活跃任务句柄");
    d.router
        .lock()
        .unwrap()
        .submit_password(handle, zeroize::Zeroizing::new(pw.to_string()))
        .expect("提交密码");
}

/// 取消应答密码（= 用户关掉密码窗；`prompt::answer_prompt` 的 PasswordCancel 路径）。
fn cancel_job_password(d: &mut AppData, job: hzip_progress::JobId) {
    let jobs = d.jobs.clone();
    let guard = jobs.lock().unwrap();
    let handle = guard.handle(job).expect("活跃任务句柄");
    d.router.lock().unwrap().cancel_request(handle);
}

/// 用户口径 2026-09-29 #2 现场：打开**加密**档案后在里面重命名/新建/删除，
/// 每次都被重新问密码——因为任务链里应答的密码从未进会话缓存（只有浏览
/// 双击的密码门才会缓存）。修后：输一次 → 任务成功 → 进缓存 → 后续操作
/// （含重开后）不再问。
#[test]
fn task_password_answered_once_is_cached_after_success() {
    let dir = tempfile::TempDir::new().unwrap();
    let zip = dir.path().join("enc.zip");
    crate::viewmodel::testutil::write_zip_encrypted(
        &zip,
        &[("a.txt", b"a"), ("b.txt", b"b")],
        "pw-a",
    );
    let mut d = app_with_zip(&zip);
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    d.signal_tx = Some(std::sync::Arc::new(tx));
    assert!(d.cached_password(&zip).is_none(), "现场：尚未输过密码");

    // ① 第一次重命名：任务链要密码 → 应答 → 任务成功
    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: Some("a.txt".into()),
        text: "a.txt".into(),
        session_epoch: d.session_epoch(),
    });
    let job1 = d.commit_name_edit("a2.txt").expect("提交").expect("有任务");
    let asked = wait_password_request(&d);
    assert_eq!(asked, job1, "问密码的就是这次编辑任务");
    assert_eq!(
        d.jobs.lock().unwrap().password_key_of(job1),
        Some(zip.as_path()),
        "任务密码的缓存键 = 档案本体（读档案类任务）"
    );
    d.note_job_password(job1, zip.clone(), zeroize::Zeroizing::new("pw-a".into()));
    answer_job_password(&mut d, job1, "pw-a");
    wait_job_terminal(&d, job1);
    assert!(
        matches!(
            d.jobs.lock().unwrap().row(job1).unwrap().state,
            crate::viewmodel::JobRowState::Done
        ),
        "正确密码下编辑应成功"
    );
    assert!(
        d.cached_password(&zip).is_none(),
        "终局收口前不落缓存（没验过的密码不喂后续操作）"
    );
    d.commit_verified_job_passwords();
    assert_eq!(
        d.cached_password(&zip).as_deref().map(String::as_str),
        Some("pw-a"),
        "任务成功 → 密码进会话缓存（用户口径：输入一次就够）"
    );
    // 待验证表已清（不留无用条目）
    assert!(d.pending_job_passwords.is_empty());

    // ② 第二次重命名：缓存直接带上 → **不再问密码**，直接成功
    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: Some("b.txt".into()),
        text: "b.txt".into(),
        session_epoch: d.session_epoch(),
    });
    let job2 = d.commit_name_edit("b2.txt").expect("提交").expect("有任务");
    wait_job_terminal(&d, job2);
    assert!(
        matches!(
            d.jobs.lock().unwrap().row(job2).unwrap().state,
            crate::viewmodel::JobRowState::Done
        ),
        "第二次编辑必须**零弹窗**直接成功（缓存密码已带上）"
    );
    d.commit_verified_job_passwords();
    assert!(
        d.router.lock().unwrap().active_job().is_none(),
        "全程没有再问过密码"
    );
}

/// 错密码不入缓存：任务失败 → 丢弃待验证条目（不给后续操作喂坏密码）。
#[test]
fn failed_job_password_is_not_cached() {
    let dir = tempfile::TempDir::new().unwrap();
    let zip = dir.path().join("enc.zip");
    crate::viewmodel::testutil::write_zip_encrypted(&zip, &[("a.txt", b"a")], "pw-a");
    let mut d = app_with_zip(&zip);
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    d.signal_tx = Some(std::sync::Arc::new(tx));

    d.begin_name_edit(NameEdit {
        dir: String::new(),
        from: Some("a.txt".into()),
        text: "a.txt".into(),
        session_epoch: d.session_epoch(),
    });
    let job = d.commit_name_edit("a2.txt").expect("提交").expect("有任务");
    let asked = wait_password_request(&d);
    assert_eq!(asked, job);
    d.note_job_password(job, zip.clone(), zeroize::Zeroizing::new("wrong".into()));
    answer_job_password(&mut d, job, "wrong");
    // 错密码 → core 再问一次（重试语义）→ 此处按用户关窗处理（取消应答）
    assert_eq!(wait_password_request(&d), job, "错密码后重问的仍是同一任务");
    cancel_job_password(&mut d, job);
    wait_job_terminal(&d, job);
    d.commit_verified_job_passwords();
    assert!(
        d.cached_password(&zip).is_none(),
        "失败/取消的任务绝不入缓存（不给后续操作喂坏密码）"
    );
    assert!(
        d.pending_job_passwords.is_empty(),
        "未验证条目必须丢弃（不无限滞留）"
    );
}

/// 进程级共享：两个窗口（两份 AppData）共用一份密码缓存——「程序没关就
/// 继续用上一次的密码」。
#[test]
fn password_cache_is_shared_between_windows() {
    let mut a = app();
    let mut b = app();
    let shared = std::sync::Arc::new(crate::app::password_cache::PasswordCache::new());
    a.share_password_cache(shared.clone());
    b.share_password_cache(shared.clone());
    let zip = std::path::PathBuf::from("C:/x/enc.zip");
    a.remember_password(&zip, zeroize::Zeroizing::new("pw-a".into()));
    assert_eq!(
        b.cached_password(&zip).as_deref().map(String::as_str),
        Some("pw-a"),
        "窗口 B 复用窗口 A 解锁过的档案密码"
    );
    // 右键菜单触发的加解密（二实例转发）= 重新打开语义 → 清空
    shared.clear();
    assert!(b.cached_password(&zip).is_none());
}

mod edit_caps_tests {
    use super::*;

    /// caps_edit 投影（T-MENU6 菜单禁用矩阵输入）：tar（无编辑能力）→ false；
    /// 未开档案 → false。
    ///
    /// zip 路径的正例由 core 集成测试（tests/edit_service.rs）覆盖——GUI 侧
    /// 只验证「投影读的是 registry caps.edit 且未开档案即 false」这条接线。
    #[test]
    fn caps_edit_projects_registry_and_archive_presence() {
        let dir = tempfile::TempDir::new().unwrap();
        let tar = dir.path().join("t.tar");
        crate::viewmodel::testutil::write_ustar(&tar, &[("a.txt", b"hello")]);

        let mut d = AppData::new(std::sync::Arc::new(crate::viewmodel::testutil::engine()));
        assert!(!d.caps_edit(), "未开档案 → 不可编辑");

        d.main.open(&d.engine.clone(), &tar).unwrap();
        d.archive = Some(tar);
        assert!(!d.caps_edit(), "tar：caps.edit=false → 不可编辑");
    }
}

mod edit_changeset_tests {
    use super::*;

    /// 删除变更集（T-MENU7）：ID → Change::Delete 保序；同一选择集重复组装
    /// 产出相同变更集（幂等锚点）。
    #[test]
    fn delete_changeset_is_deterministic_and_idempotent() {
        let cs = delete_changeset(&[3, 1, 2]);
        assert_eq!(cs.changes.len(), 3);
        for (c, want) in cs.changes.iter().zip([3usize, 1, 2]) {
            match c {
                hzip_format::Change::Delete(id) => assert_eq!(*id, want),
                other => panic!("应为 Delete，实为 {other:?}"),
            }
        }
        // 空选择 → 空变更集（调用方据此拒绝提交）
        assert!(delete_changeset(&[]).changes.is_empty());
        // 同输入重复组装 → 相同条目形态（「重复执行幂等」的机制保证；
        // `ChangeSet` 无 PartialEq——按条目逐个比对）
        let again = delete_changeset(&[3, 1, 2]);
        assert_eq!(again.changes.len(), cs.changes.len());
        for (a, b) in again.changes.iter().zip(cs.changes.iter()) {
            match (a, b) {
                (hzip_format::Change::Delete(x), hzip_format::Change::Delete(y)) => {
                    assert_eq!(x, y)
                }
                _ => panic!("重复组装应产出同样的 Delete 序列"),
            }
        }
    }

    /// 追加变更集（T-MENU6/8）：目标名直取调用方组装值、按目标全名判冲突、
    /// 无名条目跳过（T-MENU8 双向粘贴 P0 新口径）。
    #[test]
    fn append_changeset_counts_conflicts_and_skips_nameless() {
        let files = vec![
            (
                std::path::PathBuf::from("C:/src/a.txt"),
                "a.txt".to_string(),
            ),
            (
                std::path::PathBuf::from("C:/src/b.txt"),
                "b.txt".to_string(),
            ),
        ];
        let existing = ["a.txt".to_string()];
        let (cs, conflicts) = append_changeset(&files, |n| existing.iter().any(|e| e == n));
        assert_eq!(cs.changes.len(), 2);
        assert_eq!(conflicts, 1, "a.txt 同名冲突");
        match &cs.changes[0] {
            hzip_format::Change::Append { name, src } => {
                assert_eq!(name.as_str(), "a.txt", "档案内名 = 调用方组装值");
                assert_eq!(src, &std::path::PathBuf::from("C:/src/a.txt"));
            }
            other => panic!("应为 Append，实为 {other:?}"),
        }
        // 无冲突路径
        let (_, conflicts) = append_changeset(&files[1..], |n| existing.iter().any(|e| e == n));
        assert_eq!(conflicts, 0);
    }

    /// 目标名带目录前缀：冲突按**目标全名**比较（`docs/a.txt` ≠ 根下
    /// `a.txt`）；变更集条目名原样携带前缀（落位交给 fmt 层）。
    #[test]
    fn append_changeset_conflict_compares_full_target_name() {
        let entries = vec![
            (
                std::path::PathBuf::from("C:/src/a.txt"),
                "docs/a.txt".to_string(),
            ),
            (
                std::path::PathBuf::from("C:/src/b.txt"),
                "docs/b.txt".to_string(),
            ),
        ];
        // 根下已有 a.txt：目录前缀落位不与之冲突
        let root_only = ["a.txt".to_string()];
        let (cs, conflicts) = append_changeset(&entries, |n| root_only.iter().any(|e| e == n));
        assert_eq!(conflicts, 0, "带前缀目标名不与根下同名条目冲突");
        assert_eq!(cs.changes.len(), 2);
        match &cs.changes[0] {
            hzip_format::Change::Append { name, .. } => {
                assert_eq!(name.as_str(), "docs/a.txt", "前缀原样进条目名");
            }
            other => panic!("应为 Append，实为 {other:?}"),
        }
        // 同目录已有 docs/a.txt：全名命中计冲突
        let with_docs = ["a.txt".to_string(), "docs/a.txt".to_string()];
        let (_, conflicts) = append_changeset(&entries, |n| with_docs.iter().any(|e| e == n));
        assert_eq!(conflicts, 1, "docs/a.txt 全名冲突");
    }

    /// 无名条目（目标名空串）跳过：不进变更集、不计冲突（源路径末段语义
    /// 已移除，空名只可能来自调用方组装，直接丢弃）。
    #[test]
    fn append_changeset_skips_empty_target_name() {
        let entries = vec![
            (std::path::PathBuf::from("C:/src/a.txt"), String::new()),
            (
                std::path::PathBuf::from("C:/src/b.txt"),
                "b.txt".to_string(),
            ),
        ];
        let (cs, conflicts) = append_changeset(&entries, |n| n.is_empty());
        assert_eq!(cs.changes.len(), 1, "空名条目跳过");
        assert_eq!(
            conflicts, 0,
            "跳过条目不计冲突（判存闭包只命中空名——被跳过未判）"
        );
        match &cs.changes[0] {
            hzip_format::Change::Append { name, src } => {
                assert_eq!(name.as_str(), "b.txt");
                assert_eq!(src, &std::path::PathBuf::from("C:/src/b.txt"));
            }
            other => panic!("应为 Append，实为 {other:?}"),
        }
    }

    /// 粘贴目标名组装（`paste_target_name`）：根时取相对名本体，非根时
    /// `/` 连接（不用 `Path::join`——Windows 会产出 `\` 与档案内命名相撞）。
    #[test]
    fn paste_target_name_joins_with_archive_slash() {
        assert_eq!(paste_target_name("", "a.txt"), "a.txt", "根 = 相对名本体");
        assert_eq!(
            paste_target_name("docs", "a.txt"),
            "docs/a.txt",
            "非根 = current_dir/rel"
        );
        assert_eq!(
            paste_target_name("docs/src", "sub/b.bin"),
            "docs/src/sub/b.bin",
            "多层 current_dir 与多层 rel 同口径"
        );
    }
}
