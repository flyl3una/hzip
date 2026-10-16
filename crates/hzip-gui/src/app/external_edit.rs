//! 外部编辑器编辑会话：「双击用本地程序打开 → 保存 → 写回档案」。
//!
//! # 为什么要补这条链（用户实测 2026-10-16）
//!
//! 用户打开 `.qcow2.gz` 里的文件、用系统文本编辑器改完保存，**什么都没发生**——
//! 此前「打开方式/双击」只把条目物化到进程临时区并起外部程序，**没有任何写回通路**
//! （`JobKind::EditSave` 在 GUI 里从未被构造过，core 侧注释也写着「GUI 消费前保持
//! allow(dead_code)」）。物化路径是稳定的（`{复制区}/{档案哈希}/{安全名}`），所以
//! 「监听那个临时文件 → 变了就写回」是可行且自然的落地形态。
//!
//! # 链路
//!
//! ```text
//! 双击条目 ──物化──▶ {temp} ──起外部程序──▶ 编辑器
//!                    │
//!                    └─ Arm：登记会话 + FileWatch(temp)（去抖 500 ms）
//! 编辑器保存 ──▶ EditEvent::Saved{stat} ──▶ 主线程泵批处理
//!                    │
//!                    └─ SaveAction::Submit ⇒ JobSpec(EditBatch, Replace{entry, src=temp})
//! ```
//!
//! # 为什么是 `EditBatch + Change::Replace` 而不是 `JobKind::EditSave`
//!
//! `EditSave` 的直存形态固定「档案条目 0 = 整档单文件」（gz/tar 单成员语义）——
//! 卷容器里条目 0 是**分区行**，那条路会指错目标。`Change::Replace{entry}` 带的是
//! **读侧投影的条目 id**，正是用户点的那一条；卷容器的就地写路由（`hzip-vol` 的
//! `route_change`）也认这个口径。
//!
//! # 边界（如实拒绝，绝不猜）
//!
//! | 情形 | 处置 |
//! |---|---|
//! | **嵌套会话**（进了内层容器，含 `.qcow2.gz` 这类组合链） | **不提交任务**：临时文件属于内层档案的条目空间，拿它的 id 去改外层档案会写错东西。保存时告知「不会写回」（等 T-IMG0 的组合链写回） |
//! | 非可编辑格式（`.tar`、只读卷、加密档…） | 同样不提交，保存时告知原因 |
//! | 顶层 + 可编辑 | 提交 `EditBatch`；卷容器走就地写、zip/7z 走重写 + 原子替换（core 的路径选择） |
//!
//! 「保存时告知」而不是「打开时告知」是刻意的：只是想看看内容的用户不该被提示打扰
//! ——提示出现在**真的丢了改动**的那一刻。
//!
//! # 并发
//!
//! # 去重口径（`(mtime_ns, len)`）
//!
//! 保存事件与「已提交的那一次」比对 `(mtime_ns, len)`：相同即视为无变化（挡掉
//! 物化那一次的迟到事件、以及编辑器连续触发）。**已知边界**：粗粒度时间戳的
//! 文件系统（FAT/exFAT 常见 2 s）上，「同一时间戳窗口内、且长度不变」的两次保存
//! 只有后一次会被漏掉——真要根治得上内容哈希（每次保存读一遍文件），当前按
//! 「收益不抵成本」记录在此。
//!
//! # 并发
//!
//! 引擎**不按档案排队**任务，同一档案并发两个编辑任务会互相踩（重写/就地写都是
//! 读-改-写）。故每会话记 `inflight`：在飞期间再保存只置 `pending`，任务结束后由
//! [`AppData::reconcile_external_edits`]（主线程每 wake 调，≤100 ms）补一次。
//!
//! # 生命周期边界
//!
//! `watch` 句柄归 `AppShell`（UI 线程半场）：**窗口关掉 ⇒ 监听随之释放 ⇒ 之后的
//! 保存不再写回**（编辑器里那份临时文件还在，但没人看了）。会话记录随 `AppData`
//! 一起消失。同一物化路径重开同一文件 ⇒ 旧句柄先释放、新句柄接上（不会叠两个）。

use std::path::{Path, PathBuf};

use hzip_format::{Change, ChangeSet, EntryId};

use super::state::AppData;

/// 写回目标：**登记时定死**，不随界面导航变化（用户可以在编辑器里慢慢改，
/// 期间在 hzip 里翻别的档案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteBack {
    /// 可写回：保存后提交 `EditBatch + Replace{entry}`。
    Enabled {
        /// 该条目所属的档案（顶层会话的源档案路径）
        archive: PathBuf,
        /// **读侧投影**的条目 id（界面点的那一条）
        entry: EntryId,
    },
    /// 不可写回：保存时按 `reason`（i18n 键）告知用户，**绝不**提交任务。
    Disabled {
        /// i18n 键（`edit.writeback.*`）
        reason: &'static str,
    },
}

