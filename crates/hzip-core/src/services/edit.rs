//! 编辑保存服务（02 §9.9 时序八步；T33 独占）。
//!
//! # 时序（§9.9 图逐步）
//!
//! ```text
//! ① open_entry_for_edit：registry.open_reader → read_entry 物化到 TempArea
//!    稳定路径（{root}/{档案 sha256 前 8}/{条目安全名}——经与解压相同净化，
//!    TC-SEC-015）→ watch(callback) 注册变更（去抖 EDIT_DEBOUNCE_MS=500ms）
//! ② UI 拿路径自调 open_with_default_app（本服务不自动打开——图注）
//! ③ 变更事件（去抖合并多次保存，TC-CORE-019）→ 会话通道
//! ④ 确认请求：归 UI 会话边界（02 §9.11 JobRequest 只冻结 Password/Conflict；
//!    编辑确认不走 JobRequest——登记裁定，GUI 直接调 save_back）
//! ⑤ ChangeSet::Replace{ entry, src: tmp_path }
//! ⑥ editor.rewrite(changes, Sink::File({原档案}.hzip-tmp-{uuid}))——同目录同卷
//! ⑦ fs_util::atomic_replace(tmp, 原档案)——同卷 rename 快路径
//! ⑧ 失败保护：rewrite/replace 任一 Err → 删临时档案 + 原档案字节不变
//!    （前后 sha256 相等，TC-CONF-009）；成功 → EditDiff 报告
//! ```
//!
//! # 边界与裁定（rustdoc）
//!
//! - **`caps.edit` 前置**：目标格式无编辑能力（tar/single/rar）→ `Unsupported`
//!   HZ-5001（open_editor 恒拒）；
//! - **组合格式不支持编辑**（registry 语义，02 §7.5）；
//! - **原子性**由本服务保证（fmt `rewrite` 只写 dst 不触 src）；`{原档案}.
//!   hzip-tmp-{uuid}` 与原文**同目录**（atomic_replace 同卷 rename）；
//! - **TempArea 生命周期**：EditSession drop → TempArea cleanup（T11 状态机
//!   Cleaning→[*]）；进程崩溃残留由启动孤儿清理兜底（本任务不重复实现）。
//! - 多条目并发编辑：本会话单条目（ChangeSet 可含多个 Replace——批量回写
//!   语义由 `rewrite` 原生支持，02 §7.3 `changes: Vec<Change>`）。
//! - **密码档案**（用户口径 2026-09-28）：读加密条目 / 头加密档案（HZ-3102）
//!   与写侧重加密共用同一密码——来源 = [`JobSpec::password`]（GUI 参数窗/会话
//!   缓存预填），缺省经 `ctx.need(JobRequest::Password{ entry: None })` 上抛询问
//!   （≤ `PASSWORD_MAX_RETRIES`，错则 `WrongPassword` 重问；与 extract 的
//!   `resolve_early_password`/`PwRouter` 同口径）。密码经 `OpenOptions::password`
//!   交给 fmt（读端），写侧档案级加密经 [`CreateOptions::encryption`] 按源条目
//!   推导（逐条目写密码由编辑器自持——zip 保真拷贝）。
//!
//! 出处：02 §9.9 时序图、§7.3 ChangeSet/Change/ArchiveEditor 契约、
//! 02 §5.3.4 atomic_replace、§5.3.5 TempArea、B.2 EDIT_DEBOUNCE_MS。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use hzip_error::HzipError;
use hzip_format::{
    AesKey, ArchiveEditor, Change, ChangeSet, CreateOptions, EncryptionRequest, EntryId, EntryInfo,
    PasswordRef, Sink, Source, WriteCtx, WriteReport,
};
use hzip_platform::temp_area::{EntryNameSafe, TempArea};
use hzip_progress::Outcome;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::jobs::{JobCtx, JobRequest, JobResponse, JobSpec, PasswordReason};
use crate::report;
use crate::services::extract::PASSWORD_MAX_RETRIES;

