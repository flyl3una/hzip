//! GUI 运行时（T-GUI T11 自 app.rs 拆分：run_ui + 泵循环 + argv 意图）。

use std::sync::Arc;

use hzip_core::engine::{Engine, EngineConfig};
use hzip_platform::temp_area::TempArea;

use gpui_kit::prelude::*;
use gpui_kit::size;

use super::AppShell;
use super::dispatch::{open_then_extract, open_then_extract_with_dest, submit_compress_locked};
use crate::app::exit;
use crate::app::instance::{InstanceRole, acquire_or_forward, take_forwarded, watch_forward_queue};
use crate::app::pump_host::spawn_ui_forwarder;
use crate::ui::constant::{MAIN_WINDOW_H, MAIN_WINDOW_W};
use crate::ui::contracts::ShellHandlers;

pub type AppDataHandle = std::sync::Arc<std::sync::Mutex<crate::app::state::AppData>>;

pub fn run_ui(args: LaunchArgs, raw_argv: Vec<String>) -> Result<(), String> {
    // 模式在**最前面**定下：它决定要不要取单实例锁、要不要开主窗口、
    // 要不要挂转发 watch（见 `launch_mode` 文档）。
    let mode = launch_mode(&args);
    // 「完成后退出」只在 Direct 模式（右键动词进程，无主界面）生效：主界面里
    // 任何业务操作都不得把程序带走（用户口径 2026-09-29 第 ⑧ 条）。泵侧
    // （`pump_host`）与进度窗（`views/progress`）都读这个全局量。
    crate::app::pump_host::ALLOW_QUIT_ON_FINISH
        .store(mode == UiMode::Direct, std::sync::atomic::Ordering::Relaxed);
    tracing::info!(
        mode = mode.name(),
        "hzip-gui 启动 {}：open={:?} extract-here={:?} auto-extract={:?} extract-dir={:?} extract-to={:?} extract-options={:?} compress={:?} compress-to={:?} format={:?} delete={}",
        env!("CARGO_PKG_VERSION"),
        args.open,
        args.extract_here,
        args.auto_extract,
        args.extract_dir,
        args.extract_to,
        args.extract_options,
        args.compress,
        args.compress_to,
        args.format,
        args.delete_flag
    );
    // 关联/右键菜单系统状态**后台预热**（审计 B7/B10）：首次打开设置页要拿它建
    // 状态行，而真查可能秒级（macOS `pluginkit` 子进程）——启动期先热一份，用户
    // 点设置时常态命中缓存（零系统调用）。
    crate::ui::contracts::warm_assoc_status_cache(hzip_config::Config::load().ui.assoc_formats);

    // —— 单实例仲裁（**仅主界面模式**；02 §11.2 B）——
    // 转发的是**整条 argv**（含动词）：二实例把命令行写进队列即退出，由首实例
    // 还原执行。只转发路径会丢动词——「智能解压 / 添加到 zip」在 GUI 已运行时
    // 会退化成「打开该路径」（见 app::instance 模块文档）。
    //
    // 直接模式刻意**不取锁、不转发、也不消费转发队列**：它是独立进程，队列归
    // 主实例所有（消费别人的转发 = 抢主界面的活）。理由见 `launch_mode`。
    let mut startup_intents = args_intents(&args);
    // —— 会话密码缓存（**进程级**，用户口径 2026-09-29）——
    //
    // 在单实例仲裁**之前**建：二实例转发（资源管理器右键菜单/关联动词触发的
    // 加解密）到达时要清空它——「跟 hzip-gui 重新打开一样」不吃旧密码。
    // 只影响本进程的界面操作：新进程天然为空（不落盘）。
    let password_cache = Arc::new(crate::app::password_cache::PasswordCache::new());
    let instance_guard = if mode == UiMode::Main {
        match acquire_or_forward(&raw_argv) {
            InstanceRole::Second => return Ok(()), // 二实例：已转发，退出
            InstanceRole::First(guard) => {
                // 本实例起来**之前**二实例可能已写入（watch 尚未挂载）
                for argv in take_forwarded() {
                    startup_intents.extend(intents_of_argv(&argv));
                }
                Some(guard)
            }
        }
    } else {
        None
    };

    // —— 意图队列：启动意图与运行期转发意图共用一条执行路径 ——
    //
    // 右键菜单项在 GUI 已运行时经二实例转发进来，**唯一的消费者就是这里**。
    // 没有这段接线，转发就是死信（历史缺陷症状：点菜单项毫无反应）。
    let (intent_tx, intent_rx) = smol::channel::unbounded::<Vec<LaunchIntent>>();
    for intent in startup_intents {
        let _ = intent_tx.try_send(vec![intent]);
    }
    // watch 回调在去抖线程：只做「argv → 意图」纯计算 + 投递，立即返回。
    // 仅主界面模式挂载：直接模式不参与队列语义（见 `launch_mode`）。
    let _forward_watch = (mode == UiMode::Main)
        .then(|| {
            let tx = intent_tx.clone();
            let cache = password_cache.clone();
            watch_forward_queue(move |records| {
                let intents: Vec<LaunchIntent> = records
                    .iter()
                    .flat_map(|argv| intents_of_argv(argv))
                    .collect();
                if !intents.is_empty() {
                    // 用户口径 2026-09-29：资源管理器右键菜单（二实例转发）触发的
                    // 加解密，与「hzip-gui 重新打开」同语义 → 先清空会话密码缓存。
                    // 缓存只服务界面内的连续操作，不该被外部触发的一次加解密复用。
                    cache.clear();
                    let _ = tx.try_send(intents);
                }
            })
        })
        .flatten();

    // —— macOS Finder 扩展意图轨（appex / Services → 宿主）——
    //
    // appex 与 Services provider 在**沙盒**内运行：既读不到宿主私有配置，也无法
    // 向宿主注入 argv（均真机实证），只能把 `{key, paths}` 追加进 App Group 容器
    // 的 `intents.jsonl`。宿主在此消费它——与启动意图、二实例转发**共用同一条**
    // 意图队列，故三种入口的行为天然一致。仅 macOS、仅主界面模式（直接模式是
    // 独立进程，不消费队列）。
    //
    // 两半场都要：① 启动时先 drain 一次（覆盖「宿主启动前 appex 已写入」的窗口）；
    // ② 运行期 watch 队列文件（覆盖「宿主已运行时 appex 写入」——appex 无法唤醒
    //    一个已运行实例，只能靠宿主自己监听）。缺 ② 则运行期点击是死信
    //    （与 `hzip-forward-queue` 的历史缺陷同源）。
    #[cfg(target_os = "macos")]
    let _extension_watch = (mode == UiMode::Main)
        .then(|| {
            use hzip_platform::context_menu::backend;
            // ① 启动窗口：先消费一次。
            let initial = intents_of_extension(&backend().take_extension_intents());
            if !initial.is_empty() {
                let _ = intent_tx.try_send(initial);
            }
            // ② 运行期：监听队列文件（容器目录可能尚不存在——watch 内部监听父目录
            //    +路径过滤，故先建父目录与空文件）。
            let tx = intent_tx.clone();
            let cache = password_cache.clone();
            hzip_platform::context_menu::watch_extension_intents(move |_| {
                let intents = intents_of_extension(&backend().take_extension_intents());
                if !intents.is_empty() {
                    cache.clear();
                    let _ = tx.try_send(intents);
                }
            })
        })
        .flatten();

    // —— macOS Services 轨：注册 provider（让「服务」子菜单项真正可点）——
    //
    // Info.plist 声明 NSServices 只让条目出现；点击时系统调用的 selector 必须在
    // 宿主进程内注册（NSRegisterServicesProvider，NSPortName = "hzip"）。放在
    // NSApplication 起来之前注册会静默无效，故晚于此处的 app 构造之后。
    #[cfg(target_os = "macos")]
    if mode == UiMode::Main {
        hzip_platform::context_menu::register_services_provider();
    }

    // —— 临时缓冲自愈（启动一次）——
    //
    // 组合档（`.cpio.gz` 等）打开/解压要把解压后的内层容器落临时文件（cpio/tar
    // 要 seek）。`NamedTempFile` 只在 drop 时删：进程被强杀/崩溃会留下**孤儿**，
    // 实测一次中断累积 29.6 GiB、把系统盘挤到 3 GiB 可用，直接导致用户打开
    // 940 MB `.cpio.gz` 时 ENOSPC 失败。启动扫一遍（毫秒级 `read_dir`）是唯一的
    // 自愈时机；阈值 24 h，不会误删在用的缓冲（见 `services::space`）。
    hzip_core::services::space::sweep_stale_spools(hzip_core::services::space::SPOOL_STALE_AFTER);

    // —— Engine + TempArea + 配置 ——
    let engine = Arc::new(Engine::new(EngineConfig::default()).map_err(|e| e.to_string())?);
    // TempArea 交给每个窗口的 AppData 共享持有：嵌套打开（双击容器）要把内层
    // 档案解出来当会话源，临时文件活得和会话一样久。退出时 `cleanup` 清场。
    let temp_area = TempArea::new_session(None).ok().map(Arc::new);
    let config = hzip_config::Config::load();

    // —— 右键菜单能力集保鲜 ——
    //
    // `shell-menu.json` 的 `archive_exts` 决定 DLL 在某个文件上是否显示解压组，
    // 它由**引擎能力**派生（不是用户选项），却只随「应用到系统」写入——升级后
    // 新增格式（rar / tgz 别名等）时，已装的菜单仍按旧清单把 `.rar` 当非档案，
    // 表现为「右键里没有解压项」（实测缺陷）。启动时补一次：只替换该字段，
    // 不动 `items`（用户勾选，归「应用」按钮）。未安装 = no-op。
    //
    // 同步调用：与 `Config::load` 同为启动期一次性小文件读写（毫秒级、免管理员），
    // 事件循环尚未开始，不存在 UI 卡顿问题。
    {
        use hzip_platform::context_menu::backend;
        let exts = crate::ui::contracts::supported_open_extensions(&engine);
        match backend().refresh_capabilities(&exts) {
            Ok(true) => {
                tracing::info!(exts = exts.len(), "右键菜单能力集已刷新（archive_exts）")
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "右键菜单能力集刷新失败"),
        }
    }

    // —— 主窗口注册表（T-MENU10：多窗口独立 AppData + 按窗口信号路由）——
    let registry = crate::ui::shell::windows::WindowRegistry::new();

    // —— 每个窗口独立 AppData + 独立 SignalTx（设计 §5.1 裁定 2）——
    //
    // 共享：`Engine`（线程池只建一次）、配置（每次 load 读盘）、注册表；
    // 独立：浏览态 / 任务路由 / 设置投影 / 信号通道。
    //
    // `make_data` 返回 `(AppData, SignalRx)`：rx 交给该窗口的 forwarder，
    // 于是「哪个窗口发的信号回到哪个窗口」由通道归属回答（不需要 window_id
    // 载荷——见 windows.rs 模块文档）。
    let make_data = {
        let engine = engine.clone();
        let config = config.clone();
        let password_cache = password_cache.clone();
        move || -> (
            AppDataHandle,
            Arc<crate::ui::contracts::SignalTx>,
            crate::ui::contracts::SignalRx,
        ) {
            let (tx, rx): (
                crate::ui::contracts::SignalTx,
                crate::ui::contracts::SignalRx,
            ) = smol::channel::bounded(64);
            let tx = Arc::new(tx);
            let data = Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
                engine.clone(),
            )));
            if let Ok(mut d) = data.lock() {
                d.signal_tx = Some(tx.clone());
                d.temp_area = temp_area.clone();
                // 会话密码缓存（进程级一份，多窗口共享）：用户口径「程序没关就
                // 继续用上一次的密码」
                d.share_password_cache(password_cache.clone());
                // 设置偏好恢复（R-GUI2：show_dotdot/tree_width/show_folder_size 等
                // config [ui] 键经 SettingsPrefs 生效；T-G30 装配段兑现）
                d.load_settings(&config);
            }
            (data, tx, rx)
        }
    };
    // 首个窗口（主窗口/直接模式的意图宿主）：primary 数据面。
    let (data, tx, rx) = make_data();

    // 失败原因文案 locale（02 §12.2）：跟随 [ui].lang；system → zh-CN（中文
    // 产品默认文案语言；en 用户显式选择后生效）
    crate::ui::views::progress::init_locale(&config.ui.lang);

    // —— UI forwarder：SignalRx → 主线程泵（R-GUI1-02 §2.2）——
    //
    // 后台线程收到 VmSignal → 转 mpsc → 主线程 spawn 循环批量消费后
    // 派生快照刷 shell（Entity::update 不能从后台线程直呼——gpui 线程亲和）。
    // forwarder 闭包保持「立即返回」纪律（pump_host.rs 文档）。
    //
    // T-MENU10：通道**每窗口一条**（`ui_tx` 由各窗口 forwarder 克隆持有），
    // 泵不再依赖「唯一 shell 句柄」（T-MENU10 已删除旧 `shell_holder` 单例），
    // 改为按窗口注册表分发（`shell/windows.rs`）。
    // 弹窗宿主窗口句柄（R-GUI4 #1；2026-09-23 起提示改独立窗口，槽位仅留痕）
    let prompt_window: Arc<std::sync::Mutex<Option<gpui_kit::AnyWindowHandle>>> =
        Arc::new(std::sync::Mutex::new(None));
    // T-PERF1：通道用 smol（async recv）——泵才能「信号即醒」而非 100ms
    // 轮询。unbounded 与旧 std mpsc 同语义；forwarder 侧 try_send 永不阻塞。
    let (ui_tx, ui_rx): (
        smol::channel::Sender<crate::viewmodel::VmSignal>,
        smol::channel::Receiver<crate::viewmodel::VmSignal>,
    ) = smol::channel::unbounded();
    {
        let ui_tx_primary = ui_tx.clone();
        spawn_ui_forwarder(rx, move |sig| {
            // 立即返回纪律：仅转发；派生在主线程泵内做（不抢 AppData 锁）。
            let _ = ui_tx_primary.try_send(sig);
        });
    }

    // —— 机型识别 + 触控栏辅助功能安全网 ——
    //
    // 只读一次 `hw.model`：日志与逃生判定**共用同一份读数**——日志里报的
    // `touch_bar` 就是下面分流的输入，现场排查不会出现「日志说有、实际没走」
    // 的错位。判定规则、上游缺陷位置（gpui-pre-macos 0.3.8 window.rs:710/:2365，
    // accesskit_macos 0.26.x subclass.rs:203/:263）与 vendor 补丁的关系，见
    // `hzip_platform::machine` 模块文档。
    let machine_model = hzip_platform::machine::machine_model();
    let touch_bar = hzip_platform::machine::is_touch_bar_machine(machine_model.as_deref());
    let a11y_escape = touch_bar && hzip_platform::machine::a11y_fallback_requested();
    // 机型原值用于日志（`%` = Display：非 macOS / 读不到时是显式的 `<unknown>`，
    // 现场一眼能区分「不是触控栏机型」与「压根没读到」）。
    let model_str = machine_model.as_deref().unwrap_or("<unknown>");
    tracing::info!(
        model = %model_str,
        touch_bar,
        a11y_env = hzip_platform::machine::A11Y_ENV,
        a11y_escape,
        "机型识别（非 macOS 恒 <unknown>/false）"
    );

    // —— gpui Application + 主窗口 ——
    //
    // **默认**走 `application()`（正常 a11y）：主修复是 vendor 补丁
    // （不再用 accesskit 的 SubclassingAdapter）——绝不因为机型/变量而改变大
    // 多数人的运行路径。只有「触控栏机型 **且** HZIP_MACOS_A11Y=off」才切到
    // `new_inaccessible`：进程级关闭辅助功能，连 accesskit 的 Adapter 都不装，
    // 从源头规避关窗崩溃；代价是本会话没有辅助功能（屏幕阅读器不可用）。
    let app = if a11y_escape {
        tracing::warn!(
            model = %model_str,
            "HZIP_MACOS_A11Y=off：以 Application::new_inaccessible 启动（本会话无辅助功能）"
        );
        gpui_kit::Application::new_inaccessible(gpui_kit::platform::current_platform(false))
    } else {
        gpui_kit::application()
    };
    let app = app.with_assets(crate::ui::assets::ChainedAssets);
    // macOS「打开方式」不走 argv：LaunchServices 以 Apple Event
    // （application:openURLs:）投递待打开文件（见 app::open_event）。
    // 不注册它就只剩 argv——实测症状是窗口起来、压缩包没打开（日志
    // open=[]）。回调是**同步**的（gpui 在主线程事件循环里直接调用），
    // 且可能早于/晚于窗口创建：这里只做纯转换 + 投意图队列，与启动
    // 意图、二实例转发共用同一条消费路径，故两种时序都成立。
    app.on_open_urls({
        let tx = intent_tx.clone();
        move |urls| {
            let paths = crate::app::open_event::paths_from_urls(&urls);
            if paths.is_empty() {
                tracing::warn!(urls = ?urls, "打开事件没有可用的本地路径");
                return;
            }
            tracing::info!(paths = ?paths, "收到 macOS 打开事件（Apple Event）");
            let intents: Vec<LaunchIntent> = paths
                .into_iter()
                .map(|p| LaunchIntent::Open { path: p.into() })
                .collect();
            let _ = tx.try_send(intents);
        }
    });
    app.run(move |cx| {
        // Windows 系统通知要求 AppUserModelID（gpui SystemNotification
        // 平台要求；启动早期设置一次——ui::notify 系统投递的前提）
        cx.set_app_identity("fun.hzip.hzip", "hzip");
        gpui_kit::component::init(cx); // Theme/menu 全局注册（内含 menu::init 键绑定）
        crate::ui::theme::init(cx);
        // 缺中文字体 → 界面回退英文（用户口径 2026-10-12）：必须跑在**开窗之前**，
        // 否则用户先白看一屏豆腐块。探测用界面当前字体整形探针字形，见 `ui::fonts`。
        crate::ui::fonts::apply_cjk_guard(cx, &config.ui.lang);
        // 文件类型图标（T-ICON）装配：建缓存 Global + 启后台探测线程，幂等。
        // `tx` 即首个 shell 的 `signal_tx`（`make_data` 把它同时写进
        // `AppData.signal_tx`）——图标填充信号经它回投主线程泵，泵只重绘。
        crate::ui::widgets::file_icon::init(cx, Some(tx.clone()));
        // 布局密度（`[ui].density` → 进程级间距刻度）。启动装配一次：
        // 之后由泵在 `SettingApplied(Density)` 时改写（见 apply_ui_effects）
        crate::ui::spacing::init_from_config();
        // 菜单清单注册（T-GUI T21：窗口打开前写入 GlobalState；
        // AppMenuBar::reload 从那里装配；has_archive 初值 false）
        crate::ui::bar::menu_bar::register_menus(
            &crate::ui::bar::menu_bar::MenuContext::closed(),
            cx,
        );
        cx.set_quit_mode(gpui_kit::QuitMode::LastWindowClosed);
        // 菜单加速键（设计 §1.6；`Main` = AppShell 的 key_context）。
        // 菜单项的快捷键提示由 kit 按 `Kbd::binding_for_action` 反查本表
        // （`PopupMenu::render_key_binding`）——没有绑定就不显示。
        {
            use crate::ui::shell as sh;
            cx.bind_keys([
                gpui_kit::KeyBinding::new("ctrl-o", sh::Open, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-e", sh::Extract, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-shift-n", sh::NewWindow, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-a", sh::SelectAll, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-i", sh::SelectInvert, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-c", sh::CopyFiles, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-v", sh::Paste, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-f", sh::FindFile, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-t", sh::TestIntegrity, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-,", sh::Settings, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-j", sh::ShowJobs, Some("Main")),
                gpui_kit::KeyBinding::new("f5", sh::RefreshView, Some("Main")),
                gpui_kit::KeyBinding::new("delete", sh::DeleteFiles, Some("Main")),
                gpui_kit::KeyBinding::new("ctrl-q", sh::Quit, Some("Main")),
            ]);
        }
        let on_closed = {
            let data = data.clone();
            let registry = registry.clone();
            move |cx: &mut gpui_kit::App, window_id: gpui_kit::WindowId| {
                // T-MENU10 退出语义：关单窗口 ≠ 退进程。
                // 注册表注销该窗口；**全部窗口关**（注册表 + gpui 窗口集）
                // 才走既有退出链（02 §5.3.9 优雅退出）。
                let remaining = registry.unregister_window_id(window_id);
                let live = cx.windows().len();
                if crate::ui::shell::windows::all_windows_closed(remaining, live) {
                    if let Ok(d) = data.lock() {
                        exit::graceful_shutdown(&d.engine, &d.jobs, None);
                    }
                } else {
                    tracing::debug!(remaining, live, "仍有窗口存活，退进程延后");
                }
            }
        };
        let _closed_sub = cx.on_window_closed(on_closed);

        // 启动意图消费（02 §11.5 + R-GUI2 §2.6）：
        // - 全部意图先经队列转发（单实例语义；二实例 argv 已并入 argv_open）
        // - Open：后台打开 → browse（回投 EntriesChanged 刷新）
        // - ExtractHere/AutoExtract/ExtractDir/ExtractTo：打开档案 →
        //   submit_extract_locked（config [extract] 默认；dest 语义各异）
        // - ExtractTo：目录选择器（注册表 verb / 工具栏同款 smol::unblock）
        // 意图消费者：**启动意图与运行期转发意图共用**这条执行路径。
        // 二者都必须走这里——否则「GUI 已运行时点右键菜单项」会退化成
        // 死信（历史缺陷：转发队列无人消费，点击毫无反应）。
        cx.spawn({
            let data_consumer = data.clone();
            let tx_consumer = tx.clone();
            let registry_consumer = registry.clone();
            async move |cx| {
                while let Ok(batch) = intent_rx.recv().await {
                    // —— 目标窗口解析（每批一次）——
                    //
                    // 主界面模式下转发意图**不得**固定发给「第一个窗口」：它被
                    // 关掉后（多窗口：开窗 2、关窗 1）档案会被装进没人渲染的
                    // 数据面、信号也投进已注销通道——症状正是最忌讳的「双击了
                    // 却什么都没出现」。改为取**存活窗口里 id 最小**的那个（注册
                    // 表在 `on_window_closed` 里同步注销，快照即实况；主线程上
                    // 读，无竞态）。一个窗口都不剩（正走退出链）才回落 primary
                    // 数据面并告警——那时进程本就在收尾。
                    //
                    // 直接模式无主窗口（窗口都是按需开的）：一直用 primary 数据面
                    // ——它就是这个进程的意图宿主。
                    let target: Option<(AppDataHandle, Arc<crate::ui::contracts::SignalTx>)> =
                        if mode == UiMode::Main {
                            registry_consumer.snapshot().into_iter().next().and_then(
                                |(_, _, data)| {
                                    let tx = data.lock().ok().and_then(|d| d.signal_tx.clone())?;
                                    Some((data, tx))
                                },
                            )
                        } else {
                            None
                        };
                    if mode == UiMode::Main && target.is_none() {
                        tracing::warn!("转发意图到达时已无存活窗口，回落 primary 数据面");
                    }
                    let (batch_data, batch_tx) =
                        target.unwrap_or_else(|| (data_consumer.clone(), tx_consumer.clone()));
                    for intent in batch {
                        let d = batch_data.clone();
                        let tx2 = batch_tx.clone();
                        cx.spawn(async move |cx| {
                            let res: Result<Option<u64>, String> = match &intent {
                                LaunchIntent::Open { path } => {
                                    // 原实现在此处直呼 `d.main.open`（同步
                                    // browse）——意图消费者跑在主线程 ⇒ 打开大
                                    // 档案时整个界面冻住。改走统一入口：后台
                                    // browse（不卡界面）+ 按需「正在打开」窗。
                                    // 默认不展开（R-GUI2 #5）语义不变。
                                    let task = cx.update(|cx| {
                                        // Explorer 双击/右键「打开」入口：与内部
                                        // 入口同一条路——成功即记最近打开
                                        crate::ui::shell::open_archive_task(
                                            cx,
                                            d.clone(),
                                            path.clone(),
                                            true,
                                        )
                                    });
                                    task.await.map(|_| None)
                                }
                                LaunchIntent::ExtractHere { path } => {
                                    // dest = 源档案所在目录。
                                    // **后台线程**：直接模式要先打开档案（同步
                                    // browse）再提交，而本任务跑在 UI 线程的前台
                                    // 执行器上——大档案会把界面冻住（用户口径
                                    // 2026-09-24：打开类重活一律离 UI 线程）。
                                    let d2 = d.clone();
                                    let path2 = path.clone();
                                    cx.background_executor()
                                        .spawn(async move {
                                            smol::unblock(move || {
                                                open_then_extract(
                                                    &d2,
                                                    &path2,
                                                    |archive| {
                                                        archive
                                                            .parent()
                                                            .map(|p| p.to_path_buf())
                                                            .ok_or_else(|| {
                                                                "档案无父目录".to_string()
                                                            })
                                                    },
                                                    None,
                                                )
                                            })
                                            .await
                                        })
                                        .await
                                        .map(Some)
                                }
                                LaunchIntent::AutoExtract { path } => {
                                    // smart 强制 true（覆盖 config）；后台线程同
                                    // ExtractHere（打开重活不得占 UI 线程）
                                    let d2 = d.clone();
                                    let path2 = path.clone();
                                    cx.background_executor()
                                        .spawn(async move {
                                            smol::unblock(move || {
                                                open_then_extract(
                                                    &d2,
                                                    &path2,
                                                    |archive| {
                                                        archive
                                                            .parent()
                                                            .map(|p| p.to_path_buf())
                                                            .ok_or_else(|| {
                                                                "档案无父目录".to_string()
                                                            })
                                                    },
                                                    // 「智能解压」承诺的就是 smart
                                                    // 生效，覆盖配置（历史上只写
                                                    // 注释没实现）
                                                    Some(true),
                                                )
                                            })
                                            .await
                                        })
                                        .await
                                        .inspect(|&job_id| {
                                            tracing::info!(
                                                "--auto-extract 任务 {job_id}（smart=true）"
                                            );
                                        })
                                        .map(Some)
                                }
                                LaunchIntent::ExtractDir { path } => {
                                    // dest = 源目录/「压缩文件名」（去扩展名）；
                                    // 后台线程同 ExtractHere
                                    let d2 = d.clone();
                                    let path2 = path.clone();
                                    cx.background_executor()
                                        .spawn(async move {
                                            smol::unblock(move || {
                                                open_then_extract(
                                                    &d2,
                                                    &path2,
                                                    |archive| {
                                                        let stem = archive
                                                            .file_stem()
                                                            .map(|s| {
                                                                s.to_string_lossy().into_owned()
                                                            })
                                                            .unwrap_or_else(|| "archive".into());
                                                        archive
                                                            .parent()
                                                            .map(|p| p.join(stem))
                                                            .ok_or_else(|| {
                                                                "档案无父目录".to_string()
                                                            })
                                                    },
                                                    None,
                                                )
                                            })
                                            .await
                                        })
                                        .await
                                        .map(Some)
                                }
                                LaunchIntent::ExtractTo { path } => {
                                    // 目录选择器（smol::unblock 离 UI 线程）
                                    let picked = cx
                                        .background_executor()
                                        .spawn(async {
                                            smol::unblock(crate::app::file_picker::pick_directory)
                                                .await
                                        })
                                        .await;
                                    let Some(dest) = picked else {
                                        return; // 取消：静默
                                    };
                                    // 后台线程：同 ExtractHere（打开重活在锁外/线程外）
                                    let d2 = d.clone();
                                    let path2 = path.clone();
                                    cx.background_executor()
                                        .spawn(async move {
                                            smol::unblock(move || {
                                                open_then_extract_with_dest(&d2, &path2, dest, None)
                                            })
                                            .await
                                        })
                                        .await
                                        .map(Some)
                                }
                                LaunchIntent::ExtractOptions { archives } => {
                                    // 参数窗：填参数 → 开始 → 关本窗 → 输出窗
                                    //（提交在参数窗内走同一提交点）
                                    let archives = archives.clone();
                                    cx.update(|cx| {
                                        crate::ui::views::op_window::open_extract_params(
                                            cx, &d, archives,
                                        );
                                    });
                                    Ok(None)
                                }
                                LaunchIntent::Compress {
                                    paths,
                                    choose_output,
                                    format,
                                } => {
                                    use crate::viewmodel::dialog_vm::{
                                        COMPRESS_FORMATS, default_out_path, unique_out_path,
                                    };
                                    let cfg = hzip_config::Config::load();
                                    let want = format
                                        .clone()
                                        .unwrap_or_else(|| cfg.defaults.format.clone());
                                    // 「添加到压缩文件…」= 参数窗（格式/级别/分卷/
                                    // 排除/输出都在窗内定，与 7-Zip 的 Add to Archive…
                                    // 同形态）；点「开始」后由参数窗提交并开输出窗。
                                    if *choose_output {
                                        let inputs = paths.clone();
                                        cx.update(|cx| {
                                            crate::ui::views::op_window::open_compress_params(
                                                cx, &d, inputs,
                                            );
                                        });
                                        return;
                                    }
                                    // 固定格式：输出按源名派生（重名追加序号，不覆盖）
                                    let out = unique_out_path(&default_out_path(paths, &want));
                                    // 用户在保存框里可能改了扩展名（.7z/.tar…）：
                                    // 以实际扩展名为准，白名单外回落配置缺省格式
                                    let fmt = out
                                        .extension()
                                        .map(|e| e.to_string_lossy().to_ascii_lowercase())
                                        .filter(|e| COMPRESS_FORMATS.contains(&e.as_str()))
                                        .unwrap_or(want);
                                    // 提交走**后台线程**（用户口径 2026-09-24）：
                                    // `submit_compress_locked` 要读配置 + 建 spec +
                                    // `create_dir_all` + 入队，且本分支下面没有 await
                                    // ——在 UI 线程上直跑就是同步阻塞（大输入集更明显）。
                                    let d_submit = d.clone();
                                    let (paths_submit, out_submit, fmt_submit) =
                                        (paths.clone(), out.clone(), fmt.clone());
                                    let submitted = cx
                                        .background_executor()
                                        .spawn(async move {
                                            smol::unblock(move || {
                                                submit_compress_locked(
                                                    &d_submit,
                                                    &paths_submit,
                                                    &out_submit,
                                                    &fmt_submit,
                                                )
                                            })
                                            .await
                                        })
                                        .await;
                                    if let Ok(id) = &submitted {
                                        tracing::info!(
                                            "压缩任务 {id}（{} 项 → {}）",
                                            paths.len(),
                                            out.display()
                                        );
                                    }
                                    submitted.map(Some)
                                }
                            };
                            let job_id = res.as_ref().ok().and_then(|j| *j);
                            if let Some(job_id) = job_id {
                                // T-PW5：提交点接线——argv 解压意图开进度窗口。
                                // 直接模式**强制**显示（无主界面兜底，且用户点右键
                                // 的预期就是「看到它在干活」）。
                                // 解压意图（除 Open/Compress 外全为解压族）改
                                // **挂起**（设计稿 11 §4.2 密码门时序：密码已答
                                // 的首个事件由泵开窗）；压缩/浏览照旧。
                                if !matches!(
                                    intent,
                                    LaunchIntent::Open { .. } | LaunchIntent::Compress { .. }
                                ) && mode != UiMode::Direct
                                {
                                    // 有主界面：进度窗**挂起**到密码应答后的
                                    // 首个非 JobStarted 事件（主窗在场，提示不会
                                    // 无处可去）。
                                    if let Ok(app) = d.lock()
                                        && let Ok(mut j) = app.jobs.lock()
                                    {
                                        j.register_progress_open(job_id, false);
                                    }
                                } else {
                                    // 直启模式（右键「智能解压」等）：**立即开**。
                                    // 该模式没有主窗口，进度窗是唯一常驻窗口——
                                    // 若也挂起，等待密码期间进程零窗口：应答后
                                    // 提示窗一关就再没有窗口托管，用户既看不到
                                    // 进度也看不到冲突提示（2026-09-23 实测）。
                                    // 提示已独立成窗，天然浮在进度窗之上，无需
                                    // 再靠「挂起」保证层级。
                                    cx.update(|cx| {
                                        super::open_progress_window_with(
                                            cx,
                                            &d,
                                            job_id,
                                            mode == UiMode::Direct,
                                        );
                                    });
                                }
                            }
                            if let Err(e) = &res {
                                tracing::error!("启动意图 {:?} 失败: {e}", intent.kind());
                                if mode == UiMode::Direct {
                                    // 直接模式无主界面：失败必须有出口，否则用户
                                    // 看到的就是「点了没反应」（本模式最忌讳的形态）
                                    let msg =
                                        crate::tr_with("dlg.params.failed", &[("err", e.as_str())]);
                                    cx.update(|cx| {
                                        crate::ui::views::op_window::open_message_window(
                                            cx,
                                            crate::viewmodel::message::MessageDialog::error(
                                                crate::tr("dlg.error.title"),
                                                msg,
                                            ),
                                        );
                                    });
                                }
                            }
                            let _ = tx2.try_send(match &res {
                                Ok(_) => crate::viewmodel::VmSignal::EntriesChanged,
                                Err(_) => crate::viewmodel::VmSignal::DialogChanged,
                            });
                        })
                        .detach();
                    }
                }
                // 直接模式兜底：意图跑完却一个窗口都没有（参数/任务都没成），
                // 绝不能挂着一个没有窗口的进程——那正是「点了没反应」的另一种形态。
                let nothing_to_show = mode == UiMode::Direct
                    && cx.update(|cx: &mut gpui_kit::App| cx.windows().is_empty());
                if nothing_to_show {
                    tracing::warn!("直接模式：无可显示窗口，退出");
                    cx.update(|cx: &mut gpui_kit::App| cx.quit());
                }
            }
        })
        .detach();

        // —— 主线程泵（R-GUI1-02 §2.2）：**两种模式都要跑** ——
        //
        // 它负责消费信号 → 派生快照 → ①进度窗灌行 ②密码/冲突弹窗
        // 生命周期 ③（有主界面时）刷新 shell。
        //
        // T-PERF1 事件驱动：信号到达立即唤醒（`or(recv, Timer)`）——交互
        // 反馈不再等 100ms 轮询；100ms 兜底 tick 保留（任务状态回填
        // `sync_states_from_core` 每 wake 都跑，至少与旧轮询一样新鲜）。
        //
        // 位置很关键：最初它写在主窗口的 builder 里，于是**直接模式没有泵**
        // ——现象是「进度窗一直停在队列中（文件其实已解出）」，且密码/冲突
        // 请求永远无人应答。任何「只服务主界面」的直觉在这里都是错的：
        // 进度窗与弹窗都是无主界面场景的一等公民。
        let data_for_pump = data.clone();
        let tx_for_pump_root = tx.clone();
        {
            {
                let data = data_for_pump.clone();
                let registry = registry.clone();
                // 提示窗应答回调（密码提交 / 冲突决策 → router + 信号回投）
                let prompt_on_event: std::rc::Rc<dyn Fn(crate::ui::contracts::DialogEvent)> = {
                    let data_p = data.clone();
                    let tx_p = tx_for_pump_root.clone();
                    std::rc::Rc::new(move |event| {
                        // 门（无任务态）与任务态应答同一入口：门的密码提交必须
                        // 在这里被消费（旧实现只在设置面板闭包里截，提示窗提交
                        // 门密码无人处理——用户口径 2026-09-28 修复）。
                        crate::ui::shell::prompt::handle_dialog_event(&data_p, &event);
                        let _ = tx_p.try_send(crate::viewmodel::VmSignal::DialogChanged);
                        let _ = tx_p.try_send(crate::viewmodel::VmSignal::JobsChanged);
                    })
                };
                cx.spawn(async move |cx| {
                    // 通道关闭（forwarder 全退——实际随进程存活）后防忙转：
                    // 退化为纯兜底 tick
                    let mut ui_rx_closed = false;
                    loop {
                        // 等待（T-PERF1）：首条信号即醒；否则 100ms 兜底 tick。
                        // `or` 先完成者胜、另一方取消（Timer 丢弃即重置）。
                        let first: Option<
                            Result<crate::viewmodel::VmSignal, smol::channel::RecvError>,
                        > = if ui_rx_closed {
                            smol::Timer::after(std::time::Duration::from_millis(100)).await;
                            None
                        } else {
                            smol::future::or(async { Some(ui_rx.recv().await) }, async {
                                smol::Timer::after(std::time::Duration::from_millis(100)).await;
                                None
                            })
                            .await
                        };
                        if first.as_ref().is_some_and(|r| r.is_err()) {
                            ui_rx_closed = true;
                        }
                        // 批处理是 UI 线程上最重的同步块之一（行重建 + 快照导出 +
                        // 窗口投影）：看门狗盯住它（用户口径 2026-09-24）
                        let _lag = crate::app::ui_lag::UiLag::new("泵批处理(行重建+快照)");
                        let mut batch: Vec<crate::viewmodel::VmSignal> = Vec::new();
                        if let Some(Ok(sig)) = first {
                            batch.push(sig);
                        }
                        while let Ok(sig) = ui_rx.try_recv() {
                            batch.push(sig);
                        }
                        // 退出请求（R-GUI4 #3：全部任务完成 + exit_on_finish
                        // 开——泵线程回投；主线程消费执行优雅退出）
                        let quit_requested = batch
                            .iter()
                            .any(|sig| matches!(sig, crate::viewmodel::VmSignal::QuitRequested));
                        // core 权威状态回填（每 tick，含空闲 tick）：事件流对
                        // 晚订阅者会丢 `JobStarted`，行状态只靠事件会停在 Queued
                        // （流式归档读索引数秒 → 界面谎报「等待开始」）。
                        let state_changed = data
                            .lock()
                            .map(|mut d| {
                                let changed = d
                                    .jobs
                                    .lock()
                                    .map(|mut j| j.sync_states_from_core())
                                    .unwrap_or(false);
                                // 任务成功后密码进会话缓存（用户口径 2026-09-29：
                                // 「输入一次后缓存下来」；成功即证明密码正确）
                                d.commit_verified_job_passwords();
                                changed
                            })
                            .unwrap_or(false);
                        // 主线程专属半场（窗口线程亲和）：①用户通知投递
                        //（目标判定在 ui::notify 组件——主窗口在场 → 界面
                        // toast，否则系统通知）②进度窗开窗请求（编辑等
                        // 后台任务提交后的用户可见出口；强制显示——菜单
                        // 发起的操作用户预期看到过程，不受
                        // [jobs].progress_window 开关静默）。
                        for sig in &batch {
                            match sig {
                                crate::viewmodel::VmSignal::Notify(ev) => {
                                    let ev = ev.clone();
                                    let registry_n = registry.clone();
                                    cx.update(move |cx| {
                                        crate::ui::notify::dispatch_notify(cx, &registry_n, &ev);
                                    });
                                }
                                crate::viewmodel::VmSignal::OpenProgress(job_id) => {
                                    cx.update(|cx| {
                                        super::open_progress_window_with(cx, &data, *job_id, true);
                                    });
                                }
                                crate::viewmodel::VmSignal::ProgressWindowOpen(job_id, force) => {
                                    cx.update(|cx| {
                                        super::open_progress_window_with(
                                            cx, &data, *job_id, *force,
                                        );
                                    });
                                }
                                crate::viewmodel::VmSignal::ShowMessage(msg) => {
                                    let msg = msg.clone();
                                    cx.update(move |cx| {
                                        crate::ui::views::op_window::open_message_window(cx, msg);
                                    });
                                }
                                // 密码门放行：**无条件**把待执行动作重跑起来
                                // （用户口径 2026-09-28：密码窗是门，验证通过
                                // 不得拦断后续流程）。逐窗口试——只有持有该
                                // 动作的那个数据面会真正执行（`take` 语义幂等）。
                                crate::viewmodel::VmSignal::GateActionReady => {
                                    for (_, shell, _) in registry.snapshot() {
                                        shell.update(cx, |shell, cx| {
                                            shell.retry_pending_gate_action(cx);
                                        });
                                    }
                                }
                                // 文件类型图标已落缓存（T-ICON）：**只重绘**。
                                // 快照未变化（缓存不在快照里），故不能靠下面
                                // 批尾的 `update_from_state` 变化检测——直接标脏
                                // AppShell 重渲浏览视图，渲染帧即读到 `Ready`
                                // 位图。绝不重建行（`needs_browse_refresh` 已把
                                // 本信号排除在重建触发条件之外）。
                                crate::viewmodel::VmSignal::IconCacheFilled => {
                                    for (_, shell, _) in registry.snapshot() {
                                        shell.update(cx, |_, cx| cx.notify());
                                    }
                                }
                                _ => {}
                            }
                        }
                        if !batch.is_empty() || state_changed {
                            // T-MENU10：主线程批内——**按窗口**派生快照刷 shell。
                            //
                            // 共享半场（主题/语言/进度窗/设置面板）只跑一次：
                            // 它们是进程级全局态（Theme/locale/progress 窗口注册
                            // 表都是单例），与窗口数无关。窗口半场按注册表逐窗口
                            // 刷：每个窗口有自己的 AppData（浏览态/任务/设置投影）
                            // 与 shell 实体。
                            // 主窗口（primary）先刷——它是进度窗/弹窗宿主首选。
                            // 值在锁内取出（`theme_replay`/`lang_ix`/`job_rows` 必被赋值：
                            // 唯一赋值点在下方的 primary 锁内）。
                            let prompt_now: Option<crate::ui::contracts::UiDialogPrompt>;
                            let lang_ix;
                            let theme_replay;
                            let density_ix;
                            let file_icons_system;
                            let job_rows_all: std::sync::Arc<Vec<crate::viewmodel::JobRow>>;
                            // 锁外要执行的后台请求（见下方 spawn 调用）
                            let reopen_path: Option<std::path::PathBuf>;
                            let want_rows_rebuild: bool;
                            {
                                let Ok(mut d) = data.lock() else {
                                    continue;
                                };
                                // 浏览行只在有主界面时派生（直接模式无浏览视图）。
                                // T-PERF1 信号分级：会话保活（编辑重开兜底）
                                // 每 wake 必跑（便宜）；行重建只在
                                // EntriesChanged/SettingApplied——任务进度
                                // tick 不再重做 O(行集) 重建。
                                let has_shell = !registry.is_empty();
                                // 会话保活（审计 B3）与行重建（B5）都只**请求**：
                                // 真活由后台线程干（旧实现在这里 UI 线程 + 持锁跑
                                // 完整 browse / O(行集) 派生）。spawn 必须放在
                                // **锁外**（下面 `drop(d)` 之后）——那两个函数自身
                                // 要取 AppData 锁，此处持锁调用会自锁。
                                reopen_path = if has_shell {
                                    d.browse_session_reopen_request()
                                } else {
                                    None
                                };
                                want_rows_rebuild = has_shell && needs_browse_refresh(&batch);
                                let snap = d.export_snapshot();
                                prompt_now = snap.dialog.clone();
                                lang_ix =
                                    batch_requested(&batch, crate::viewmodel::SettingId::Lang)
                                        .then_some(d.settings.lang_ix);
                                // 主题重放（模式/色/字体族/字号/圆角）：值一律取
                                // **内存镜像** `d.settings`——旧实现让重放内部
                                // `Config::load()` 读盘，而设置落盘是异步的 ⇒ 每次
                                // 都拿到上一拍的值（用户实报：点一次不生效、再点
                                // 一次才生效）。详见 [`theme_replay_of`]。
                                theme_replay = theme_replay_of(&batch, &d.settings);
                                density_ix =
                                    batch_requested(&batch, crate::viewmodel::SettingId::Density)
                                        .then_some(d.settings.density_ix);
                                // 文件类型图标开关（T-ICON）：批内出现本项才动——
                                // 值取**内存镜像**下标（0 = system、1 = builtin），
                                // 与 density 同款（勿读盘：落盘是异步的）。
                                file_icons_system = batch_requested(
                                    &batch,
                                    crate::viewmodel::SettingId::FileTypeIcons,
                                )
                                .then_some(d.settings.file_type_icons_ix == 0);
                                job_rows_all = snap.job_rows.clone();
                                drop(d);
                                // 锁外：起后台线程干真活（见上「只请求」注释）
                                if let Some(path) = reopen_path {
                                    super::spawn_browse_reopen(data.clone(), path);
                                }
                                if want_rows_rebuild {
                                    super::spawn_rows_rebuild(data.clone());
                                }
                                // primary 窗口快照（直接模式无 shell → 只跑半场）
                                if let Some((_, shell, _)) = registry.snapshot().into_iter().next()
                                {
                                    let mru_bump = batch.iter().any(|sig| {
                                        matches!(sig, crate::viewmodel::VmSignal::MenuEpochBump)
                                    });
                                    // T-PERF3：配置缓存失效（设置落盘/菜单
                                    // 数据变化 → 各 shell 重读一次，渲染帧
                                    // 读缓存零磁盘 I/O）
                                    let cfg_stale = mru_bump
                                        || batch.iter().any(|sig| {
                                            matches!(
                                                sig,
                                                crate::viewmodel::VmSignal::SettingApplied(_)
                                            )
                                        });
                                    shell.update(cx, |shell, cx| {
                                        if mru_bump {
                                            shell.state.update(cx, |s, cx| {
                                                s.menu_epoch = s.menu_epoch.wrapping_add(1);
                                                cx.notify();
                                            });
                                        }
                                        shell.update_from_state(cx, &snap);
                                        if cfg_stale {
                                            shell.refresh_cfg(cx);
                                        }
                                        // 密码门验证通过 → 重跑原操作（用户口径
                                        // 2026-09-28；无待执行动作时零开销）
                                        shell.retry_pending_gate_action(cx);
                                    });
                                }
                            }
                            // 其余窗口（第 2..n 个）：各自的 AppData 快照。
                            // T-PERF1：行重建同款信号分级（旧实现在
                            // is_open 时无差别重建）；T-PERF3：配置缓存
                            // 同步失效
                            let cfg_stale_any = batch.iter().any(|sig| {
                                matches!(
                                    sig,
                                    crate::viewmodel::VmSignal::SettingApplied(_)
                                        | crate::viewmodel::VmSignal::MenuEpochBump
                                )
                            });
                            for (id, shell, win_data) in registry.snapshot() {
                                if id == 1 {
                                    continue; // primary 已刷（id 1 = 首窗口）
                                }
                                let Ok(d) = win_data.lock() else {
                                    continue;
                                };
                                // 审计 B5：非主窗口的行重建同样交后台（它的行集
                                // 也要 O(行集) 派生，不该压在 UI 线程上）
                                let want_rows = d.main.is_open() && needs_browse_refresh(&batch);
                                if want_rows {
                                    // 先放锁再 spawn（spawn 内部要取同一把锁）
                                    drop(d);
                                    super::spawn_rows_rebuild(win_data.clone());
                                    continue;
                                }
                                let snap = d.export_snapshot();
                                drop(d);
                                shell.update(cx, |shell, cx| {
                                    shell.update_from_state(cx, &snap);
                                    if cfg_stale_any {
                                        shell.refresh_cfg(cx);
                                    }
                                    // 密码门放行动作：各窗口各自的数据面（同上）
                                    shell.retry_pending_gate_action(cx);
                                });
                            }
                            // 共享半场（全局态；跑一次即可）
                            let settings_applied = batch.iter().any(|sig| {
                                matches!(sig, crate::viewmodel::VmSignal::SettingApplied(_))
                            });
                            cx.update(|cx| {
                                apply_ui_effects(
                                    cx,
                                    theme_replay,
                                    lang_ix,
                                    density_ix,
                                    &job_rows_all,
                                );
                                // 文件类型图标开关（T-ICON）：批内出现本项才动。
                                // 关闭 ⇒ 清缓存 + 停探测（重开无需预热：下一次渲染
                                // 重新登记）。重绘让本帧立刻换掉/换上系统位图。
                                if let Some(system) = file_icons_system {
                                    crate::ui::widgets::file_icon::set_enabled(cx, system);
                                    cx.refresh_windows();
                                }
                                crate::ui::views::progress::dispatch_rows(cx, &job_rows_all);
                                if settings_applied {
                                    crate::ui::views::settings_layout::reload_open_panel(cx);
                                }
                            });
                            // 请求-应答提示窗（密码/冲突）：独立 OS 窗口
                            // （2026-09-23 返工）。旧实现把提示长在「宿主窗口」
                            // 内的 Modal 上——直启模式无主窗、进度窗又没挂
                            // 对话框层，于是密码请求登记了却无人渲染（120 s
                            // 决策超时）；冲突弹窗还被独立进度窗压在下面。
                            // 现在每批信号无条件同步：开/更新/关由窗口模块
                            // 按身份自行去重，重试（身份不变、次数变）也会刷新。
                            cx.update(|cx: &mut gpui_kit::App| {
                                crate::ui::views::prompt_window::sync(
                                    cx,
                                    prompt_now.as_ref(),
                                    prompt_on_event.clone(),
                                );
                            });
                        }
                        // 进度窗延迟关窗到点检查（勾选「完成后退出」时终局
                        // 后停留 exit_delay_ms 再关）：独立于上面的批次门控
                        // ——终局后不再有行信号，必须每 tick 兜底驱动
                        // （10 Hz 精度对秒级停留足够；无排定项时零开销）。
                        cx.update(crate::ui::views::progress::tick_pending_closes);
                        // 「正在打开」窗同款：终局后不再有信号，延迟关窗靠兜底 tick
                        cx.update(crate::ui::views::open_progress::tick);
                        // 双重门（用户口径 2026-09-29 第 ⑧ 条）：主界面
                        // （非 Direct 模式）恒不退出——即便信号是从直接模式
                        // 语义残留/误投而来，也不得把程序带走。
                        let quit_requested = quit_requested
                            && crate::app::pump_host::ALLOW_QUIT_ON_FINISH
                                .load(std::sync::atomic::Ordering::Relaxed);
                        if batch
                            .iter()
                            .any(|s| matches!(s, crate::viewmodel::VmSignal::QuitRequested))
                            && !quit_requested
                        {
                            tracing::info!("QuitRequested 被忽略：主界面模式不得自动退出");
                        }
                        if quit_requested {
                            tracing::info!("QuitRequested：全部任务完成，优雅退出 hzip");
                            if let Ok(d) = data.lock() {
                                exit::graceful_shutdown(&d.engine, &d.jobs, None);
                            }
                            cx.update(|cx| cx.quit());
                            return; // 泵循环退出（进程随事件循环收尾）
                        }
                        // 等待在循环顶部（信号即醒 / 100ms 兜底 tick）
                    }
                })
                .detach();
            }
        }

        // —— 主窗口：**仅主界面模式** ——
        //
        // 直接模式不开主窗口：右键动作要的是它自己那件事，不是浏览器窗口。
        // 它的窗口（参数窗/进度窗/失败提示窗）由意图消费者按需打开。
        // T-MENU10 窗口工厂：多窗口共用同一装配（独立 AppData + 独立通道
        // + 注册表登记）；`spawn_main_window` 是新窗口 Action 与启动期共用
        // 的唯一开窗点。
        //
        // 闭包捕获：`make_data`（新窗口的独立数据面）/`registry`/`ui_tx`
        //（新窗口 forwarder 的回投通道）/`prompt_window`（弹窗宿主）。
        // 自引用槽：新窗口工厂需要在**已经构建好的 shell 上注入自己**，
        // 而闭包无法在定义处引用自身——故先占位、定义后回填（Rc 克隆廉价）。
        let spawn_slot: crate::ui::shell::windows::SpawnWindowSlot =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let spawn_main_window: std::rc::Rc<crate::ui::shell::windows::SpawnWindowFn> = {
            let registry = registry.clone();
            let prompt_window = prompt_window.clone();
            let ui_tx = ui_tx.clone();
            let spawn_slot = spawn_slot.clone();
            std::rc::Rc::new(move |cx, data_for_shell, tx_for_shell| {
                let _shell_window = cx.open_window(
                    gpui_kit::WindowOptions {
                        window_bounds: Some(gpui_kit::WindowBounds::Windowed(
                            gpui_kit::Bounds::centered(
                                None,
                                size(MAIN_WINDOW_W, MAIN_WINDOW_H),
                                cx,
                            ),
                        )),
                        ..gpui_kit::component::TitleBar::window_options()
                    },
                    |window, cx| {
                        // 根视图必须是 gpui_component::Root（R-GUI1-03 §2.1）：
                        // SettingsPanel/对话框/notification 的 open_dialog 链经
                        // window.root::<Root>() 找层——裸 AppShell 根会让面板
                        // 「看起来从没打开过」（root.rs 文档原文症状）。
                        // 浏览导航 handlers（R-GUI 走查 #2 统一语义：路径跳转事件
                        // = 进入目录 + 左树跟进定位——所有入口共用 enter_dir_handler）：
                        // - on_tree_click：左栏单击目录 / 右栏双击目录 → 进入 +
                        //   expand_to 补展开父链（只增不删；树选中由 browse_root
                        //   按 current_dir 变化同步 + 滚动到位）
                        // - on_tree_toggle：左栏双击目录 → 翻转该目录展开态
                        //   （browse_layout click_count 分流；树内建单击 toggle
                        //   已被 stop_propagation 压制）
                        // - on_row_activate：右栏双击文件 → 预览（既有 ShellState
                        //   dialog_open 驱动；目录分支为兜底，目录双击实走
                        //   on_tree_click——browse.rs TableEvent 分流）
                        let data_nav = data_for_shell.clone();
                        let enter_dir_handler = std::rc::Rc::new(
                            move |path: String, tx: Option<Arc<crate::ui::contracts::SignalTx>>| {
                                if let Ok(mut d) = data_nav.lock() {
                                    if path.is_empty() {
                                        d.main.go_root_dir();
                                    } else {
                                        d.main.enter_dir(&path);
                                        // 路径跳转事件统一处理：进入即在左树定位该目录
                                        // （父链展开；自身也展开——树选中/滚动由
                                        // browse_root 消费快照时同步）
                                        d.main.expand_to(&path);
                                    }
                                    d.selected_path = Some(path.clone());
                                }
                                if let Some(tx) = tx {
                                    let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
                                }
                            },
                        );
                        let data_tree = data_for_shell.clone();
                        let tx_tree = tx_for_shell.clone();
                        let handlers = ShellHandlers {
                            // `..` 行/面包屑段点击 → 跳到 seg 目录（R-GUI3 #5）
                            //
                            // 嵌套会话（R-GUI2-06）：面包屑横跨多层，`level > 0`
                            // 的段属于**外层会话**——先弹回那一层（弹出会连带删掉
                            // 被退出层的临时档案），再在层内跳 seg。
                            on_crumb: {
                                let enter = enter_dir_handler.clone();
                                let tx = tx_tree.clone();
                                let data_crumb = data_for_shell.clone();
                                Box::new(move |crumb: crate::ui::contracts::Crumb| {
                                    if crumb.level > 0
                                        && let Ok(mut d) = data_crumb.lock()
                                    {
                                        for _ in 0..crumb.level {
                                            if !d.pop_open_frame() {
                                                break;
                                            }
                                        }
                                    }
                                    enter(crumb.seg, Some(tx.clone()));
                                })
                            },
                            on_tree_click: {
                                let enter = enter_dir_handler.clone();
                                let tx = tx_tree.clone();
                                Box::new(move |path| {
                                    enter(path, Some(tx.clone()));
                                })
                            },
                            on_row_activate: {
                                let enter = enter_dir_handler.clone();
                                let tx = tx_tree.clone();
                                Box::new(move |path| {
                                    // 双击：目录/`..` → 进入（enter_dir_handler）；
                                    // 文件 → **定位**：进入其所在目录（current_dir
                                    // ← 父目录；expand_to 展开父链）+ 选中该文件
                                    // （面包屑/左树随 current_dir 跟随——通用导航
                                    // 语义，搜索结果行点击同样命中此槽）。
                                    let data = data_tree.clone();
                                    let is_dir = data
                                        .lock()
                                        .map(|d| {
                                            path.is_empty()
                                                || d.dir_rows
                                                    .iter()
                                                    .any(|row| row.path == path && row.is_dir)
                                        })
                                        .unwrap_or(false);
                                    if is_dir || path.is_empty() {
                                        // 兜底分支（目录双击实走 on_tree_click）：
                                        // 进入 + expand_to 由共用 enter_dir_handler 统一
                                        enter(path, Some(tx.clone()));
                                        return;
                                    }
                                    // 文件激活：定位到所在目录 + 单选该文件
                                    if let Ok(mut d) = data.lock() {
                                        let parent = match path.rfind('/') {
                                            Some(i) => path[..i].to_string(),
                                            None => String::new(),
                                        };
                                        d.main.enter_dir(&parent);
                                        d.main.expand_to(&parent);
                                        if let Some(id) = d
                                            .dir_rows
                                            .iter()
                                            .find(|row| row.path == path)
                                            .and_then(|row| row.entry_id)
                                        {
                                            d.main.select_only(id);
                                        }
                                        d.selected_path = Some(path);
                                    }
                                    let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
                                })
                            },
                            // 双击目录 → 翻转该目录展开态（R-GUI4-1：browse_layout
                            // click_count≥2 分流到此处——旧装配漏接此槽 = no-op，
                            // 双击无展开的根因）。VM expanded 是唯一事实源，翻转后
                            // 回投 EntriesChanged → 快照 nav_rows 变化 → set_items。
                            on_tree_toggle: {
                                let data_toggle = data_for_shell.clone();
                                let tx = tx_tree.clone();
                                Box::new(move |id: String| {
                                    // id 带层号前缀（`binding::tree_item_id`）：
                                    // 展开态要写回**该行所属会话**，嵌套展示下
                                    // 同名目录内外各有一份，不能一律写当前会话。
                                    if let Some((level, path)) =
                                        crate::ui::binding::split_tree_id(&id)
                                        && let Ok(mut d) = data_toggle.lock()
                                    {
                                        d.toggle_tree_expand(level, path);
                                    }
                                    let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
                                })
                            },
                            // 密码/冲突弹窗事件 → router 应答（R-GUI4 #1：解压
                            // decision-timeout 根因——UI 半场从未接线）
                            on_dialog_event: {
                                let data_evt = data_for_shell.clone();
                                let tx = tx_tree.clone();
                                Box::new(move |event: crate::ui::contracts::DialogEvent| {
                                    crate::ui::shell::prompt::handle_dialog_event(
                                        &data_evt, &event,
                                    );
                                    let _ = tx.try_send(crate::viewmodel::VmSignal::DialogChanged);
                                    let _ = tx.try_send(crate::viewmodel::VmSignal::JobsChanged);
                                })
                            },
                            // T-MENU1：过滤输入 → VM set_filter（名称子串；
                            // 空串 = 清除）→ 回投刷新信号重灌浏览行
                            on_filter_changed: {
                                let data_f = data_for_shell.clone();
                                let tx = tx_tree.clone();
                                Box::new(move |text: String| {
                                    if let Ok(mut d) = data_f.lock() {
                                        let sub = text.trim().to_string();
                                        d.main.set_filter(crate::viewmodel::Filter {
                                            name_substr: (!sub.is_empty()).then_some(sub),
                                            ..Default::default()
                                        });
                                    }
                                    let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
                                })
                            },
                            // T-TVS3：行选中上行（此前落 `..ShellHandlers::default()`
                            // no-op——单击选中从未到达 VM，解压选中/选中计数只反映
                            // 菜单全选/反选）。整组替换 VM 选择集 → 回投刷新
                            // （快照 selection/selection_count → icon/list 高亮）。
                            // T-SEL 走查 #1：双通道（实体 id + 隐式目录路径——
                            // tar/无目录条目档案的目录行按路径选）
                            on_selection_changed: {
                                let data_sel = data_for_shell.clone();
                                let tx = tx_tree.clone();
                                Box::new(move |up: crate::ui::contracts::SelectionUpdate| {
                                    let ids: Vec<usize> =
                                        up.ids.into_iter().map(|i| i as usize).collect();
                                    if let Ok(mut d) = data_sel.lock() {
                                        d.main.select_rows(&ids, &up.dirs);
                                    }
                                    let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
                                })
                            },
                            ..ShellHandlers::default()
                        };
                        let app_shell = AppShell::with_data(
                            cx,
                            handlers,
                            Some(data_for_shell.clone()),
                            Some(tx_for_shell.clone()),
                        );
                        let app_shell = cx.new(|_| app_shell);
                        // T-MENU10：把「新窗口」工厂注入 shell（多窗口入口）。
                        // 工厂需要新窗口的独立数据面与通道：`make_data` 每次调用
                        // 产出一套（Engine 共享、浏览态/通道独立）。
                        {
                            let make_data_inner = make_data.clone();
                            let ui_tx_inner = ui_tx.clone();
                            let spawn_slot_inner = spawn_slot.clone();
                            let factory: std::rc::Rc<crate::ui::shell::windows::NewWindowFn> =
                                std::rc::Rc::new(move |cx: &mut gpui_kit::App| {
                                    let (new_data, new_tx, new_rx) = make_data_inner();
                                    // 新窗口信号 → 同一主线程泵（通道归属即窗口归属）
                                    {
                                        let ui_tx = ui_tx_inner.clone();
                                        spawn_ui_forwarder(new_rx, move |sig| {
                                            let _ = ui_tx.try_send(sig);
                                        });
                                    }
                                    let f = spawn_slot_inner.borrow().clone();
                                    if let Some(f) = f {
                                        f(cx, new_data, new_tx);
                                    }
                                });
                            app_shell.update(cx, |shell, _| {
                                shell.set_new_window_factory(factory);
                            });
                        }
                        // T-MENU10：注册进窗口注册表（泵按表分发快照；
                        // 关闭时 on_window_closed 注销）。id 由注册表分配。
                        let handle = window.window_handle();
                        let id = registry.alloc_id();
                        registry.register(crate::ui::shell::windows::WindowEntry {
                            id,
                            shell: app_shell.clone(),
                            data: data_for_shell.clone(),
                            handle,
                        });
                        // 弹窗宿主窗口句柄（R-GUI4 #1：泵线程开密码/冲突弹窗用）
                        if let Ok(mut h) = prompt_window.lock() {
                            *h = Some(handle);
                        }
                        let root_view: gpui_kit::AnyView = app_shell.into();
                        cx.new(|cx| gpui_kit::component::Root::new(root_view, window, cx))
                    },
                );
                // 窗口主动激活：`--open` 由资源管理器拉起时，窗口可能被
                // 资源管理器窗口挡在后面（同进度窗的成因）。
                if let Ok(wh) = _shell_window
                    && let Err(e) = wh.update(cx, |_, window, _| window.activate_window())
                {
                    tracing::warn!("主窗口激活失败: {e}");
                }
            })
        };
        *spawn_slot.borrow_mut() = Some(spawn_main_window.clone());

        // 启动窗口（仅主界面模式）：主窗口 = 承载 argv 意图的数据面。
        if mode == UiMode::Main {
            spawn_main_window(cx, data.clone(), tx.clone());
        }
    });

    drop(instance_guard); // 退出释放单实例锁
    Ok(())
}

