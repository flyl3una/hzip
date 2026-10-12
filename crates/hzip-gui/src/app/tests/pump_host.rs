use super::*;

mod drain_tests {
    use super::*;
    use crate::viewmodel::router::PasswordDialogState;
    use hzip_core::jobs::PasswordReason;

    fn pw_req() -> JobRequest {
        JobRequest::Password {
            entry: None,
            reason: PasswordReason::Initial,
        }
    }

    /// 用户口径 2026-09-29：「除了关闭程序按钮，其他业务操作不能导致 hzip-gui
    /// 程序退出」——`exit_on_finish`（默认 false，用户可开）只在**成功的归档类
    /// 长操作**后生效，且仅限 `allow_quit`（Direct 模式）为真时。
    ///
    /// 现场缺陷：用户在加密档里右键重命名 → 任务失败 → 配了
    /// `exit_on_finish = true` 的界面**直接退出**（看起来像崩溃，错误也来不及看）。
    #[test]
    fn exit_on_finish_never_fires_for_failures_or_edit_jobs() {
        use crate::viewmodel::JobRowState::*;
        // 失败/部分失败/取消：一律不退（错误要看得到）
        for st in [Failed, PartialDone, Cancelled] {
            assert!(
                !exit_eligible_after_finish(true, Some(("extract", st)), false, true),
                "{st:?} 不得触发自动退出"
            );
        }
        // 交互类任务（编辑/打开）成功也不退——改个文件名不该把界面解散
        for kind in ["edit", "open", "copy"] {
            assert!(
                !exit_eligible_after_finish(true, Some((kind, Done)), false, true),
                "{kind} 成功不得触发自动退出"
            );
        }
        // 归档类长操作成功 + Direct 模式 → 退（原有语义保留）
        for kind in ["extract", "compress", "test", "convert"] {
            assert!(
                exit_eligible_after_finish(true, Some((kind, Done)), false, true),
                "{kind} 成功应可自动退出"
            );
        }
        // 归档类常操作成功但处于主界面模式（allow_quit=false）→ 不退：
        // 用户口径 2026-09-29 —— 业务操作完成不得把整个 hzip-gui 带走。
        assert!(
            !exit_eligible_after_finish(true, Some(("extract", Done)), false, false),
            "主界面模式：成功归档任务不得触发自动退出"
        );
        // 队列里残留失败行 → 不退（同一会话失败过就不解散界面）
        assert!(!exit_eligible_after_finish(
            true,
            Some(("extract", Done)),
            true,
            true
        ));
        // 开关关（默认）→ 恒不退；行缺失（None）→ 保守不退
        assert!(!exit_eligible_after_finish(
            false,
            Some(("extract", Done)),
            false,
            true
        ));
        assert!(!exit_eligible_after_finish(true, None, false, true));
    }

    /// 启动模式门（用户口径 2026-09-29 ⑧）：是否自动关窗由启动模式决定——
    /// 主界面（`allow_quit=false`）任何业务操作都不得带走程序；Direct 模式
    /// （右键动词进程，无主界面）保持「干完活自动关窗」的既有设计目的。
    #[test]
    fn exit_on_finish_requires_allow_quit_launch_mode() {
        use crate::viewmodel::JobRowState::Done;
        let success_archive = || Some(("compress", Done));
        assert!(
            !exit_eligible_after_finish(true, success_archive(), false, false),
            "主界面模式：成功归档任务 + exit_on=true 也不得退出"
        );
        assert!(
            exit_eligible_after_finish(true, success_archive(), false, true),
            "Direct 模式：成功归档任务 + exit_on=true 应保持自动退出"
        );
    }

    /// 弹窗互斥：已有待决密码弹窗时，后到任务的请求**留在通道**不被 intake；
    /// 当前弹窗收口后的下一轮才收——防 `active_job` 被顶掉、应答路由错任务。
    ///
    /// 覆盖线上缺陷（多任务并发加密解压）：先到任务等待应答期间，后到任务的
    /// 请求若顶掉弹窗状态，先到任务无人应答 → 120 s 决策超时。
    #[test]
    fn drain_defers_while_dialog_busy_then_intakes() {
        let jobs = Arc::new(Mutex::new(JobsViewModel::new()));
        let router = Arc::new(Mutex::new(DialogRouter::new()));
        let (tx, rx) = crossbeam_channel::bounded::<JobRequest>(1);
        let sigs: Arc<Mutex<Vec<VmSignal>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&sigs);
        let notifier: Arc<dyn crate::viewmodel::VmNotifier> =
            Arc::new(move |s| seen.lock().unwrap().push(s));

        // 任务 7 已弹窗待决（占用 router 单槽）
        assert!(router.lock().unwrap().intake(7, pw_req()));
        // 任务 9 的请求到达
        tx.send(pw_req()).unwrap();
        drain_requests(9, &jobs, &router, &rx, &notifier);
        assert_eq!(rx.len(), 1, "忙时请求必须留在通道（不得被消费）");
        assert!(
            !sigs
                .lock()
                .unwrap()
                .iter()
                .any(|s| matches!(s, VmSignal::DialogChanged)),
            "忙时不应再发 DialogChanged（弹窗状态未变）"
        );