/// 编辑会话（§9.9 步 1~3 产物：物化 + watch 已就绪）。
///
/// GUI 会话态编辑 API（open_entry_for_edit → 外部编辑器 → save_back）——T40
/// GUI 消费前保持 allow(dead_code)；JobKind::EditSave 直存路径已全链测试。
#[allow(dead_code)] // T40 GUI 会话态消费面
pub(crate) struct EditSession {
    archive: PathBuf,
    /// 目标条目（entries 下标）
    entry: EntryId,
    /// 物化临时文件路径
    tmp_path: PathBuf,
    /// TempArea 会话（drop → cleanup）
    area: TempArea,
    /// 变更事件接收端（watch 去抖回调 → 本通道）
    changed: mpsc::Receiver<()>,
    /// 变更已发生标志
    dirty: AtomicBool,
}

/// JobKind::EditSave 实现体（02 §9.9 步 4~8 的直调封装）。
///
/// 编辑流为 UI 会话内交互（GUI 调 [`open_entry_for_edit`] + [`EditSession::save_back`]，
/// CLI 无编辑流）——JobKind::EditSave 经本函数把「直接保存当前物化文件」
/// 收敛为 JobOutcome（服务层占位调用面，测试直调 save_back 为主路径）。
pub(crate) fn run_edit_save(ctx: &JobCtx, spec: &JobSpec) -> Outcome {
    match save_direct(ctx, spec) {
        Ok(outcome) => outcome,
        Err(HzipError::Cancelled) => Outcome::Cancelled,
        Err(e) => Outcome::Failed(e),
    }
}

/// JobKind::EditBatch 实现体（T-MENU6：ChangeSet 批量提交入口）。
///
/// 与 [`save_direct`] 同链路：open_editor（caps.edit 前置；组合/无编辑
/// 能力格式 Unsupported）→ rewrite 到同卷临时档案 → atomic_replace。
/// 变更集由 GUI 装配（Append/Delete/Replace；`#[non_exhaustive]` 未知
/// 变体由 fmt 层忽略）。取消/进度经 WriteCtx 贯通（进度窗/报告复用
/// EditSave 既有事件流）。
///
/// JobSpec 载体语义：`source` = 被编辑档案；`changes` = 变更集；
/// `password` = 加密档案密码（缺省经 `ctx.need` 上抛询问）；其余字段忽略。
pub(crate) fn run_edit_batch(ctx: &JobCtx, spec: &JobSpec) -> Outcome {
    match rewrite_in_place(
        ctx,
        spec.source.as_path(),
        spec.changes.clone().unwrap_or_default(),
        spec.password.as_ref(),
    ) {
        Ok(outcome) => outcome,
        Err(HzipError::Cancelled) => Outcome::Cancelled,
        Err(e) => Outcome::Failed(e),
    }
}

/// rewrite + 原子替换公共体（save_direct 与 run_edit_batch 收口）。
///
/// 密码档案（用户口径 2026-09-28）：读加密条目与重写共用同一密码——`seed`
/// （[`JobSpec::password`]）优先；缺省/被 fmt 层判错时经 `ctx.need` 上抛询问
/// （≤ [`PASSWORD_MAX_RETRIES`]，见 [`PwPrompter`]）。写侧选项由源条目表推导
/// （[`write_opts_for`]：档案级加密与「加密文件名」保持）。
fn rewrite_in_place(
    ctx: &JobCtx,
    archive: &Path,
    changes: ChangeSet,
    seed: Option<&Zeroizing<String>>,
) -> Result<Outcome, HzipError> {
    rewrite_and_replace(ctx, archive, archive, seed, &|| changes.clone(), &|infos| {
        infos.first().cloned().ok_or_else(bad_header)
    })
}

/// JobKind::EditSave 直存路径（02 §9.9 步 5~8 的整档直调形态）。
///
/// JobSpec 载体语义：`source` = 被编辑档案（须存在）；`dest` = 变更后的内容
/// 文件（物化编辑产物——外部编辑器输出）；`out` = None → 原档案原地替换。
/// 编辑确认归 UI 会话边界（本直调形态即确认动作；02 §9.11 裁定 rustdoc 见模块
/// 文档）。Replace 目标 = 档案条目 0（整档单文件编辑）；条目级编辑走
/// [`open_entry_for_edit`] + [`EditSession::save_back`]（GUI 会话态）。
fn save_direct(ctx: &JobCtx, spec: &JobSpec) -> Result<Outcome, HzipError> {
    let archive = spec.source.clone();
    let new_content = spec.dest.clone().ok_or_else(|| HzipError::Unsupported {
        what: "hz-5003 edit-save 缺变更内容（JobSpec.dest）".into(),
    })?;
    if !new_content.is_file() {
        return Err(HzipError::Io {
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "变更内容不是文件"),
            path: Some(new_content),
        });
    }
    let target = spec.out.clone().unwrap_or_else(|| archive.clone());
    // Replace 目标 = 档案条目 0（整档单文件编辑）
    let changes = ChangeSet {
        changes: vec![Change::Replace {
            entry: 0,
            src: new_content,
        }],
    };
    rewrite_and_replace(
        ctx,
        &archive,
        &target,
        spec.password.as_ref(),
        &|| changes.clone(),
        &|infos| infos.first().cloned().ok_or_else(bad_header),
    )
}

