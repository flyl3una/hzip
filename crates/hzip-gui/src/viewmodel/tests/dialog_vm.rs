use super::*;
use hzip_core::Level;

fn tmp() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("tmpdir")
}

/// 解压参数携带密码 → spec 通道（2026-09-23：解压选项密码框）。
#[test]
fn extract_build_spec_carries_password() {
    let dir = tmp();
    let a = dir.path().join("pw.zip");
    std::fs::write(&a, b"x").expect("写占位档案");
    let vm = ExtractDialogVm {
        password: Some(Zeroizing::new("pw".into())),
        ..Default::default()
    };
    let spec = vm.build_spec(&a, &SelectionArg::All).expect("build_spec");
    let opts = spec.extract.expect("extract opts 必须带上");
    assert_eq!(
        opts.password.as_ref().map(|z| z.as_str()),
        Some("pw"),
        "密码必须进 ExtractOptions（core 首个候选）"
    );
}

// README §2.5 契约：5 策略逐一映射（与 CLI CONFLICTS 白名单同值域）
#[test]
fn conflict_from_str_covers_whitelist() {
    assert_eq!(conflict_from_str("ask"), ConflictPolicy::Ask);
    assert_eq!(conflict_from_str("overwrite"), ConflictPolicy::Overwrite);
    assert_eq!(conflict_from_str("skip"), ConflictPolicy::Skip);
    assert_eq!(conflict_from_str("rename"), ConflictPolicy::RenameAuto);
    assert_eq!(conflict_from_str("newer"), ConflictPolicy::NewerOnly);
}

// README §2.5 契约：未知值回落默认 Ask（配置写坏不 panic）
#[test]
fn conflict_from_str_unknown_falls_back_to_ask() {
    assert_eq!(conflict_from_str(""), ConflictPolicy::Ask);
    assert_eq!(conflict_from_str("bogus"), ConflictPolicy::Ask);
    assert_eq!(conflict_from_str("OVERWRITE"), ConflictPolicy::Ask);
}

// README §2.5 契约：6 编码逐一映射（与 CLI parse_codepage 白名单同值域）
#[test]
fn codepage_from_str_covers_whitelist() {
    assert_eq!(codepage_from_str(Some("gbk")), Some(Encoding::Gbk));
    assert_eq!(codepage_from_str(Some("big5")), Some(Encoding::Big5));
    assert_eq!(codepage_from_str(Some("sjis")), Some(Encoding::ShiftJis));
    assert_eq!(codepage_from_str(Some("euc-kr")), Some(Encoding::EucKr));
    assert_eq!(codepage_from_str(Some("cp1252")), Some(Encoding::Cp1252));
    assert_eq!(codepage_from_str(Some("utf-8")), Some(Encoding::Utf8));
}

// 符号链接策略（跨平台档案释放；config [security].symlink 白名单）
#[test]
fn symlink_from_str_covers_whitelist() {
    use hzip_format::SymlinkPolicy;
    assert_eq!(symlink_from_str("deny-escape"), SymlinkPolicy::DenyEscape);
    assert_eq!(symlink_from_str("skip"), SymlinkPolicy::Skip);
    assert_eq!(symlink_from_str("flatten"), SymlinkPolicy::Flatten);
    assert_eq!(symlink_from_str("deny-all"), SymlinkPolicy::DenyAll);
}

// 未知值回落默认 DenyEscape（配置写坏不 panic；02 §9.6 默认档）
#[test]
fn symlink_from_str_unknown_falls_back_to_deny_escape() {
    use hzip_format::SymlinkPolicy;
    assert_eq!(symlink_from_str(""), SymlinkPolicy::DenyEscape);
    assert_eq!(symlink_from_str("bogus"), SymlinkPolicy::DenyEscape);
}

// README §2.5 契约：None/auto/空/未知 → None（自动检测）
#[test]
fn codepage_from_str_auto_and_unknown_are_none() {
    assert_eq!(codepage_from_str(None), None);
    assert_eq!(codepage_from_str(Some("auto")), None);
    assert_eq!(codepage_from_str(Some("AUTO")), None);
    assert_eq!(codepage_from_str(Some("")), None);
    assert_eq!(codepage_from_str(Some("iso-8859-1")), None);
}