/// 批内是否请求了某设置项的生效分派（`VmSignal::SettingApplied`）。
///
/// 泵把「哪些项需要 UI 半场动作」与「动作怎么做」分开：本函数只回答前者，
/// [`apply_ui_effects`] 只回答后者——两者都是纯函数/纯 UI 副作用，可单测
/// 覆盖（泵循环本身内联在 `run_ui` 的异步闭包里，测试不可达）。
pub(crate) fn batch_requested(
    batch: &[crate::viewmodel::VmSignal],
    id: crate::viewmodel::SettingId,
) -> bool {
    batch.iter().any(|sig| match sig {
        crate::viewmodel::VmSignal::SettingApplied(applied) => *applied == id,
        _ => false,
    })
}

/// 批内信号是否需要浏览行重建（T-PERF1 信号分级）。
///
/// 只有真正改变行集/行内容的信号才重建：`EntriesChanged`（条目/过滤/树/
/// 选择）、`SettingApplied`（如目录行聚合大小开关）。`JobsChanged` 等
/// 进度类信号不碰行集——任务运行时的进度 tick 不再触发 O(行集) 重做；
/// 编辑完成后的会话重开由每 wake 必跑的 `ensure_browse_session` 兜底。
pub(crate) fn needs_browse_refresh(batch: &[crate::viewmodel::VmSignal]) -> bool {
    batch.iter().any(|sig| {
        matches!(
            sig,
            crate::viewmodel::VmSignal::EntriesChanged
                | crate::viewmodel::VmSignal::SettingApplied(_)
        )
    })
}