/// 档案头/条目表不可用的统一错误（沿用既有口径）。
fn bad_header() -> HzipError {
    HzipError::Format {
        format: hzip_format::FormatId::from("?"),
        kind: hzip_error::FormatErrorKind::BadHeader,
    }
}

/// 「列条目 → 按源推导写选项 → rewrite 到同卷临时档 → 原子替换」全链。
///
/// 密码询问链贯通：列条目被拒（头加密 HZ-3102）或含加密条目而手上无密码
/// （[`PwPrompter`]）→ 上抛询问；`rewrite` 返回 `Password` 错（预填密码不对/
/// 猜错）→ `WrongPassword` 重问后重试整轮。`changes_for` 每轮重建变更集
/// （重试不复用上一轮产物）。
fn rewrite_and_replace(
    ctx: &JobCtx,
    src_archive: &Path,
    target: &Path,
    seed: Option<&Zeroizing<String>>,
    changes_for: &dyn Fn() -> ChangeSet,
    before_of: &dyn Fn(&[EntryInfo]) -> Result<EntryInfo, HzipError>,
) -> Result<Outcome, HzipError> {
    let mut prompter = PwPrompter::new(ctx, seed);
    loop {
        let infos = prompter.entries(src_archive)?;
        let before = before_of(&infos)?;
        // 卷容器（磁盘镜像）+ 单格式（非组合链）⇒ **就地写**：不产生「重写整档」的
        // 全量副本（40 GB 镜像不可接受），也不做临时档 + 原子替换——原子性由格式自持
        // （覆盖层暂存 → 提交回放；提交前原档字节不变，见 02 附录 C.4）。
        // 其余格式（含组合链）照旧 rewrite + 原子替换。
        if in_place_supported(ctx, src_archive) {
            let result = {
                let editor = open_editor(src_archive, ctx, prompter.password())?;
                let opts = write_opts_for(&infos);
                let wctx = WriteCtx {
                    cancel: &ctx.cancel,
                    progress: ctx.progress(),
                    opts: &opts,
                };
                editor.write_in_place(changes_for(), &wctx)
            };
            // 密码被 fmt 层判错 → 重问重试（与 rewrite 分支同口径）
            if matches!(&result, Err(HzipError::Password { .. })) {
                prompter.ask()?;
                continue;
            }
            return finish_in_place(src_archive, result, &before);
        }
        // rewrite 用独立作用域：完成后 editor 立即 drop（其源 reader 句柄关闭——
        // Windows 不可替换仍被打开的文件；02 §9.9 步 ⑦ 前置）
        let (result, tmp_archive) = {
            let editor = open_editor(src_archive, ctx, prompter.password())?;
            let tmp_archive = tmp_archive_path(target);
            let opts = write_opts_for(&infos);
            let wctx = WriteCtx {
                cancel: &ctx.cancel,
                progress: ctx.progress(),
                opts: &opts,
            };
            let result = editor.rewrite(changes_for(), Sink::File(tmp_archive.clone()), &wctx);
            (result, tmp_archive)
        };
        // 密码被 fmt 层判错 → 清理临时档 + 重问重试（超限 → Password 错收敛）
        if matches!(&result, Err(HzipError::Password { .. })) {
            let _ = std::fs::remove_file(&tmp_archive);
            prompter.ask()?;
            continue;
        }
        return finish_replace(target, &tmp_archive, result, &before);
    }
}

