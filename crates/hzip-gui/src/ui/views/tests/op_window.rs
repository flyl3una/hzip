#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::ui::shell::dispatch::{quick_compress_vm, quick_extract_vm};
use gpui_kit::px;

/// 标准消息弹窗的级别图标：三级**形状互异**（不只靠颜色区分）。
///
/// 守的是「一眼可辨」：三个级别若共用一个图标，用户只能靠颜色猜严重程度
/// （色觉障碍/深色主题下更糟）。载荷构造器同时钉住级别到载荷的映射。
#[test]
fn message_window_levels_have_distinct_icons() {
    use crate::viewmodel::message::{MessageDialog, MessageLevel};
    let icons = [
        level_icon(MessageLevel::Info),
        level_icon(MessageLevel::Warning),
        level_icon(MessageLevel::Error),
    ];
    assert_eq!(icons[0], AppIcon::Info);
    assert_eq!(icons[1], AppIcon::Warn);
    assert_eq!(icons[2], AppIcon::OctagonAlert);
    assert!(
        icons[0] != icons[1] && icons[1] != icons[2] && icons[0] != icons[2],
        "三级图标必须互不相同：{icons:?}"
    );
    // 载荷构造器（业务侧唯一入口）
    assert_eq!(MessageDialog::info("t", "d").level, MessageLevel::Info);
    assert_eq!(
        MessageDialog::warning("t", "d").level,
        MessageLevel::Warning
    );
    assert_eq!(MessageDialog::error("t", "d").level, MessageLevel::Error);
}

/// 解压选项窗密码框 → VM 通道（2026-09-23：右键「解压选项」缺密码输入项）。
#[test]
fn extract_form_password_reaches_vm() {
    let cfg = hzip_config::Config::default();
    let mut f = ParamsForm::defaults_extract(&cfg);
    assert!(
        f.to_extract_vm(&cfg).password.is_none(),
        "未填 → None（回落运行时询问链）"
    );
    f.extract_pw_text = "pw".into();
    let vm = f.to_extract_vm(&cfg);
    assert_eq!(
        vm.password.as_ref().map(|z| z.as_str()),
        Some("pw"),
        "填了 → 进 spec 作 core 首个候选"
    );
}

/// 填了密码即视为要加密（缺省 AES-256）——否则撞 HZ-5003 单边，
/// 用户看到「不支持的操作」（2026-09-23 实测缺陷）。
#[test]
fn password_without_scheme_implies_aes256() {
    use hzip_format::{AesKey, EncryptionRequest};
    let none_ix = labels::ENC_METHOD
        .iter()
        .position(|(_, v)| *v == "none")
        .expect("ENC_METHOD 必含 none");
    let mut f = ParamsForm::empty();
    f.enc_ix = none_ix;
    assert!(f.enc_request().is_none(), "无密码无方案 → 不加密");
    f.password_text = "pw".into();
    assert_eq!(
        f.enc_request(),
        Some(EncryptionRequest::Aes(AesKey::Bits256)),
        "有密码无方案 → 缺省 AES-256（不留单边）"
    );
    // 显式选了方案 → 按所选（不被缺省覆盖）
    let zipcrypto_ix = labels::ENC_METHOD
        .iter()
        .position(|(_, v)| *v == "zipcrypto")
        .expect("ENC_METHOD 必含 zipcrypto");
    f.enc_ix = zipcrypto_ix;
    assert_eq!(f.enc_request(), Some(EncryptionRequest::ZipCrypto));
}

/// 参数窗按内容估高（用户实测：解压参数窗下方大片空白）。
#[test]
fn params_height_follows_content() {
    let ex = params_window_height(&OpKind::Extract {
        archives: vec![std::path::PathBuf::from("a.zip")],
    });
    let cp = params_window_height(&OpKind::Compress {
        inputs: vec![std::path::PathBuf::from("a.txt")],
    });
    assert!(ex < cp, "解压面行数少，应比压缩面矮：{ex:?} vs {cp:?}");
    assert!(
        f32::from(ex) >= f32::from(PARAMS_WINDOW_MIN_H),
        "不得低于最小高：{ex:?}"
    );
    assert!(
        f32::from(cp) <= f32::from(PARAMS_WINDOW_H),
        "不得超过默认高：{cp:?}"
    );
}

/// 用例表：事件 + 从表单读出「该事件应改变的字段」的取值器。
type KnobCase = (DialogEvent, fn(&ParamsForm) -> String);
/// 窗口构建闭包只能返回根视图，共享句柄/事件接收端用它带出。
type WindowSlots = Rc<RefCell<(Option<Shared>, Option<smol::channel::Receiver<DialogEvent>>)>>;