/// 主题重放的批量取值（纯函数，供泵内联调用 + 单测）。
///
/// 批内有 `Theme | ThemeColor | FontFamily | FontSize | CornerRadius` 之一
/// ⇒ `Some(内存镜像值)`（否则 `None`，重放零开销）。
///
/// **为什么取内存镜像而不是读盘**（用户实报 2026-10-09「界面字号/圆角点一次
/// 不生效、再点一次才生效」）：设置落盘是**异步**的（`Config::save_async`），
/// 而本函数在信号到达后微秒级执行——读盘必然拿到**上一拍**的值（实测 200/200
/// 陈旧，见 `hzip-config` 的「写后立读」修复），而 `AppData` 的镜像在 `store`
/// 步骤已就地更新 ⇒ 与快照同拍。
pub(crate) fn theme_replay_of(
    batch: &[crate::viewmodel::VmSignal],
    prefs: &crate::app::state::SettingsPrefs,
) -> Option<crate::ui::theme::ThemeReplay> {
    use crate::viewmodel::SettingId;
    const REPLAY_IDS: [SettingId; 5] = [
        SettingId::Theme,
        SettingId::ThemeColor,
        SettingId::FontFamily,
        SettingId::FontSize,
        SettingId::CornerRadius,
    ];
    let wanted = REPLAY_IDS.into_iter().any(|id| batch_requested(batch, id));
    wanted.then(|| crate::ui::theme::ThemeReplay {
        mode_ix: prefs.theme_ix,
        theme_color: prefs.theme_color.clone(),
        family: prefs.font_family.clone(),
        size_px: prefs.font_size,
        radius_px: prefs.corner_radius,
    })
}