        // 任务 7 终局 → 弹窗收口 → 下一轮收任务 9 的请求
        router.lock().unwrap().on_job_finished(7);
        drain_requests(9, &jobs, &router, &rx, &notifier);
        assert_eq!(rx.len(), 0, "空闲后应消费请求");
        assert!(
            matches!(
                router.lock().unwrap().password_state(),
                PasswordDialogState::Awaiting { job_id: 9, .. }
            ),
            "请求应落到任务 9 名下"
        );
        assert!(
            sigs.lock()
                .unwrap()
                .iter()
                .any(|s| matches!(s, VmSignal::DialogChanged)),
            "intake 成功应发 DialogChanged（触发弹窗）"
        );
    }

    /// 线上缺陷回归（2026-09-23 用户实测）：解压加密档案的早期密码问询发生在
    /// 「首个进度事件之后、下一个事件之前」的**无事件窗口**。泵若只在事件到达
    /// 时排空请求，密码窗永不出现，最终拖到 worker 侧 120 s 决策超时
    /// （hz-5002 → GUI 显示「不支持的操作」）。
    ///
    /// 本测试驱动真实 [`run_job_loop`]：**只投请求、不投任何进度事件**，断言
    /// 弹窗状态秒级内到账。
    #[test]
    fn run_job_loop_prompts_password_in_event_silence() {
        use std::time::Duration;
        let bus = hzip_progress::ProgressBus::new();
        let sub = bus.subscribe();
        let jobs = Arc::new(Mutex::new(JobsViewModel::new()));
        let router = Arc::new(Mutex::new(DialogRouter::new()));
        let (req_tx, req_rx) = crossbeam_channel::bounded::<JobRequest>(1);
        // 事件驱动等待（不轮询墙钟）：泵一投出 DialogChanged 立即醒，并行测试
        // 负载下也不靠时限撞运气。
        let (sig_tx, sig_rx) = std::sync::mpsc::channel::<VmSignal>();
        let notifier: Arc<dyn crate::viewmodel::VmNotifier> = Arc::new(move |s| {
            let _ = sig_tx.send(s);
        });

        let (jobs_t, router_t) = (Arc::clone(&jobs), Arc::clone(&router));
        let pump = std::thread::spawn(move || {
            let mut throttle = Throttle::new();
            run_job_loop(
                1,
                &jobs_t,
                &router_t,
                &sub,
                &req_rx,
                &mut throttle,
                &notifier,
            )
        });

        // 静默窗口：只有请求，没有任何进度事件
        req_tx.send(pw_req()).unwrap();
        let mut saw_dialog = false;
        while let Ok(sig) = sig_rx.recv_timeout(Duration::from_secs(30)) {
            if matches!(sig, VmSignal::DialogChanged) {
                saw_dialog = true;
                break;
            }
        }
        assert!(
            saw_dialog,
            "静默窗口内的密码请求必须被排空并弹窗（事件驱动排空 = 120 s 决策超时缺陷）"
        );
        assert!(
            matches!(
                router.lock().unwrap().password_state(),
                PasswordDialogState::Awaiting { job_id: 1, .. }
            ),
            "弹窗状态必须落在本任务名下"
        );

        drop(bus); // 总线关闭 → 泵退出
        assert!(pump.join().is_ok(), "任务泵线程不得 panic");
    }
}

mod forwarder_tests {
    use super::*;
    use crate::ui::contracts::{SignalRx, SignalTx};
    use std::sync::mpsc as std_mpsc;

    // forwarder roundtrip：后台线程 → SignalTx → forwarder → 回调断言
    #[test]
    fn signal_forwarder_roundtrip() {
        let (tx, rx): (SignalTx, SignalRx) = smol::channel::bounded(64);
        let (done_tx, done_rx) = std_mpsc::channel();
        let handle = std::thread::Builder::new()
            .spawn(move || {
                spawn_ui_forwarder(rx, move |sig| {
                    let _ = done_tx.send(sig);
                });
            })
            .unwrap();
        tx.try_send(VmSignal::JobsChanged).unwrap();
        tx.try_send(VmSignal::DialogChanged).unwrap();
        drop(tx); // 关闭 channel → forwarder 退出
        handle.join().unwrap();
        assert_eq!(done_rx.recv(), Ok(VmSignal::JobsChanged));
        assert_eq!(done_rx.recv(), Ok(VmSignal::DialogChanged));
    }

    // notifier 桥：SignalTx 版 make_notifier 等价物（try_send 即返回）
    #[test]
    fn signal_notifier_try_send_bridge() {
        let (tx, rx): (SignalTx, SignalRx) = smol::channel::bounded(8);
        let notifier = signal_notifier(std::sync::Arc::new(tx));
        notifier(VmSignal::EntriesChanged);
        assert_eq!(smol::block_on(rx.recv()), Ok(VmSignal::EntriesChanged));
    }
}