#[test]
fn compress_build_spec_fields_mapped() {
    let d = tmp();
    let src = d.path().join("src");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("a.txt"), b"x").expect("write");
    let out = d.path().join("out.zip");
    let vm = CompressDialogVm {
        format: Some(FormatId::from("zip")),
        level: Level::Fast,
        split: Some(1024),
        excludes: vec!["*.tmp".into()],
        ..Default::default()
    };
    let spec = vm
        .build_spec(std::slice::from_ref(&src), &out)
        .expect("spec");
    assert_eq!(spec.kind, JobKind::Compress);
    let c = spec.compress.expect("compress spec");
    assert_eq!(c.format.as_str(), "zip");
    assert_eq!(c.level, Level::Fast);
    assert_eq!(c.split, Some(1024));
    assert_eq!(c.exclude_globs, vec!["*.tmp"]);
    assert_eq!(c.inputs, vec![src]);
    assert_eq!(c.out, out);
}

// 级别解析：值域与 labels::LEVEL / 配置字符串一一对应（错位会让「最快」
// 落成「最大」这类静默降级）
#[test]
fn level_from_str_matches_labels_table_order() {
    use crate::app::labels::LEVEL;
    for (ix, (_, v)) in LEVEL.iter().enumerate() {
        let want = [
            Level::Store,
            Level::Fastest,
            Level::Fast,
            Level::Normal,
            Level::Maximum,
            Level::Ultra,
        ][ix];
        assert_eq!(level_from_str(v), want, "{v} 应映射到 {want:?}");
    }
    // 大小写不敏感 + 未知值回落 normal（02 附录 B.1 缺省）
    assert_eq!(level_from_str("ULTRA"), Level::Ultra);
    assert_eq!(level_from_str("bogus"), Level::Normal);
}

// 输出命名：单选取源主名、多选取目录名；重名追加序号而不是覆盖。
// 这两条共同守住「静默覆盖用户既有档案」这类不可逆事故。
#[test]
fn default_out_path_follows_source_shape() {
    let d = tmp();
    let f = d.path().join("a.tar.gz");
    std::fs::write(&f, b"x").unwrap();
    assert_eq!(
        default_out_path(std::slice::from_ref(&f), "zip"),
        d.path().join("a.tar.zip"),
        "文件应取主名（a.tar.gz → a.tar）"
    );
    let sub = d.path().join("photos");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(
        default_out_path(std::slice::from_ref(&sub), "7z"),
        d.path().join("photos.7z"),
        "目录应取目录名"
    );
    assert_eq!(
        default_out_path(&[d.path().join("a.txt"), d.path().join("b.txt")], "zip"),
        d.path().join(format!(
            "{}.zip",
            d.path().file_name().unwrap().to_string_lossy()
        )),
        "多选应取首项所在目录名"
    );
}

// 逆映射往返：`level_index(level_from_str(s)) == ix`（参数窗回显靠它，
// 错位会让「配置写 maximum、界面显示 normal」这类静默错显）
#[test]
fn level_index_inverts_level_from_str() {
    use crate::app::labels::LEVEL;
    for (ix, (_, v)) in LEVEL.iter().enumerate() {
        assert_eq!(level_index(level_from_str(v)), ix, "{v} 逆映射应回到 {ix}");
    }
}

#[test]
fn unique_out_path_never_overwrites() {
    let d = tmp();
    let target = d.path().join("x.zip");
    assert_eq!(unique_out_path(&target), target, "不存在时原样返回");
    std::fs::write(&target, b"old").unwrap();
    assert_eq!(unique_out_path(&target), d.path().join("x_1.zip"));
    std::fs::write(d.path().join("x_1.zip"), b"old").unwrap();
    assert_eq!(unique_out_path(&target), d.path().join("x_2.zip"));
    // 原档案内容不得被改动
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
}