/// 该源是否走「**就地写**」：**单格式**（非组合链）+ 源是常规文件 + handler 声明可
/// 就地编辑。
///
/// 组合链（`.qcow2.gz` 之类）不再此列：外层必须重写，就地写无意义（02 附录 C.4）；
/// 也正因如此，`Sniffed::Composite` 一律走 `rewrite` + 原子替换。
fn in_place_supported(ctx: &JobCtx, src: &Path) -> bool {
    if !std::fs::metadata(src).map(|m| m.is_file()).unwrap_or(false) {
        return false;
    }
    match ctx.registry.sniff(&Source::File(src.to_path_buf())) {
        Ok(hzip_format::Sniffed::Single(id, _)) => ctx
            .registry
            .handler(id.as_str())
            .is_some_and(|h| h.supports_in_place_edit()),
        _ => false,
    }
}

/// 就地写收尾：与 [`finish_replace`] 同款报告，但**无临时档、无原子替换**——原子性
/// 由格式自持（覆盖层暂存 → 提交回放）；失败时原档按契约不变。
fn finish_in_place(
    archive: &Path,
    result: Result<WriteReport, HzipError>,
    before: &EntryInfo,
) -> Result<Outcome, HzipError> {
    let wreport = match result {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(path = %archive.display(), "就地写失败（原档按契约不变）：{e}");
            return Err(e);
        }
    };
    let diff = report::edit_diff(before, &wreport);
    let mut stats = report::stats_from_write(&wreport);
    stats.finished = Some(std::time::Instant::now());
    tracing::debug!(?diff, "编辑保存完成（就地写）");
    Ok(Outcome::Done(stats))
}

/// 写侧选项（按源档案条目推导）；未加密档案 → 全默认（`Level::Normal` 语义保留）。
/// - 档案级加密：取**首条加密条目**的算法（[`EntryInfo::encryption`] 的
///   `&'static str` 口径 → [`EncryptionRequest`]），逐条目差异由 fmt 编辑器
///   自行保真；
/// - 「加密文件名」（7z `-mhe`）：沿用源头部加密标志（`Some(false)` = 显式
///   不加密，`None` 会落格式默认——故未加密档案两者都不置）。
fn write_opts_for(infos: &[EntryInfo]) -> CreateOptions {
    let mut opts = CreateOptions::default();
    let Some(first_encrypted) = infos.iter().find(|i| i.encrypted) else {
        return opts; // 未加密档案：encryption/encrypt_names 保持 None
    };
    opts.encryption = encryption_request(first_encrypted.encryption);
    opts.encrypt_names = Some(infos.first().map(|i| i.header_encrypted).unwrap_or(false));
    opts
}

/// `EntryInfo.encryption`（`&'static str`）→ [`EncryptionRequest`]；
/// 未知/缺省 → `None`（不猜算法）。
fn encryption_request(alg: Option<&'static str>) -> Option<EncryptionRequest> {
    match alg {
        // "AES" 无长度信息 → 取最长（安全默认）
        Some("AES-256") | Some("AES") => Some(EncryptionRequest::Aes(AesKey::Bits256)),
        Some("AES-192") => Some(EncryptionRequest::Aes(AesKey::Bits192)),
        Some("AES-128") => Some(EncryptionRequest::Aes(AesKey::Bits128)),
        Some("ZipCrypto") => Some(EncryptionRequest::ZipCrypto),
        _ => None,
    }
}

/// 档案密码取用器（读加密条目 / 头加密 7z 与写侧共用同一密码）。
///
/// 口径与 extract 的 `resolve_early_password`/`PwRouter` 一致：`seed`
/// （[`JobSpec::password`]）优先且零弹窗；需要时经 `ctx.need` 上抛询问
/// （≤ [`PASSWORD_MAX_RETRIES`]，`Initial`/`HeaderEncrypted` → `WrongPassword`
/// 重问），超限收敛 `Password` 错（HZ-3101）。
struct PwPrompter<'a> {
    ctx: &'a JobCtx,
    pw: Option<Zeroizing<String>>,
    /// 已上抛次数（超限即收敛）
    asks: u32,
    /// 下一次询问原因
    reason: PasswordReason,
}