/// 设置生效的 **UI 半场**应用（主题/语言是全局态，快照不携带，须 `&mut App`）。
///
/// - `theme_replay`：主题模式 / 主题色 / 字体族 / 字号 / **圆角**变更 → 重放主题
///   （都装进全局 `Theme`：`font.family` 由根 div 继承、`font.size` 由各窗口
///   `Root` 设为 rem 基准、`radius`/`radius_lg` 由 kit 各组件取用——见
///   [`crate::ui::theme::apply_theme_replay`]）。取值来自**内存镜像**
///   （[`theme_replay_of`]），不读盘。
/// - `lang_ix`：语言下标 → `init_locale`（rust-i18n 全局 locale）→
///   `relocalize` 重建**已定稿**的输出区文案（`OutLine.tail` 在进料时就已
///   定稿，增量 `apply` 无法回译）→ `refresh_windows` 触发重绘。
/// - `density_ix`：布局密度下标 → 写进程级间距刻度 + 重绘（布局构建期没有
///   `App`，间距刻度只能是原子量，见 `ui::spacing`）。
///
/// 值型设置（列序/开关等）不走此处：它们随快照进视图。
pub(crate) fn apply_ui_effects(
    cx: &mut gpui_kit::App,
    theme_replay: Option<crate::ui::theme::ThemeReplay>,
    lang_ix: Option<usize>,
    density_ix: Option<usize>,
    job_rows: &[crate::viewmodel::JobRow],
) {
    // 主题（模式/色/字体/圆角）：一次重放。`refresh_windows` 是必须的——重放改
    // 的是全局 `Theme` 的值，而真正把 `font.size` 落到窗口 rem 基准上的是各窗口
    // `Root` 的渲染帧。
    if let Some(replay) = &theme_replay {
        crate::ui::theme::apply_theme_replay(replay, cx);
        cx.refresh_windows();
    }
    // 布局密度：写全局间距刻度后重绘（`sp()` 在元素构建期读它）。
    if let Some(ix) = density_ix {
        crate::ui::spacing::set_density(crate::viewmodel::density::Density::from_index(ix));
        cx.refresh_windows();
    }
    if let Some(ix) = lang_ix {
        let lang = crate::app::labels::value_of(&crate::app::labels::LANG, ix);
        crate::init_locale(lang);
        // 切到中文但本机没有中文字形 → 回退英文（显式 `zh-CN` 只告警不改，见
        // `ui::fonts`）。必须在 `relocalize` **之前**：它按最终 locale 重建文案。
        crate::ui::fonts::apply_cjk_guard(cx, lang);
        crate::ui::views::progress::relocalize(cx, job_rows);
        cx.refresh_windows();
    }
}