fn engine() -> std::sync::Arc<hzip_core::engine::Engine> {
    std::sync::Arc::new(crate::viewmodel::testutil::engine())
}

fn test_data() -> AppDataHandle {
    std::sync::Arc::new(std::sync::Mutex::new(crate::app::state::AppData::new(
        engine(),
    )))
}

/// 压缩 VM 的可观测指纹（`CompressDialogVm` 无 `PartialEq`；指纹只取会影响
/// `JobSpec` 的字段——「两模式一致」说的就是这些字段一致）。
fn compress_key(vm: &CompressDialogVm) -> String {
    format!(
        "fmt={:?} lvl={:?} split={:?} excl={:?} out={:?}",
        vm.format.as_ref().map(|f| f.as_str().to_string()),
        vm.level,
        vm.split,
        vm.excludes,
        vm.out
    )
}

fn extract_key(vm: &crate::viewmodel::dialog_vm::ExtractDialogVm) -> String {
    format!(
        "dest={:?} smart={} conflict={:?} codepage={:?} best={} symlink={:?}",
        vm.dest, vm.smart, vm.conflict, vm.codepage, vm.best_effort, vm.symlink
    )
}

/// **两模式一致性红线**：参数窗（表单）与快捷动词（config）对同一组参数
/// 必须产出同一份 VM —— 进而同一份 `JobSpec`。
///
/// 守的是「右键压缩」与「主界面/快捷压缩」各写一套参数翻译而漂移：本仓库
/// 已经吃过一次同类亏（`archive_exts` 取错源），故把等价性钉成测试。
#[test]
fn params_form_matches_quick_path() {
    let cfg = hzip_config::Config::default();
    let level_ix = labels::LEVEL
        .iter()
        .position(|(_, v)| *v == cfg.defaults.level)
        .unwrap_or(3);
    for (ix, (_, fmt)) in labels::UI_FORMAT.iter().enumerate() {
        // 输出路径按格式派生（文件名 + 保存到 两段），故逐格式构造期望值。
        // 期望值与 `ParamsForm::out_path` 同样走 `Path::join`——该输入是
        // Windows 风格字面量（产品以 Windows 为准），但本机分隔符非 `\` 时
        // 直接拼字符串会得到 `C:\tmp/a.zip`，故不能硬编码 `\`。
        let out = std::path::Path::new(r"C:\tmp").join(format!("a.{fmt}"));
        let form = ParamsForm {
            format_ix: ix,
            level_ix,
            filename_text: "a".into(),
            out_dir_text: r"C:\tmp".into(),
            ..ParamsForm::empty()
        };
        assert_eq!(
            compress_key(&form.to_compress_vm()),
            compress_key(&quick_compress_vm(fmt, &out, &cfg)),
            "格式 {fmt}：参数窗与快捷路径的压缩参数必须逐字段一致"
        );
    }
}

/// 09 旋钮的**未设置值** == `CompressDialogVm::default()`：参数窗不引入自己的
/// 隐式默认（否则「右键压缩」与「面板压缩」在用户没碰旋钮时就会分叉）。
#[test]
fn params_form_knobs_default_to_vm_defaults() {
    let d = CompressDialogVm::default();
    let f = ParamsForm::empty();
    assert_eq!(f.method_value(), d.method, "方式默认");
    assert_eq!(f.split_bytes(), d.split, "分卷默认");
    assert_eq!(f.threads_value(), d.threads, "线程默认");
    assert_eq!(f.dict_bytes(), d.dict_size, "字典默认");
    assert_eq!(f.solid, d.solid, "固实默认");
    assert_eq!(f.append_value(), d.append, "更新方式默认");
    assert_eq!(f.exclude_hidden, d.exclude_hidden, "排除隐藏默认");
    assert_eq!(f.follow_symlinks, d.follow_symlinks, "跟随链接默认");
    assert_eq!(f.keep_metadata, d.keep_metadata, "保留属性默认");
    assert_eq!(f.verify_after, d.verify_after, "完成后校验默认");
    assert_eq!(f.best_effort, d.best_effort, "尽力而为默认");
}

/// 解压侧同款等价性（含 conflict/codepage/symlink 三项配置派生）。
#[test]
fn extract_form_matches_quick_path() {
    let cfg = hzip_config::Config::default();
    let form = ParamsForm {
        dest_text: "C:/out".into(),
        ..ParamsForm::defaults_extract(&cfg)
    };
    assert_eq!(
        extract_key(&form.to_extract_vm(&cfg)),
        extract_key(&quick_extract_vm(Some(PathBuf::from("C:/out")), None, &cfg)),
        "解压参数窗与快捷路径必须逐字段一致"
    );
}