impl<'a> PwPrompter<'a> {
    fn new(ctx: &'a JobCtx, seed: Option<&Zeroizing<String>>) -> Self {
        Self {
            ctx,
            pw: seed.cloned(),
            asks: 0,
            // 参数窗预填密码优先：首个重问按「密码错误」呈现（别让用户以为没试过）
            reason: if seed.is_some() {
                PasswordReason::WrongPassword
            } else {
                PasswordReason::HeaderEncrypted
            },
        }
    }

    /// 当前密码（供 `OpenOptions` / 编辑器逐条目写）。
    fn password(&self) -> Option<&Zeroizing<String>> {
        self.pw.as_ref()
    }

    /// 上抛一次密码询问；超限 → `Password` 错（HZ-3101）。
    fn ask(&mut self) -> Result<(), HzipError> {
        if self.asks >= PASSWORD_MAX_RETRIES {
            return Err(HzipError::Password {
                target: hzip_error::PasswordTarget::Archive,
            });
        }
        self.asks += 1;
        if let JobResponse::Password(p) = self.ctx.need(JobRequest::Password {
            entry: None,
            reason: self.reason,
        })? {
            self.pw = Some(p);
        }
        self.reason = PasswordReason::WrongPassword;
        Ok(())
    }

    /// 列条目 + 密码齐备：打开被拒（头加密）或含加密条目而缺密码 → 询问后重试。
    fn entries(&mut self, archive: &Path) -> Result<Vec<EntryInfo>, HzipError> {
        loop {
            match editor_entries(archive, self.ctx, self.pw.as_ref()) {
                Ok(infos) => {
                    let needs_pw = infos.iter().any(|i| i.encrypted || i.header_encrypted);
                    if needs_pw && self.pw.is_none() {
                        self.ask()?;
                        continue;
                    }
                    return Ok(infos);
                }
                Err(HzipError::Password { .. }) => self.ask()?,
                Err(e) => return Err(e),
            }
        }
    }
}

/// 打开编辑器（caps.edit 前置；组合格式 Unsupported）。
///
/// `password` 经 [`OpenOptions`] 交给 fmt（头加密档案打开 / 加密条目读取
/// 所需；zip 编辑器亦自此取写侧密码）。
fn open_editor(
    archive: &Path,
    ctx: &JobCtx,
    password: Option<&Zeroizing<String>>,
) -> Result<Box<dyn ArchiveEditor>, HzipError> {
    ctx.registry.open_editor(
        Source::File(archive.to_path_buf()),
        &hzip_format::OpenOptions {
            password: password.cloned(),
            ..Default::default()
        },
    )
}

/// 编辑器条目表（读取用；头加密档案经 `password` 解头，否则 HZ-3102）。
fn editor_entries(
    archive: &Path,
    ctx: &JobCtx,
    password: Option<&Zeroizing<String>>,
) -> Result<Vec<EntryInfo>, HzipError> {
    let reader = ctx.registry.open_reader(
        Source::File(archive.to_path_buf()),
        &hzip_format::OpenOptions {
            password: password.cloned(),
            ..Default::default()
        },
    )?;
    reader.entries()
}

/// 时序步 1~3：打开编辑会话（物化 + watch + 返回临时路径）。
///
/// `ctx` 用于打开 reader/密码通道；`password` = 会话密码（读加密条目用；
/// `None` → 由调用方先经 [`PwPrompter`] 链取齐）。物化路径 = TempArea 稳定
/// 分配（`{session}/{sha256(档案)[..8]}/{净化条目名}`——TC-SEC-015）。
#[allow(dead_code)] // T40 GUI 会话态消费面
pub(crate) fn open_entry_for_edit(
    archive: &Path,
    entry: EntryId,
    ctx: &JobCtx,
    password: Option<&Zeroizing<String>>,
) -> Result<EditSession, HzipError> {
    let _editor = open_editor(archive, ctx, password)?;
    let infos = editor_entries(archive, ctx, password)?;
    let info = infos.get(entry).ok_or_else(bad_header)?;
    let reader = ctx.registry.open_reader(
        Source::File(archive.to_path_buf()),
        &hzip_format::OpenOptions {
            password: password.cloned(),
            ..Default::default()
        },
    )?;
    let pw_ref = match password {
        Some(pw) => PasswordRef::new(pw),
        None => PasswordRef::none(),
    };
    let mut stream = reader.read_entry(entry, pw_ref)?;
    let area = TempArea::new_session(None).map_err(HzipError::from)?;
    let safe = EntryNameSafe::sanitize(&entry_safe_name(info)).map_err(HzipError::from)?;
    let tmp_path = area
        .materialize(archive, &safe, &mut stream)
        .map_err(HzipError::from)?;
    let (tx, rx) = mpsc::channel::<()>();
    let tx2 = tx.clone();
    area.watch(Box::new(move |_p| {
        let _ = tx2.send(());
    }))
    .map_err(HzipError::from)?;
    Ok(EditSession {
        archive: archive.to_path_buf(),
        entry,
        tmp_path,
        area,
        changed: rx,
        dirty: AtomicBool::new(false),
    })
}