#[test]
fn compress_build_spec_defaults() {
    let d = tmp();
    let src = d.path().join("a.txt");
    std::fs::write(&src, b"x").expect("write");
    let out = d.path().join("a.tar");
    let vm = CompressDialogVm::new();
    let spec = vm.build_spec(&[src], &out).expect("spec");
    let c = spec.compress.expect("compress");
    assert_eq!(c.format.as_str(), DEFAULT_FORMAT);
    assert_eq!(c.level, Level::Normal);
    assert_eq!(c.split, None);
    assert!(c.exclude_globs.is_empty());
}

#[test]
fn compress_build_spec_rejects_missing_format_and_empty_inputs() {
    let d = tmp();
    let out = d.path().join("x.zip");
    let vm = CompressDialogVm {
        format: None,
        ..Default::default()
    };
    let e = vm.build_spec(&[d.path().join("a")], &out).unwrap_err();
    assert!(
        e.to_string().contains("hz-5003"),
        "缺 format 应报 5003 语义: {e}"
    );
    let vm2 = CompressDialogVm::new();
    let e2 = vm2.build_spec(&[], &out).unwrap_err();
    assert!(
        e2.to_string().contains("hz-5003"),
        "空 inputs 应报 5003 语义: {e2}"
    );
}

#[test]
fn extract_build_spec_maps_options() {
    let d = tmp();
    let archive = d.path().join("a.zip");
    std::fs::write(&archive, b"PK\x03\x04junk").expect("write"); // browse 前仅 submit 校验存在性
    let dest = d.path().join("dest");
    let vm = ExtractDialogVm {
        dest: Some(dest.clone()),
        smart: false,
        conflict: ConflictPolicy::Skip,
        codepage: Some(Encoding::Gbk),
        best_effort: true,
        ..Default::default()
    };
    let spec = vm
        .build_spec(&archive, &SelectionArg::Names(vec!["a.txt".into()]))
        .expect("spec");
    assert_eq!(spec.kind, JobKind::Extract);
    let eo = spec.extract.expect("extract options");
    assert_eq!(eo.dest_root, Some(dest));
    assert!(!eo.smart);
    assert_eq!(eo.conflict, ConflictPolicy::Skip);
    assert_eq!(eo.codepage, Some(Encoding::Gbk));
    assert!(eo.best_effort);
    match eo.selection {
        Selection::Names(ns) => assert_eq!(ns.len(), 1),
        _ => panic!("Names 选择"),
    }
}

#[test]
fn extract_build_spec_all_selection_and_defaults() {
    let d = tmp();
    let archive = d.path().join("a.zip");
    std::fs::write(&archive, b"PK\x03\x04junk").expect("write");
    let vm = ExtractDialogVm::new();
    let spec = vm.build_spec(&archive, &SelectionArg::All).expect("spec");
    let eo = spec.extract.expect("extract options");
    assert!(matches!(eo.selection, Selection::All));
    assert!(!eo.smart, "默认恒原样直展");
    assert_eq!(eo.conflict, ConflictPolicy::Ask);
    assert!(eo.codepage.is_none());
    assert!(!eo.best_effort);
}

#[test]
fn extract_build_spec_rejects_missing_archive() {
    let d = tmp();
    let vm = ExtractDialogVm::new();
    let e = vm
        .build_spec(&d.path().join("missing.zip"), &SelectionArg::All)
        .unwrap_err();
    assert!(
        e.to_string().contains("hz-5003"),
        "缺 archive 应报 5003 语义: {e}"
    );
}