/// 参数窗初始值 == 配置值（否则会出现「设置里 normal、窗口里 maximum」
/// 这类错显，用户按窗口里的值执行却怪设置没生效）。
#[test]
fn param_window_defaults_follow_config() {
    let mut cfg = hzip_config::Config::default();
    cfg.defaults.format = "7z".into();
    cfg.defaults.level = "ultra".into();
    cfg.extract.smart = true;
    cfg.extract.conflict = "overwrite".into();
    cfg.extract.codepage = Some("gbk".into());

    let c = ParamsForm::defaults_compress(&[PathBuf::from("C:/d/x.txt")], &cfg);
    assert_eq!(labels::UI_FORMAT[c.format_ix].1, "7z");
    assert_eq!(labels::LEVEL[c.level_ix].1, "ultra");
    // 输出预填拆成「文件名 + 保存到」两段（09 §3.1）：x + C:/d，扩展名随格式
    assert_eq!(c.filename_text, "x", "文件名取源主名");
    assert_eq!(c.ext(), "7z", "扩展名随格式");
    let p = c.out_path().expect("应派生出输出路径");
    assert!(
        p.to_string_lossy()
            .replace('\\', "/")
            .ends_with("C:/d/x.7z"),
        "实际: {p:?}"
    );

    let e = ParamsForm::defaults_extract(&cfg);
    assert!(e.smart, "smart 应跟随 [extract].smart");
    assert_eq!(labels::CONFLICT[e.conflict_ix].1, "overwrite");
    assert_eq!(labels::CODEPAGE[e.codepage_ix].1, "gbk");
}

/// 分卷/排除/路径的边界编码。
///
/// - 预设「不分卷」与自定义 `0`/空 → `None`（**不是** `Some(0)`，否则 core
///   会按 0 字节分卷）；
/// - 排除为空白 → 空列表（不是 `[""]`）；
/// - 文件名/目录两侧空白应裁剪；
/// - 自定义分卷单位为 MB。
#[test]
fn compress_form_encodes_empty_inputs_as_none() {
    let ix_of = |v: &str| labels::SPLIT.iter().position(|(_, x)| *x == v).unwrap();
    let form = ParamsForm {
        split_ix: ix_of("none"),
        excludes_text: "   ".into(),
        filename_text: "  a  ".into(),
        out_dir_text: "  C:/t  ".into(),
        ..ParamsForm::empty()
    };
    let vm = form.to_compress_vm();
    assert_eq!(vm.split, None, "不分卷预设 → None");
    assert!(vm.excludes.is_empty(), "空白排除项不得变成空串元素");
    assert_eq!(
        vm.out,
        Some(PathBuf::from("C:/t/a.zip")),
        "路径两侧空白应裁剪"
    );

    // 自定义分卷：单位 MB；0/空 → None
    let custom = ParamsForm {
        split_ix: ix_of("custom"),
        split_custom_text: "0".into(),
        ..ParamsForm::empty()
    };
    assert_eq!(custom.split_bytes(), None, "自定义 0 = 不分卷");
    let custom = ParamsForm {
        split_ix: ix_of("custom"),
        split_custom_text: "45".into(),
        ..ParamsForm::empty()
    };
    assert_eq!(custom.split_bytes(), Some(45 * 1024 * 1024));

    // 预设 100 MB + 排除项
    let form = ParamsForm {
        split_ix: ix_of("100m"),
        excludes_text: "*.tmp, sub/*".into(),
        ..ParamsForm::empty()
    };
    let vm = form.to_compress_vm();
    assert_eq!(vm.split, Some(100 * 1024 * 1024));
    assert_eq!(vm.excludes, vec!["*.tmp", "sub/*"]);
}