/// 一个外部编辑会话。
#[derive(Debug, Clone)]
pub struct ExternalEdit {
    /// 条目名（提示文案用）
    pub name: String,
    /// 物化路径（复制区稳定路径；watch 与 `Replace.src` 都用它）
    pub temp: PathBuf,
    /// 写回目标
    pub target: WriteBack,
    /// 最近一次**已提交**的 `(mtime_ns, len)`：相同 ⇒ 忽略事件（去抖后的重复触发、
    /// 以及**我们自己物化那一次写入**的迟到事件——登记时就以此为基准）。
    applied: Option<(u64, u64)>,
    /// 在飞编辑任务 id（同一档案不得并发）
    inflight: Option<u64>,
    /// 在飞期间又保存过 ⇒ 任务结束后补提交
    pending: bool,
    /// 「不可写回」已提示过（同一会话不重复弹）
    told: bool,
}

/// 监听线 → 主线程的事件。
#[derive(Debug, Clone)]
pub enum EditEvent {
    /// 登记会话 + 挂写回监听（主线程半场做：建 watch、存句柄）。
    Arm {
        /// 条目名
        name: String,
        /// 物化路径
        temp: PathBuf,
        /// 写回目标
        target: WriteBack,
    },
    /// 临时文件变过（去抖线程 → 主线程）。
    Saved {
        /// 物化路径
        temp: PathBuf,
        /// `(mtime_ns, len)`
        stat: (u64, u64),
    },
}

/// 保存事件的处理决定（**纯数据**；投递归 UI 侧，本模块零 gpui / 零 ui 依赖）。
#[derive(Debug, Clone)]
pub enum SaveAction {
    /// 提交这个规格（调用方走 `ui::shell::dispatch::submit_spec`），成功后回调
    /// [`AppData::note_external_edit_submitted`]。
    Submit {
        /// 已装配的 `EditBatch` 规格（source = 档案，changes = Replace）。
        /// 装箱：`JobSpec` 比另一个变体大一个数量级（clippy::large_enum_variant）。
        spec: Box<hzip_core::jobs::JobSpec>,
        /// 本次对应的文件状态（提交成功后原样回填）
        stat: (u64, u64),
        /// 会话的物化路径（回填定位用）
        temp: PathBuf,
    },
    /// 不可写回：调用方提示用户（`reason` 为 i18n 键，`name` 为条目名）。
    Refused {
        /// i18n 键
        reason: &'static str,
        /// 条目名
        name: String,
    },
}

/// 文件状态指纹 `(mtime_ns, len)`。
///
/// `mtime` 取纳秒；拿不到（文件刚被删/权限）→ 返回 `None`（调用方视为「无需动作」）。
#[must_use]
pub fn stat_of(path: &Path) -> Option<(u64, u64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    Some((mtime, md.len()))
}

impl AppData {
    /// 登记（或替换）一个外部编辑会话。
    ///
    /// `applied` 以**登记时的现状**为基准：物化刚刚写过这个文件，那条迟到的
    /// change 事件不该被当成「用户保存了」。
    pub fn arm_external_edit(&mut self, name: &str, temp: &Path, target: WriteBack) {
        self.external_edits.retain(|e| e.temp != temp);
        self.external_edits.push(ExternalEdit {
            name: name.to_string(),
            temp: temp.to_path_buf(),
            target,
            applied: stat_of(temp),
            inflight: None,
            pending: false,
            told: false,
        });
    }

    /// 会话查询（按物化路径；测试与提示用）。
    #[must_use]
    pub fn external_edit(&self, temp: &Path) -> Option<&ExternalEdit> {
        self.external_edits.iter().find(|e| e.temp == temp)
    }

    /// 全部会话（只读）。
    #[must_use]
    pub fn external_edits(&self) -> &[ExternalEdit] {
        &self.external_edits
    }

    /// 丢弃一个会话（监听句柄被释放、或其已失效时调用——**不再假装还在看着**）。
    pub fn forget_external_edit(&mut self, temp: &Path) {
        self.external_edits.retain(|e| e.temp != temp);
    }

    /// 该会话当前在飞的写回任务 id（诊断/测试用；`None` = 无在飞）。
    #[must_use]
    pub fn external_edit_job(&self, temp: &Path) -> Option<u64> {
        self.external_edit(temp).and_then(|e| e.inflight)
    }