#[test]
fn extract_repaired_name_gbk() {
    // 修复预览语义（02 §11.4）：`repaired_name` 对「可解码名字」给出
    // codepage 修复对。注意：fmt 层交付的 `EntryName` 是 lossy UTF-8
    // 字符串（原始字节已丢）——GBK 字节级修复需 core 透传原始字节
    // （T32 encoding 的原始字节保留路径），本 VM 层只做表征层修复：
    // 纯 ASCII 直通 + 含替换符名不乱猜（挂跟踪表 TODO，T45 深化）。
    let vm = ExtractDialogVm {
        codepage: Some(Encoding::Gbk),
        ..Default::default()
    };
    // ASCII 名直通（GBK 与 ASCII 兼容区）
    let fixed = vm.repaired_name("plain.txt").expect("ascii 直通");
    assert_eq!(fixed, "plain.txt");
    // U+FFFD 替换符名（fmt lossy 产物）→ None（不乱猜，TC-CORE-016）
    let raw = "bad\u{FFFD}name.txt";
    assert!(vm.repaired_name(raw).is_none(), "lossy 名不乱猜");
    // 无 codepage → None
    let vm2 = ExtractDialogVm::new();
    assert!(vm2.repaired_name("plain.txt").is_none());
}

// ── 09 压缩对话框：派生量与默认值互锁 ──

/// **默认值互锁**：VM 默认 = `CompressSpec` 默认（仅 `format`/`out` 两处为
/// 对话框语义覆盖）。
///
/// 守的是「两处各写一份默认值」漂移：`verify_after`/`keep_metadata` 在 spec 侧
/// 默认**开**，VM 若改回 `#[derive(Default)]` 就会变成关——用户什么都没动，
/// 压缩却不再自检、也不再保留文件时间戳。
#[test]
fn compress_vm_defaults_mirror_spec_defaults() {
    let vm = CompressDialogVm::default();
    let s = CompressSpec::default();
    assert_eq!(vm.level, s.level);
    assert_eq!(vm.method, s.method);
    assert_eq!(vm.threads, s.threads);
    assert_eq!(vm.solid, s.solid);
    assert_eq!(vm.split, s.split);
    assert_eq!(vm.exclude_hidden, s.exclude_hidden);
    assert_eq!(vm.follow_symlinks, s.follow_symlinks);
    assert_eq!(vm.keep_metadata, s.keep_metadata);
    assert_eq!(vm.verify_after, s.verify_after);
    assert_eq!(vm.best_effort, s.best_effort);
    // 故意的两处对话框语义覆盖
    assert!(vm.format.is_none(), "未选格式（spec 默认为 tar）");
    assert!(vm.out.is_none(), "未填输出（spec 默认为空路径）");
    // spec 侧默认开的两项必须仍为开（回归护栏）
    assert!(vm.keep_metadata, "保留文件属性默认开");
    assert!(vm.verify_after, "完成后校验默认开");
}

/// 格式 → 扩展名，以及单流格式判定（扩展名 chip 与「多文件提示」的数据源）。
#[test]
fn default_ext_and_stream_formats() {
    assert_eq!(default_ext("zip"), "zip");
    assert_eq!(default_ext("7z"), "7z");
    assert_eq!(default_ext("tar"), "tar");
    assert_eq!(default_ext("gz"), "gz");
    assert_eq!(default_ext("bz2"), "bz2");
    assert_eq!(default_ext("xz"), "xz");
    assert_eq!(default_ext("zst"), "zst");
    assert_eq!(default_ext("bogus"), "zip", "白名单外回落 zip");
    // 单流格式 = 恰一条目语义（多文件/目录须给出提示）
    for f in ["gz", "bz2", "xz", "zst"] {
        assert!(is_stream_format(f), "{f} 应为单流格式");
    }
    for f in ["zip", "7z", "tar"] {
        assert!(!is_stream_format(f), "{f} 可承载多条目");
    }
}

/// 文件名非法字符（Windows 保留集）——提交前挡在对话框内。
#[test]
fn invalid_name_char_detects_windows_reserved() {
    for c in ['\\', '/', ':', '*', '?', '"', '<', '>', '|'] {
        let n = format!("a{c}b");
        assert_eq!(invalid_name_char(&n), Some(c), "{n} 含非法字符 {c}");
    }
    assert_eq!(invalid_name_char("project_2024-09.tar"), None);
    assert_eq!(invalid_name_char("中文名 带空格"), None);
    assert_eq!(invalid_name_char(""), None);
}