/// 控件事件 → 表单：**每个**旋钮都在 `apply_form_event` 里有落点。
///
/// 这条守的是「面板画了控件、事件没人接」——`_ => {}` 时代它就发生在
/// `CompressDict`/`CompressUpdateMode` 这类后加旋钮上（能点，但值不进 spec）。
#[test]
fn apply_form_event_maps_every_knob() {
    let cases: Vec<KnobCase> = vec![
        (
            DialogEvent::CompressSplitPreset(5),
            (|f: &ParamsForm| f.split_ix.to_string()) as fn(&ParamsForm) -> String,
        ),
        (DialogEvent::CompressThreads(3), |f| {
            f.threads_ix.to_string()
        }),
        (DialogEvent::CompressDict(4), |f| f.dict_ix.to_string()),
        (DialogEvent::CompressUpdateMode(1), |f| {
            f.update_ix.to_string()
        }),
        (DialogEvent::CompressEncMethod(2), |f| f.enc_ix.to_string()),
        (DialogEvent::CompressMethod(1), |f| {
            format!("{:?}", f.method_value())
        }),
        (DialogEvent::CompressSolid(true), |f| f.solid.to_string()),
        (DialogEvent::CompressExcludeHidden(true), |f| {
            f.exclude_hidden.to_string()
        }),
        (DialogEvent::CompressFollowSymlinks(true), |f| {
            f.follow_symlinks.to_string()
        }),
        (DialogEvent::CompressKeepMetadata(false), |f| {
            f.keep_metadata.to_string()
        }),
        (DialogEvent::CompressVerifyAfter(false), |f| {
            f.verify_after.to_string()
        }),
        (DialogEvent::CompressBestEffort(true), |f| {
            f.best_effort.to_string()
        }),
        // enc_names 默认 true（决策② 安全默认）→ 事件须发反值才可证「改了」
        (DialogEvent::CompressEncryptNames(false), |f| {
            f.enc_names.to_string()
        }),
        (DialogEvent::CompressToggleAdvanced(true), |f| {
            f.advanced_open.to_string()
        }),
        (DialogEvent::CompressToggleEncryption(true), |f| {
            f.enc_open.to_string()
        }),
        (DialogEvent::ExtractConflict(1), |f| {
            f.conflict_ix.to_string()
        }),
        (DialogEvent::ExtractCodepage(2), |f| {
            f.codepage_ix.to_string()
        }),
        (DialogEvent::ExtractSmart(true), |f| f.smart.to_string()),
        (DialogEvent::ExtractBestEffort(true), |f| {
            f.best_effort.to_string()
        }),
    ];
    for (ev, read) in cases {
        // 每个用例从**默认表单**出发：断言事件确实改了该字段（而非撞上前一例的残留值）
        let mut f = ParamsForm::empty();
        let before = read(&f);
        assert!(apply_form_event(&mut f, &ev), "{ev:?} 应有落点");
        assert_ne!(read(&f), before, "{ev:?} 未改变对应字段");
    }
    // 非「落表单」事件不得被误判为已处理（它们是提交/取消/浏览/取路径）
    for ev in [
        DialogEvent::CompressStart,
        DialogEvent::ExtractCancel,
        DialogEvent::CompressBrowse,
    ] {
        assert!(!apply_form_event(&mut ParamsForm::empty(), &ev));
    }
}

/// 面板布局不变量（2026-09 返工的核心）：**正文可滚、底部永远在窗口内**。
///
/// 旧实现用 `Dialog` 弹层承载参数界面：弹层高度由内容驱动（无 `max_h`），
/// 高级/加密区一展开，底部「开始」按钮就被推到窗口外且无法滚动。此测试把
/// 窗口压到 420px 高（内容必然超），断言：
/// 1. 正文内容高于正文可视区（真的溢出 → 有东西可滚，不是被裁掉）；
/// 2. 底部条整体在窗口内（按钮点得到）。
#[gpui_kit::test]
fn panel_body_scrolls_and_footer_stays_visible(cx: &mut gpui_kit::TestAppContext) {
    // 窗口高度刻意压到 420px：内容（两项折叠区全展开）必然超出
    let handle = cx.open_window(size(px(560.), px(420.)), |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let mut form =
            ParamsForm::defaults_compress(&[PathBuf::from("C:/src/a.txt")], &Default::default());
        form.advanced_open = true; // 两项都展开 = 内容最多的形态
        form.enc_open = true;
        let (tx, _rx) = smol::channel::unbounded();
        let ctl = build_controls(&form, &tx, window, cx);
        let view = cx.new(|_| ParamsWindow {
            state: Rc::new(RefCell::new(OpState {
                kind: OpKind::Compress {
                    inputs: vec![PathBuf::from("C:/src/a.txt")],
                },
                data: test_data(),
                form,
                // 长错误行（底部固定区）把正文可视区压小 —— 测试平台的窗口
                // 恒为最大化尺寸（`resize`/`open_window` 的尺寸不生效），
                // 只能靠内容把「内容 > 可视区」造出来。
                error: "这是一段很长的错误说明，用来把底部固定区撑高，从而让正文区变小；                            参数面板必须在这种情况下仍然可滚动、底部按钮仍然可见。"
                    .repeat(2),
                    ctl,
            })),
            ev_tx: tx,
        });
        let any: gpui_kit::AnyView = view.into();
        Root::new(any, window, cx)
    });
    let mut vtcx = gpui_kit::VisualTestContext::from_window(*handle, cx);
    vtcx.update(|window, cx| window.draw(cx).clear(cx));

    let head = vtcx.debug_bounds("params-head").expect("头部有界");
    let root = vtcx.debug_bounds("params-root").expect("面板根有界");
    let inner = vtcx
        .debug_bounds("params-body-inner")
        .expect("正文内容有界");
    let foot = vtcx.debug_bounds("params-foot").expect("底部条有界");
    let win_h = vtcx.update(|window, _| window.viewport_size().height);

    // ① 面板铺满窗口
    assert!(
        (root.size.height - win_h).abs() <= px(1.),
        "面板根 {:.0}px 应铺满窗口 {:.0}px",
        root.size.height,
        win_h
    );
    // ② 底部条整体在窗口内（「开始」按钮永远点得到——旧 Dialog 承载时会被顶出窗外）
    assert!(
        foot.origin.y + foot.size.height <= win_h + px(1.),
        "底部条 {:.0}px 必须整体在窗口 {:.0}px 内",
        foot.origin.y + foot.size.height,
        win_h
    );
    // ③ 内容高于「头与底之间」的可用高度 → 真的有东西要滚
    let viewport = foot.origin.y - (head.origin.y + head.size.height);
    assert!(
        inner.size.height > viewport,
        "正文内容 {:.0}px 应高于可视区 {:.0}px（否则测不到滚动）",
        inner.size.height,
        viewport
    );
    // ④ 在可视区内滚轮 → 内容上移：证明这是**有界滚动视口**，
    //    而不是「不加约束地把内容铺出去」（那样滚轮不会动任何东西）
    let before = vtcx
        .debug_bounds("params-body-inner")
        .expect("正文内容有界")
        .origin
        .y;
    let mid = gpui_kit::point(
        root.size.width / 2.,
        head.origin.y + head.size.height + viewport / 2.,
    );
    vtcx.simulate_event(gpui_kit::ScrollWheelEvent {
        position: mid,
        delta: gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -4.)),
        ..Default::default()
    });
    vtcx.update(|window, cx| window.draw(cx).clear(cx));
    let after = vtcx
        .debug_bounds("params-body-inner")
        .expect("正文内容有界")
        .origin
        .y;
    assert!(
        after < before,
        "滚轮应让内容上移（before {before:.0} → after {after:.0}）"
    );
}