    /// 保存事件 → 决定（`None` = 无需动作：无变化/在飞已记 pending/不可写回已提示过）。
    ///
    /// 调用方拿到 [`SaveAction::Submit`] 后**必须**走唯一投递出口，并在成功后回填
    /// [`Self::note_external_edit_submitted`]；投递失败则**不要**调用回填
    /// （`applied` 不变 ⇒ 下一次该文件再变时自然重试）。
    pub fn on_external_save(&mut self, temp: &Path, stat: (u64, u64)) -> Option<SaveAction> {
        let idx = self.external_edits.iter().position(|e| e.temp == temp)?;
        // 先把要用的量全取出来（结束对清单的借用），再装配规格（要读密码缓存）
        let (name, target, applied, inflight, temp_path) = {
            let e = &self.external_edits[idx];
            (
                e.name.clone(),
                e.target.clone(),
                e.applied,
                e.inflight,
                e.temp.clone(),
            )
        };
        match target {
            WriteBack::Disabled { reason } => {
                if self.external_edits[idx].told {
                    return None; // 同一会话只提示一次（反复保存不刷屏）
                }
                self.external_edits[idx].told = true;
                Some(SaveAction::Refused { reason, name })
            }
            WriteBack::Enabled { archive, entry } => {
                if applied == Some(stat) {
                    return None; // 内容没变（含我们自己物化那一次的迟到事件）
                }
                if inflight.is_some() {
                    self.external_edits[idx].pending = true; // 在飞：任务结束后补
                    return None;
                }
                let spec = edit_spec(&archive, entry, &temp_path, self);
                Some(SaveAction::Submit {
                    spec: Box::new(spec),
                    stat,
                    temp: temp_path,
                })
            }
        }
    }

    /// 提交成功回填（`applied` = 本次状态、`inflight` = 任务 id）。
    pub fn note_external_edit_submitted(&mut self, temp: &Path, stat: (u64, u64), job_id: u64) {
        if let Some(e) = self.external_edits.iter_mut().find(|e| e.temp == temp) {
            e.applied = Some(stat);
            e.inflight = Some(job_id);
        }
    }

    /// 每 wake 对账（主线程泵批处理调用，≤100 ms 一次；**无在飞会话时零成本**）。
    ///
    /// 在飞任务结束后：清位；若期间又保存过（`pending`）→ 主动比对一次并给出
    /// 动作（那次保存的 change 事件已经丢了，只能这里补）。
    pub fn reconcile_external_edits(&mut self) -> Vec<SaveAction> {
        let inflight: Vec<u64> = self
            .external_edits
            .iter()
            .filter_map(|e| e.inflight)
            .collect();
        if inflight.is_empty() {
            return Vec::new();
        }
        // 完成判定：行已终局（`finished` 有值）或行已消失（用户清了任务中心）
        let finished: Vec<u64> = {
            let Ok(jobs) = self.jobs.lock() else {
                return Vec::new();
            };
            inflight
                .into_iter()
                .filter(|id| jobs.row(*id).is_none_or(|r| r.finished.is_some()))
                .collect()
        };
        if finished.is_empty() {
            return Vec::new();
        }
        // 待补的（temp, stat）：先收集，再逐个走 on_external_save（借不到 &mut 两次）
        let mut todo: Vec<(PathBuf, (u64, u64))> = Vec::new();
        for e in &mut self.external_edits {
            let Some(id) = e.inflight else { continue };
            if !finished.contains(&id) {
                continue;
            }
            e.inflight = None;
            if !e.pending {
                continue;
            }
            e.pending = false;
            if let Some(stat) = stat_of(&e.temp) {
                todo.push((e.temp.clone(), stat));
            }
        }
        todo.into_iter()
            .filter_map(|(temp, stat)| self.on_external_save(&temp, stat))
            .collect()
    }
}

/// 装配 `EditBatch` 规格（source = 档案 + `Replace{entry, src = 物化文件}`）。
///
/// 密码取会话缓存（该档案此前经密码门验证过的密码）——与既有编辑入口同口径：
/// 未缓存则由服务层经密码门询问一次。
fn edit_spec(archive: &Path, entry: EntryId, temp: &Path, d: &AppData) -> hzip_core::jobs::JobSpec {
    let mut spec =
        hzip_core::jobs::JobSpec::new(hzip_core::jobs::JobKind::EditBatch, archive.to_path_buf());
    spec.changes = Some(ChangeSet {
        changes: vec![Change::Replace {
            entry,
            src: temp.to_path_buf(),
        }],
    });
    spec.password = d.cached_password(archive);
    spec
}

#[cfg(test)]
#[path = "tests/external_edit.rs"]
mod tests;