/// 启动意图（bin main argv 归拢；02 §11.5 + R-GUI2 §2.6 协议增量）。
#[derive(Debug, Clone)]
pub(crate) enum LaunchIntent {
    /// 打开档案进入浏览
    Open { path: std::path::PathBuf },
    /// 解压到源档案所在目录
    ExtractHere { path: std::path::PathBuf },
    /// 智能解压（smart 强制 true）
    AutoExtract { path: std::path::PathBuf },
    /// 解压到源目录/「压缩文件名」/
    ExtractDir { path: std::path::PathBuf },
    /// 解压到目录选择器目标
    ExtractTo { path: std::path::PathBuf },
    /// 解压参数窗（右键「解压选项…」）：整批档案共用一套参数。
    ExtractOptions { archives: Vec<std::path::PathBuf> },
    /// 压缩：**整批**输入 → 一个档案（`%SELS` 语义）。
    ///
    /// `choose_output`：`false` = 输出按源名派生（右键「添加到 X.zip」，
    /// 固定格式不弹窗）；`true` = 由用户选输出路径（右键「添加到压缩文件…」）。
    Compress {
        paths: Vec<std::path::PathBuf>,
        choose_output: bool,
        format: Option<String>,
    },
}

impl LaunchIntent {
    /// 日志用意图名。
    fn kind(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open",
            Self::ExtractHere { .. } => "extract-here",
            Self::AutoExtract { .. } => "auto-extract",
            Self::ExtractDir { .. } => "extract-dir",
            Self::ExtractTo { .. } => "extract-to",
            Self::ExtractOptions { .. } => "extract-options",
            Self::Compress { .. } => "compress",
        }
    }
}