/// **参数面随格式变化**（2026-09 用户要求：参照 7-Zip）。
///
/// 面板只渲染 `*_supported` 为真的项，故本表即「各格式露出的参数面」这一
/// 用户可见契约：zip 有分卷/更新方式（无字典/无线程差异），7z 有字典，
/// tar 纯打包（没有压缩方式、没有线程数、没有分卷/更新方式以外的压缩旋钮），
/// 单流格式（gz 等）只压不打包（无分卷/无追加）。
///
/// 同时钉住「压缩方式」：本项目所有格式 `caps.methods` 都只有 1 个候选 →
/// `method_selectable` 恒 false → 那一行**不渲染**（用户报障：灰着的下拉
/// 点不动也看不懂）。
#[gpui_kit::test]
fn param_face_follows_format(cx: &mut gpui_kit::TestAppContext) {
    let slots: WindowSlots = Rc::new(RefCell::new((None, None)));
    let slots_w = slots.clone();
    let (host, _vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let (tx, rx) = smol::channel::unbounded();
        let form =
            ParamsForm::defaults_compress(&[PathBuf::from("C:/src/a.txt")], &Default::default());
        let ctl = build_controls(&form, &tx, window, cx);
        let state = Rc::new(RefCell::new(OpState {
            kind: OpKind::Compress {
                inputs: vec![PathBuf::from("C:/src/a.txt")],
            },
            data: test_data(),
            form,
            error: String::new(),
            ctl,
        }));
        let view = cx.new(|_| ParamsWindow {
            state: state.clone(),
            ev_tx: tx,
        });
        *slots_w.borrow_mut() = (Some(state), Some(rx));
        let any: gpui_kit::AnyView = view.into();
        Root::new(any, window, cx)
    });
    let _ = host;
    let state = slots.borrow().0.clone().expect("状态已带出");

    let ix_of = |v: &str| labels::UI_FORMAT.iter().position(|(_, x)| *x == v).unwrap();
    let face = |fmt: &str| {
        let mut st = state.borrow_mut();
        st.form.format_ix = ix_of(fmt);
        let p = st.compress_props();
        (
            p.method_selectable,
            p.level_supported,
            p.threads_supported,
            p.dict_supported,
            p.split_supported,
            p.append_supported,
            p.enc_writable,
        )
    };

    // (方式可选, 级别行, 线程, 字典, 分卷, 更新方式, 加密区)
    assert_eq!(
        face("zip"),
        (false, true, true, false, true, true, true),
        "zip（加密区随写侧接通露出，设计稿 11）"
    );

    assert_eq!(
        face("7z"),
        (false, true, true, true, false, false, true),
        "7z：**分卷区不露出**——写侧分卷未接通（caps.split=false，T46），声称支持会让用户点到一个必然失败的控件；加密区随写侧接通露出（设计稿 11）"
    );
    assert_eq!(
        face("tar"),
        (false, false, false, false, false, true, false),
        "tar 纯打包：无压缩层 → 级别/线程整行不渲染"
    );
    assert_eq!(
        face("gz"),
        (false, true, true, false, false, false, false),
        "gz 单流"
    );
    // 组合格式（tar.gz 等）已从界面**屏蔽**（`labels::UI_FORMAT`）：
    // 它们不在 `format_ix` 下标域内，`format_id()` 不可达——这里改为断言
    // UI 下标域确实不含组合格式（防将来有人把表加回去而不同步本测试）。
    for (_, v) in labels::FORMAT.iter().filter(|(_, v)| v.contains('.')) {
        assert!(
            labels::UI_FORMAT.iter().all(|(_, uv)| uv != v),
            "{v} 组合格式应保持界面屏蔽"
        );
    }
    // 所有格式的压缩方式都只有 1 个候选 → 面板不渲染该行
    for (_, fmt) in labels::UI_FORMAT.iter() {
        assert!(!face(fmt).0, "{fmt} 的压缩方式不应出现（单候选）");
    }
}