/// 分卷：预设 value → 字节，自定义文本按 **MB** 换算；`0`/空 = 不分卷。
#[test]
fn split_bytes_from_preset_and_custom() {
    assert_eq!(split_preset_bytes("none"), None);
    assert_eq!(split_preset_bytes("custom"), None);
    assert_eq!(split_preset_bytes("bogus"), None);
    assert_eq!(split_preset_bytes("10m"), Some(10 * 1024 * 1024));
    assert_eq!(split_preset_bytes("100m"), Some(100 * 1024 * 1024));
    assert_eq!(split_preset_bytes("700m"), Some(700 * 1024 * 1024));
    assert_eq!(split_preset_bytes("1g"), Some(1024 * 1024 * 1024));
    assert_eq!(split_preset_bytes("4g"), Some(4096 * 1024 * 1024));

    // 自定义：单位 MB
    assert_eq!(split_custom_bytes("45"), Some(45 * 1024 * 1024));
    assert_eq!(split_custom_bytes("  100 "), Some(100 * 1024 * 1024));
    assert_eq!(split_custom_bytes("0"), None, "0 = 不分卷");
    assert_eq!(split_custom_bytes(""), None);
    assert_eq!(split_custom_bytes("abc"), None);
}

/// 级别实参 == `hzip_codec::level` 映射表（界面显示的字典/级别必须等于实际
/// 写入时用的那一个）。
#[test]
fn level_detail_follows_codec_tables() {
    use hzip_core::codec_consts as consts;
    use hzip_core::level as lv;

    for level in [Level::Store, Level::Fastest, Level::Normal, Level::Ultra] {
        assert_eq!(
            level_detail("zip", level),
            LevelDetail::Deflate {
                level: lv::deflate_level(level)
            }
        );
        // gzip 容器内部同为 deflate
        assert_eq!(
            level_detail("gz", level),
            LevelDetail::Deflate {
                level: lv::deflate_level(level)
            }
        );
        let p = lv::lzma2_params(level);
        assert_eq!(
            level_detail("7z", level),
            LevelDetail::Lzma2 {
                dict: p.dict,
                mode: p.mode
            }
        );
        assert_eq!(
            level_detail("xz", level),
            LevelDetail::Xz {
                preset: lv::xz_preset(level),
                extreme: lv::xz_extreme(level)
            }
        );
        assert_eq!(
            level_detail("zst", level),
            LevelDetail::Zstd {
                level: lv::zstd_level(level)
            }
        );
        assert_eq!(
            level_detail("bz2", level),
            LevelDetail::BZip2 {
                block: lv::bzip2_level(level)
            }
        );
        assert_eq!(level_detail("tar", level), LevelDetail::None);
    }

    // 抽样钉死具体数值（表本身改了也能对上；这里防的是「档位错位」）
    assert_eq!(
        level_detail("7z", Level::Normal),
        LevelDetail::Lzma2 {
            dict: consts::LZMA2_DICT_NORMAL,
            mode: lv::Mode::Normal
        }
    );
    assert_eq!(
        level_detail("7z", Level::Ultra),
        LevelDetail::Lzma2 {
            dict: consts::LZMA2_DICT_ULTRA,
            mode: lv::Mode::Normal
        }
    );
    assert_eq!(
        level_detail("7z", Level::Store),
        LevelDetail::Lzma2 {
            dict: consts::LZMA2_DICT_STORE,
            mode: lv::Mode::Copy
        }
    );
    assert_eq!(
        level_detail("zst", Level::Ultra),
        LevelDetail::Zstd { level: 22 }
    );
    // 极限档是 xz 唯一置 extreme 的档位
    assert_eq!(
        level_detail("xz", Level::Ultra),
        LevelDetail::Xz {
            preset: 9,
            extreme: true
        }
    );
    assert!(!matches!(
        level_detail("xz", Level::Maximum),
        LevelDetail::Xz { extreme: true, .. }
    ));
}