/// argv 段归拢为意图序列（裸路径 = Open；顺序保留）。
///
/// bin main 只识别 `--xxx <path>` 相邻对；本函数再把无标记段按当前位置
/// 语义归类——main.rs 已保证每意图路径紧跟其旗标，这里按「旗标路径段
/// 计数」重建（open/extract-here/... 各 Vec 顺序拼接）。
pub(crate) fn args_intents(args: &LaunchArgs) -> Vec<LaunchIntent> {
    let mut intents = Vec::new();
    for p in &args.open {
        intents.push(LaunchIntent::Open {
            path: std::path::PathBuf::from(p),
        });
    }
    for p in &args.extract_here {
        intents.push(LaunchIntent::ExtractHere {
            path: std::path::PathBuf::from(p),
        });
    }
    for p in &args.auto_extract {
        intents.push(LaunchIntent::AutoExtract {
            path: std::path::PathBuf::from(p),
        });
    }
    for p in &args.extract_dir {
        intents.push(LaunchIntent::ExtractDir {
            path: std::path::PathBuf::from(p),
        });
    }
    for p in &args.extract_to {
        intents.push(LaunchIntent::ExtractTo {
            path: std::path::PathBuf::from(p),
        });
    }
    // 解压参数窗：一批档案共用一套参数（与压缩同款批次语义）
    if !args.extract_options.is_empty() {
        intents.push(LaunchIntent::ExtractOptions {
            archives: args
                .extract_options
                .iter()
                .map(std::path::PathBuf::from)
                .collect(),
        });
    }
    // 压缩是**批次**语义：右键「添加到 X.zip」= 一次进程把全部选中项打成一个包
    // （verb 表用 `%SELS`）。若按逐路径拆成多个意图，会变成「每个输入各打一个包」。
    if !args.compress.is_empty() {
        intents.push(LaunchIntent::Compress {
            paths: args.compress.iter().map(std::path::PathBuf::from).collect(),
            choose_output: false,
            format: args.format.clone(),
        });
    }
    if !args.compress_to.is_empty() {
        intents.push(LaunchIntent::Compress {
            paths: args
                .compress_to
                .iter()
                .map(std::path::PathBuf::from)
                .collect(),
            choose_output: true,
            format: None,
        });
    }
    intents
}