/// 目标已存在（2026-09 用户要求）：**弹 Dialog 确认覆盖/取消**。
///
/// 断言三件：
/// 1. 决策：目标存在能被识别；「不覆盖」改名兜底、「覆盖」原样写（纯函数）；
/// 2. 对话框**真的被画出来**（层由 kit `WindowState` 插件随 `Root` 自动挂）；
/// 3. 对话框里的「覆盖」按钮通到 `CompressOverwriteAccept`（按钮不是死的）。
#[gpui_kit::test]
fn existing_target_opens_confirm_dialog(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join("hzip-overwrite-dialog");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("packed.zip");
    std::fs::write(&target, b"OLD").unwrap();

    let slots: WindowSlots = Rc::new(RefCell::new((None, None)));
    let slots_w = slots.clone();
    let dir_s = dir.display().to_string();
    let (host, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let (tx, rx) = smol::channel::unbounded();
        let form = ParamsForm {
            filename_text: "packed".into(),
            out_dir_text: dir_s,
            ..ParamsForm::empty()
        };
        let ctl = build_controls(&form, &tx, window, cx);
        let state = Rc::new(RefCell::new(OpState {
            kind: OpKind::Compress {
                inputs: vec![PathBuf::from("C:/src/a.txt")],
            },
            data: test_data(),
            form,
            error: String::new(),
            ctl,
        }));
        let view = cx.new(|_| ParamsWindow {
            state: state.clone(),
            ev_tx: tx,
        });
        *slots_w.borrow_mut() = (Some(state), Some(rx));
        let any: gpui_kit::AnyView = view.into();
        Root::new(any, window, cx)
    });
    let _ = host;
    let (state, rx) = {
        let b = slots.borrow();
        (
            b.0.clone().expect("状态已带出"),
            b.1.clone().expect("接收端已带出"),
        )
    };

    // ① 决策：认得出目标已存在；覆盖/不覆盖两条路径分开
    assert!(
        state.borrow().existing_target().is_some(),
        "目标 {target:?} 已存在，应被识别"
    );
    let form = state.borrow().form.clone();
    assert_ne!(
        target_path(&form, false).unwrap(),
        target,
        "「不覆盖」必须避开既有档案"
    );
    assert_eq!(
        target_path(&form, true).unwrap(),
        target,
        "「覆盖」= 原样写用户指定路径"
    );

    // ②③ 对话框本体：挂进本窗口的 dialog 层，点「覆盖」发事件
    let (tx2, _rx2) = smol::channel::unbounded::<DialogEvent>();
    vtcx.update(|window, cx| {
        window.open_dialog(cx, move |_dialog, _window, cx| {
            let tx = tx2.clone();
            crate::ui::dialogs::compress_overwrite_dialog(cx, "C:/tmp/packed.zip", move |e| {
                let _ = tx.try_send(e);
            })
        });
    });
    vtcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        vtcx.debug_bounds("dialog-layer").is_some(),
        "参数窗必须挂上对话框层（kit `WindowState` 插件随 `Root` 自动挂）\
         ——否则确认对话框登记了却没人画"
    );
    // 对话框内容确实到了屏幕上（含「覆盖」按钮）：
    // 用 `debug_bounds` 而非 DOM 式按 id 查找（gpui 无该 API）：
    // 渲染帧的命中区里有这个 id = 按钮真的画出来了。
    assert!(
        vtcx.debug_bounds("compress-overwrite-accept").is_some(),
        "确认对话框里应有「覆盖」按钮"
    );
    struct OverwriteDialogHost {
        tx: smol::channel::Sender<DialogEvent>,
    }
    impl gpui_kit::Render for OverwriteDialogHost {
        fn render(
            &mut self,
            _: &mut Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            let tx = self.tx.clone();
            crate::ui::dialogs::compress_overwrite_dialog(cx, "C:/tmp/packed.zip", move |e| {
                let _ = tx.try_send(e);
            })
        }
    }
    let (tx3, rx3) = smol::channel::unbounded::<DialogEvent>();
    let (host2, vtcx2) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let any: gpui_kit::AnyView = cx.new(|_| OverwriteDialogHost { tx: tx3 }).into();
        Root::new(any, window, cx)
    });
    let _ = host2;
    vtcx2.update(|window, cx| window.draw(cx).clear(cx));
    let btn = vtcx2
        .debug_bounds("compress-overwrite-accept")
        .expect("「覆盖」按钮应有渲染命中区");
    vtcx2.simulate_click(btn.center(), gpui_kit::Modifiers::none());
    let ev = rx3
        .try_recv()
        .expect("点「覆盖」必须发出 CompressOverwriteAccept（否则按钮是死的）");
    assert!(
        matches!(ev, DialogEvent::CompressOverwriteAccept),
        "实得 {ev:?}"
    );

    // ④ 既有档案未被本测试动过（提交归 start()，这里只验决策与对话框接线）
    assert_eq!(std::fs::read(&target).unwrap(), b"OLD");
    let _ = rx.try_recv();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 下拉接线回归（2026-09 用户报障）：**选格式必须真的改到提交物**。