/// 物化安全名（条目末段 + 净化；目录条目补 `_dir` 后缀防碰撞）。
#[allow(dead_code)] // open_entry_for_edit 消费面
fn entry_safe_name(info: &EntryInfo) -> String {
    let base = Path::new(info.name.as_str())
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "entry".into());
    if info.is_dir {
        format!("{base}_dir")
    } else {
        base
    }
}

impl EditSession {
    /// 物化临时文件路径（UI 调 open_with_default_app 的目标）。
    #[allow(dead_code)] // T40 GUI 会话态消费面
    pub(crate) fn tmp_path(&self) -> &Path {
        &self.tmp_path
    }

    /// 变更是否已发生（watch 去抖合并；TC-CORE-019 语义）。
    #[allow(dead_code)] // T40 GUI 会话态消费面
    pub(crate) fn poll_changed(&self) -> bool {
        if let Ok(()) = self.changed.try_recv() {
            self.dirty.store(true, Ordering::Relaxed);
        }
        self.dirty.load(Ordering::Relaxed)
    }

    /// 时序步 4~8：保存回档案（确认已由 UI 会话完成——本方法即确认动作）。
    ///
    /// `password` = 会话密码（加密档案重写用；`None` → 经 `ctx.need` 上抛询问）。
    #[allow(dead_code)] // T40 GUI 会话态消费面
    pub(crate) fn save_back(
        &self,
        ctx: &JobCtx,
        password: Option<&Zeroizing<String>>,
    ) -> Result<Outcome, HzipError> {
        let archive = self.archive.clone();
        let entry = self.entry;
        // ⑤ ChangeSet::Replace（id 引用；src = 物化文件）
        let changes = ChangeSet {
            changes: vec![Change::Replace {
                entry,
                src: self.tmp_path.clone(),
            }],
        };
        // ⑥⑦⑧ rewrite → {原档案}.hzip-tmp-{uuid}（同目录）→ 原子替换
        rewrite_and_replace(
            ctx,
            &archive,
            &archive,
            password,
            &|| changes.clone(),
            &|infos| infos.get(entry).cloned().ok_or_else(bad_header),
        )
    }
}

/// 临时档案路径（同目录同卷；uuid v4）。
fn tmp_archive_path(archive: &Path) -> PathBuf {
    let name = archive
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    let dir = archive
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    dir.join(format!("{name}.hzip-tmp-{}", Uuid::new_v4()))
}

/// ⑦⑧：atomic_replace 成功 → Outcome；失败 → 原档不变 + 清理临时档案。
fn finish_replace(
    archive: &Path,
    tmp_archive: &Path,
    rewrite_result: Result<WriteReport, HzipError>,
    before: &EntryInfo,
) -> Result<Outcome, HzipError> {
    let wreport = match rewrite_result {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(tmp_archive); // 失败临时清理（best-effort）
            return Err(e);
        }
    };
    // atomic_replace（同卷 rename；跨卷回退归 T11 fs_util）
    hzip_platform::fs_util::atomic_replace(tmp_archive, archive).map_err(|e| {
        let _ = std::fs::remove_file(tmp_archive);
        HzipError::Io {
            source: e,
            path: Some(archive.to_path_buf()),
        }
    })?;
    // 成功：diff 报告
    let diff = report::edit_diff(before, &wreport);
    let mut stats = report::stats_from_write(&wreport);
    stats.finished = Some(std::time::Instant::now());
    tracing::debug!(?diff, "编辑保存完成（EditDiff）");
    Ok(Outcome::Done(stats))
}

#[cfg(test)]
#[path = "tests/edit.rs"]
mod tests;