/// argv（不含程序名）→ 意图序列。
///
/// 转发消费与启动共用：队列里存的是**原始 argv**，故这里用与 `bin main`
/// 同一个解析器（[`LaunchArgs::parse`]）还原——动词解释永远只有一套。
fn intents_of_argv(argv: &[String]) -> Vec<LaunchIntent> {
    match LaunchArgs::parse(argv) {
        ArgvParse::Run(a) => args_intents(&a),
        // 帮助/版本不产生意图（二实例永不转发它们：main 在仲裁前就返回）
        ArgvParse::Help | ArgvParse::Version => Vec::new(),
    }
}

/// 扩展意图（`{key, paths}`）→ 意图序列。
///
/// macOS FinderSync appex 与 Services provider 运行在**沙盒**内，既读不到宿主
/// 私有配置，也无法向宿主注入 argv（均已真机实证）；它们只能把 `{key, paths}`
/// 写进 App Group 容器的 `intents.jsonl`。宿主在此把 `key` 经 [`VERBS`] 表展开
/// 成 argv（`%SEL` 逐路径各一条、`%SELS` 一条收全批），再走**同一个**
/// [`LaunchArgs::parse`]/[`args_intents`] 路径——动词语义与命令行、转发队列完全
/// 同源，扩展侧绝不自己拼 argv。
///
/// [`VERBS`]: hzip_platform::context_menu::VERBS
//
// 非 macOS 主机上（Windows/Linux）本函数的唯一两处调用都在同一文件里被
// `#[cfg(target_os = "macos")]` 关掉了 ⇒ 它会「存在但没人调」，`clippy -D warnings`
// 的 dead_code 直接判红。按仓库既有口径（见 `hzip-platform/src/context_menu/mod.rs`
// 的 macos/linux 模块）用 `cfg_attr` **精确**放行：只对非 macOS 构建关掉这条 lint，
// 函数本体照样参与编译与类型检查（不留覆盖空洞）。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn intents_of_extension(
    intents: &[hzip_platform::context_menu::ExtensionIntent],
) -> Vec<LaunchIntent> {
    use hzip_platform::context_menu::entry_for;
    let mut out = Vec::new();
    for intent in intents {
        let Some(spec) = entry_for(&intent.key) else {
            tracing::warn!(key = %intent.key, "扩展意图的 verb key 不在白名单，忽略");
            continue;
        };
        if intent.paths.is_empty() {
            continue;
        }
        // argv 展开：`%SEL` → 每条路径各一条 argv；`%SELS` → 一条 argv 收全批。
        let expand = |template: &[&'static str], paths: &[String]| -> Vec<Vec<String>> {
            if template.contains(&"%SEL") {
                paths
                    .iter()
                    .map(|p| {
                        template
                            .iter()
                            .map(|a| {
                                if *a == "%SEL" {
                                    p.clone()
                                } else {
                                    (*a).to_string()
                                }
                            })
                            .collect()
                    })
                    .collect()
            } else {
                vec![
                    template
                        .iter()
                        .flat_map(|a| {
                            if *a == "%SELS" {
                                paths.to_vec()
                            } else {
                                vec![(*a).to_string()]
                            }
                        })
                        .collect(),
                ]
            }
        };
        for argv in expand(spec.argv, &intent.paths) {
            out.extend(intents_of_argv(&argv));
        }
    }
    out
}