///
/// 两个缺陷一起守：
/// 1. 候选项曾直接塞 label key（下拉里显示 `opt.format.zip`）——现在候选项是
///    本地化 glob 文案（`*.zip` / `*.7z`）；
/// 2. `select_string_state` 曾把 `on_pick(ix)` 的返回值**丢掉**，下拉「选了等于
///    没选」：格式/方式/分卷… 全停在初始值 → 不管选什么，输出永远是 zip。
#[gpui_kit::test]
fn select_confirm_flows_to_spec(cx: &mut gpui_kit::TestAppContext) {
    let slots: WindowSlots = Rc::new(RefCell::new((None, None)));
    let slots_w = slots.clone();
    let handle = cx.open_window(size(px(560.), px(1400.)), |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let (tx, rx) = smol::channel::unbounded();
        let form =
            ParamsForm::defaults_compress(&[PathBuf::from("C:/src/a.txt")], &Default::default());
        let ctl = build_controls(&form, &tx, window, cx);
        let state = Rc::new(RefCell::new(OpState {
            kind: OpKind::Compress {
                inputs: vec![PathBuf::from("C:/src/a.txt")],
            },
            data: test_data(),
            form,
            error: String::new(),
            ctl,
        }));
        let view = cx.new(|_| ParamsWindow {
            state: state.clone(),
            ev_tx: tx,
        });
        *slots_w.borrow_mut() = (Some(state), Some(rx));
        let any: gpui_kit::AnyView = view.into();
        Root::new(any, window, cx)
    });
    let mut vtcx = gpui_kit::VisualTestContext::from_window(*handle, cx);
    let (state, rx) = {
        let b = slots.borrow();
        (
            b.0.clone().expect("状态已带出"),
            b.1.clone().expect("接收端已带出"),
        )
    };
    vtcx.update(|window, cx| window.draw(cx).clear(cx));

    // 候选项文案 = 本地化 glob（不是 i18n 键）
    let shown = labels::labels_of(&labels::UI_FORMAT);
    assert_eq!(shown[1], "*.7z", "格式候选项应显示为 *.7z");
    assert!(
        shown.iter().all(|s| !s.starts_with("opt.")),
        "候选项不得是 i18n 内部键：{shown:?}"
    );

    // 模拟「用户在下拉里选中 *.7z」：Select 确认 → 订阅 → 事件通道
    let sel = state.borrow().ctl.format_select.clone();
    let pick = shown[1].clone();
    vtcx.update(|_window, cx| {
        sel.update(cx, |_, cx| {
            cx.emit(gpui_kit::component::select::SelectEvent::Confirm(Some(
                pick,
            )));
        });
    });

    let mut applied = 0;
    while let Ok(ev) = rx.try_recv() {
        if apply_form_event(&mut state.borrow_mut().form, &ev) {
            applied += 1;
        }
    }
    assert_eq!(
        applied, 1,
        "格式选择必须发出可落表单的事件（旧实现丢掉 on_pick 返回值 → 选了等于没选）"
    );

    let st = state.borrow();
    assert_eq!(st.form.format_id(), "7z", "表单格式应切到 7z");
    let vm = st.form.to_compress_vm();
    assert_eq!(
        vm.format
            .as_ref()
            .map(|f| f.as_str().to_string())
            .as_deref(),
        Some("7z"),
        "VM 格式应随选择变化"
    );
    let out = vm.out.clone().expect("输出路径");
    assert!(
        out.to_string_lossy().replace('\\', "/").ends_with("a.7z"),
        "输出后缀必须随格式走（实际 {out:?}）"
    );
    let spec = vm
        .build_spec(&[PathBuf::from("C:/src/a.txt")], &out)
        .expect("build_spec");
    assert_eq!(
        spec.compress.as_ref().expect("压缩 spec").format.as_str(),
        "7z",
        "JobSpec 必须带用户选的格式（否则压缩结果还是按初始格式写）"
    );
}