/// `build_spec` 把 09 新增字段逐项落进 `CompressSpec`（含加密三件套）。
#[test]
fn compress_build_spec_carries_new_fields() {
    let d = tmp();
    let src = d.path().join("a.txt");
    std::fs::write(&src, b"x").expect("write");
    let out = d.path().join("a.7z");
    let vm = CompressDialogVm {
        format: Some(FormatId::from("7z")),
        method: Some(CodecMethod::Lzma2),
        level: Level::Maximum,
        split: Some(100 * 1024 * 1024),
        threads: 4,
        dict_size: Some(64 * 1024 * 1024),
        solid: true,
        append: true,
        excludes: vec!["*.tmp".into()],
        exclude_hidden: true,
        follow_symlinks: true,
        keep_metadata: false,
        verify_after: false,
        best_effort: true,
        // 密码/加密方式给了值：写侧已接通，必须**成对**进 spec
        password: Some(Zeroizing::new("secret".into())),
        encryption: Some(EncryptionRequest::ZipCrypto),
        encrypt_names: false,
        out: Some(out.clone()),
    };
    let spec = vm.build_spec(&[src], &out).expect("spec");
    let c = spec.compress.expect("compress spec");
    assert_eq!(c.method, Some(CodecMethod::Lzma2));
    assert_eq!(c.level, Level::Maximum);
    assert_eq!(c.split, Some(100 * 1024 * 1024));
    assert_eq!(c.threads, 4);
    assert_eq!(c.dict_size, Some(64 * 1024 * 1024), "字典覆写应进 spec");
    assert!(c.solid);
    assert!(c.append, "更新方式=追加应进 spec");
    assert_eq!(c.exclude_globs, vec!["*.tmp"]);
    assert!(c.exclude_hidden);
    assert!(c.follow_symlinks);
    assert!(!c.keep_metadata);
    assert!(!c.verify_after);
    assert!(c.best_effort);
    assert_eq!(
        c.encryption,
        Some(EncryptionRequest::ZipCrypto),
        "加密方式必须进 spec（core writer_for 按 caps 校验）"
    );
    assert_eq!(
        c.password.as_ref().map(|z| z.as_str()),
        Some("secret"),
        "密码必须与加密方式成对进 spec（Zeroizing 透传）"
    );
    assert_eq!(c.encrypt_names, Some(false), "7z 加密文件名开关进 spec");
    assert!(c.comment.is_none());
}

/// 字典白名单 value → 字节；`auto`/未知 → `None`（随级别派生）。
#[test]
fn dict_from_str_maps_whitelist() {
    assert_eq!(dict_from_str("auto"), None);
    assert_eq!(
        dict_from_str("bogus"),
        None,
        "未知回落「随级别」（安全默认）"
    );
    assert_eq!(dict_from_str(""), None);
    assert_eq!(dict_from_str("64k"), Some(64 * 1024));
    assert_eq!(dict_from_str("1m"), Some(1024 * 1024));
    assert_eq!(dict_from_str("4m"), Some(4 * 1024 * 1024));
    assert_eq!(dict_from_str("16m"), Some(16 * 1024 * 1024));
    assert_eq!(dict_from_str("64m"), Some(64 * 1024 * 1024));
    assert_eq!(dict_from_str("256m"), Some(256 * 1024 * 1024));
}

/// **字典档位与白名单表逐个对上**（表加/改档位而换算漏改即红）。
#[test]
fn dict_table_and_conversion_agree() {
    for (_, v) in crate::app::labels::DICT {
        assert_eq!(
            dict_from_str(v).is_some(),
            v != "auto",
            "{v}：除 auto 外每档都必须有字节值"
        );
    }
}

/// 更新方式：**只有** `append` 是追加——未知值必须回落「新建」。
///
/// 守的是一类不可逆事故：配置写坏 / 新版本降级读旧值时把未知值当追加处理，
/// 就会去改动用户既有档案。默认必须落在最保守的那一支。
#[test]
fn append_from_str_defaults_to_new() {
    assert!(append_from_str("append"));
    for v in ["new", "", "bogus", "APPEND", "true", "1"] {
        assert!(!append_from_str(v), "{v:?} 必须回落「新建」而不是追加");
    }
}