/// 启动模式（argv → 模式；见本模块文档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiMode {
    /// 主界面模式：`--open` 或无参（单实例锁 + 主窗口 + 转发队列）。
    Main,
    /// **直接模式**：只有动作动词（不开主界面；独立进程，不取单实例锁、不参与
    /// 转发队列）。右键菜单的动作动词全部走这里。
    Direct,
}

impl UiMode {
    fn name(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Direct => "direct",
        }
    }
}

/// argv → 模式（纯函数，可单测）。
///
/// 规则：有 `--open`、或**没有任何动作动词** → 主界面；否则直接模式。
///
/// `--open` 优先是刻意的：显式要求「浏览这个档案」时主界面是唯一合理解释，
/// 其余动作照旧在主界面里执行（R-GUI2 的混用语义不变）。
///
/// 直接模式的进程语义（为什么不是「转发给已运行的主实例再开窗」）：
/// - 用户点右键要的是**这件事本身**，不是主界面；让主实例代劳就得给它加一堆
///   「别弹主窗」的特例，主窗生命周期还会牵着动作走；
/// - 独立进程可并发（同时解压三个档案 = 三个进度窗），与 7-Zip/Bandizip 一致；
/// - 不取 `hzip.lock`：那把锁的语义是「主界面单实例」，直接模式无主界面。
pub(crate) fn launch_mode(args: &LaunchArgs) -> UiMode {
    let has_action = !args.extract_here.is_empty()
        || !args.auto_extract.is_empty()
        || !args.extract_dir.is_empty()
        || !args.extract_to.is_empty()
        || !args.extract_options.is_empty()
        || !args.compress.is_empty()
        || !args.compress_to.is_empty();
    if args.open.is_empty() && has_action {
        UiMode::Direct
    } else {
        UiMode::Main
    }
}

/// argv 解析结果。
pub enum ArgvParse {
    /// `-h` / `--help`：调用方打印帮助后退出（不启动 UI）
    Help,
    /// `-V` / `--version`：调用方打印版本后退出
    Version,
    /// 正常启动参数（装箱：`ArgvParse` 只在启动路径构造一次，但枚举尺寸
    /// 一致性由 clippy 把关，装箱比 `allow` 更省心）
    Run(Box<LaunchArgs>),
}

/// GUI 启动参数（02 §11.5 + R-GUI2 §2.6 协议增量）。
#[derive(Debug, Default, Clone)]
pub struct LaunchArgs {
    /// --open <path>：启动即打开档案
    pub open: Vec<String>,
    /// --extract-here <path>：解压到档案所在目录
    pub extract_here: Vec<String>,
    /// --auto-extract <path>：智能解压（smart 强制 true）
    pub auto_extract: Vec<String>,
    /// --extract-dir <path>：解压到「压缩文件名/」子目录
    pub extract_dir: Vec<String>,
    /// --extract-to <path>：解压到目录选择器目标
    pub extract_to: Vec<String>,
    /// --extract-options <path...>：**解压参数窗**（右键「解压选项…」）。
    ///
    /// 多值：一次进程收下所有选中档案，参数窗只弹一次、参数共用（`%SELS` 语义，
    /// 与 Bandizip 的「解压选项」一致；逐个弹窗会连点 N 个对话框）。
    pub extract_options: Vec<String>,
    /// --compress <paths...>：弹压缩对话框预填输入
    pub compress: Vec<String>,
    /// --compress-to <dir>：压缩到目标目录（本轮打开 GUI 预填）
    pub compress_to: Vec<String>,
    /// --format <fmt>：--compress 的格式修饰
    pub format: Option<String>,
    /// `--delete`：解压后删除源档案（收尾挂账 R-GUI3——仅置位告警）
    pub delete_flag: bool,
}

impl LaunchArgs {
    /// 解析 argv（**不含程序名**）。
    ///
    /// 这是命令行协议的**唯一解析器**：`bin main` 与转发队列还原共用它，
    /// 故「右键菜单动的词」与「命令行动词」不会出现两套解释。手解析而非
    /// clap：本 bin 参数面小，且 clap 的全局参数语义会与 CLI 冲突（02 §11.5
    /// 协议冻结）。
    ///
    /// 语义要点（与 02 §11.5 一致，勿单独改动）：
    ///
    /// - 每个动词旗标**只吃紧跟其后的一个路径**；
    /// - 无旗标的裸参数视作 `--open`（资源管理器双击关联 `%1` 的形态）；
    /// - `--format <fmt>` 是 `--compress` 的修饰，紧随其后即可。
    pub fn parse(argv: &[String]) -> ArgvParse {
        let mut args = LaunchArgs::default();
        let mut pending: Option<&'static str> = None;
        for a in argv {
            match a.as_str() {
                "-h" | "--help" => return ArgvParse::Help,
                "-V" | "--version" => return ArgvParse::Version,
                "--open" => pending = Some("open"),
                "--extract-here" => pending = Some("extract"),
                "--auto-extract" => pending = Some("auto-extract"),
                "--extract-dir" => pending = Some("extract-dir"),
                "--extract-to" => pending = Some("extract-to"),
                "--extract-options" => pending = Some("extract-options"),
                "--compress" => pending = Some("compress"),
                "--compress-to" => pending = Some("compress-to"),
                "--format" => pending = Some("format"),
                "--delete" => {
                    // 收尾挂账 R-GUI3（ExtractOptions 无 delete 字段）：注册表
                    // verb 可带此参数，本轮按「不带删除」执行并告警。
                    args.delete_flag = true;
                    tracing::warn!("--delete 收尾挂账 R-GUI3：本次不会删除源文件");
                }
                _ => match pending {
                    // 多值动词：持续吸收后续非旗标参数，直到下一个旗标。
                    // 右键菜单用 `%SELS` 把**全部选中项**塞进同一条命令行
                    // （`--compress p1 p2 --format zip`），若按单值处理，
                    // p2 会掉进「裸路径 = --open」分支被当成打开。
                    Some("compress") => args.compress.push(a.clone()),
                    Some("compress-to") => args.compress_to.push(a.clone()),
                    // 多值：参数窗对整批生效（见字段文档）
                    Some("extract-options") => args.extract_options.push(a.clone()),
                    Some("open") => {
                        args.open.push(a.clone());
                        pending = None;
                    }
                    Some("extract") => {
                        args.extract_here.push(a.clone());
                        pending = None;
                    }
                    Some("auto-extract") => {
                        args.auto_extract.push(a.clone());
                        pending = None;
                    }
                    Some("extract-dir") => {
                        args.extract_dir.push(a.clone());
                        pending = None;
                    }
                    Some("extract-to") => {
                        args.extract_to.push(a.clone());
                        pending = None;
                    }
                    Some("format") => {
                        args.format = Some(a.clone());
                        pending = None;
                    }
                    // 裸路径参数视作 --open（shell 双击关联：%1）
                    _ => args.open.push(a.clone()),
                },
            }
        }
        ArgvParse::Run(Box::new(args))
    }
}

#[cfg(test)]
#[path = "tests/runtime.rs"]
mod tests;