/// **端到端接线**：测试窗口里点真控件 → 事件 → 表单 → `JobSpec`。
///
/// 覆盖的正是用户可见的「参数选择」：勾上「完成后校验」「排除隐藏文件」等
/// 开关后，提交出去的 `CompressSpec` 必须逐字段跟着变。
///
/// 定位方式：`debug_bounds` 取各开关格的矩形（`dialogs::switch_row` 给每格
/// 挂了以控件 id 命名的 selector），再按坐标点击——kit 的 `Checkbox` 把 id
/// 只当 keyed state 键，`window.click(id)` 找不到它。
#[gpui_kit::test]
fn panel_controls_reach_spec(cx: &mut gpui_kit::TestAppContext) {
    let slots: WindowSlots = Rc::new(RefCell::new((None, None)));
    let slots_w = slots.clone();
    // 窗口放大到一屏放下所有控件（点击要求目标在可视区内）
    let handle = cx.open_window(size(px(560.), px(1400.)), |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let mut form =
            ParamsForm::defaults_compress(&[PathBuf::from("C:/src/a.txt")], &Default::default());
        // 开关都在「高级选项」折叠区里——不展开就等于没渲染
        form.advanced_open = true;
        let (tx, rx) = smol::channel::unbounded();
        let ctl = build_controls(&form, &tx, window, cx);
        let state = Rc::new(RefCell::new(OpState {
            kind: OpKind::Compress {
                inputs: vec![PathBuf::from("C:/src/a.txt")],
            },
            data: test_data(),
            form,
            error: String::new(),
            ctl,
        }));
        let view = cx.new(|_| ParamsWindow {
            state: state.clone(),
            ev_tx: tx,
        });
        *slots_w.borrow_mut() = (Some(state), Some(rx));
        let any: gpui_kit::AnyView = view.into();
        Root::new(any, window, cx)
    });
    let mut vtcx = gpui_kit::VisualTestContext::from_window(*handle, cx);
    let (state, rx) = {
        let b = slots.borrow();
        (
            b.0.clone().expect("状态已带出"),
            b.1.clone().expect("接收端已带出"),
        )
    };

    // 四个开关：点各自的格子（点左侧 = 落在勾选框本体上）
    let ids = [
        "compress-verify-after",
        "compress-exclude-hidden",
        "compress-follow-symlinks",
        "compress-best-effort",
    ];
    for id in ids {
        let cell = vtcx
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("{id} 格应有界（面板必须渲染该开关）"));
        let hit = cell.origin + gpui_kit::point(px(8.), cell.size.height / 2.);
        vtcx.simulate_click(hit, gpui_kit::Modifiers::default());
    }
    // 事件 → 表单（生产链路里由消费者任务做，这里显式驱动同一函数）
    let mut applied = 0;
    while let Ok(ev) = rx.try_recv() {
        if apply_form_event(&mut state.borrow_mut().form, &ev) {
            applied += 1;
        }
    }
    assert_eq!(applied, ids.len(), "四个开关都应发出可落表单的事件");

    let (vm, spec) = {
        let st = state.borrow();
        let vm = st.form.to_compress_vm();
        let out = vm.out.clone().expect("输出路径");
        let spec = vm
            .build_spec(&[PathBuf::from("C:/src/a.txt")], &out)
            .unwrap();
        (vm, spec)
    };
    let c = spec.compress.as_ref().expect("压缩 spec");
    assert!(!vm.verify_after, "「完成后校验」勾掉后 vm 应为 false");
    assert!(!c.verify_after, "「完成后校验」必须落到 JobSpec");
    assert!(c.exclude_hidden, "「排除隐藏文件」必须落到 JobSpec");
    assert!(c.follow_symlinks, "「跟随符号链接」必须落到 JobSpec");
    assert!(c.best_effort, "「尽力而为」必须落到 JobSpec");
}
