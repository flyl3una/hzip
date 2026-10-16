//! 浏览服务（02 §9.10；T34）。
//!
//! 同步轻量——`browse()` 在调用线程执行，**不占线程池**（不产生 JobId/
//! 事件流）。零 IO 由契约红线 1 保证（`entries()` 不读数据区），耗时与
//! 数据区大小无关（TC-CONF-010）。
//!
//! [`Browser`] 结构（02 §9.10 原文）：`{ reader, entries, filter }` + enter
//! 递归的回溯链。`refresh()` 对同一 Source 重开 reader 重读；`set_filter`
//! glob 即时过滤（对缓存 entries 匹配，调 0 次 reader）；`tree()` O(n) 一次
//! 遍历构建目录树（BTreeMap 名称稳定序 + 隐式目录补建）；`enter(entry_id)`
//! 嵌套容器条目递归（`Source::Stream` → registry 组合装配，深度防护同
//! T20 `COMPOSITE_DEPTH_MAX`）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use hzip_error::HzipError;
use hzip_format::{
    ArchiveReader, CompositeTail, EntryId, EntryInfo, FormatRegistry, OpenOptions, PasswordRef,
    Sniffed, Source,
};
use smol_str::SmolStr;

use crate::services::preview::{PreviewData, PreviewMode};

/// 浏览会话（02 §9.10 原文结构 + 回溯链）。
pub struct Browser {
    /// 内层 reader（**延迟展开**的组合档为 `None`——见 [`Browser::deferred`]）。
    reader: Option<Box<dyn ArchiveReader>>,
    entries: Vec<EntryInfo>,
    /// 名称过滤（glob 段级匹配；None = 全量）
    filter: Option<glob::Pattern>,
    /// enter() 递归的回溯链（GUI 返回上级）
    path: Vec<EntryName>,
    /// 组合深度（enter 递归防护）
    depth: u32,
    /// **待展开**的组合档内层容器（用户口径 2026-09-24）。
    ///
    /// 打开 `.cpio.gz` 只做嗅探（毫秒级）：把「装配解码链 → 落临时 spool →
    /// 索引内层容器」整段推迟到用户在列表/树里双击那一层
    /// （[`Browser::enter_deferred`]）。此前打开即展开——实测 940 MB 的
    /// `.cpio.gz` 打开要 10+ 秒（无进度）、3.9 GB 的 initramfs 更要落同量临时盘。
    ///
    /// `Some` 期间 `reader` 恒为 `None`；**展开后不清**（本层留作「可再进的层」
    /// ——GUI 退回本层后同一行还要能再展开，见 [`Browser::enter_deferred`]）。
    deferred: Option<DeferredComposite>,
}

/// 组合档待展开的内层容器（`init.cpio.gz` → `init.cpio`）。
struct DeferredComposite {
    /// 源档案路径（装配解码链需要**可重放**源；`Source::File` 才延迟——
    /// 存路径而非 `Source`：`Source` 不可 `Clone`，而展开失败要能重试）。
    path: PathBuf,
    /// 内层容器的 handler id（cpio/tar/…；仅诊断用，真正装配仍走 registry）
    handler_id: String,
    /// 内层容器条目名（列表里那一行）
    inner_name: String,
    /// 内层容器**声明**大小（外层编码层元数据：gz 尾 ISIZE / xz 索引 / zst 帧头；
    /// 未知 = 0）——列表大小列与临时盘预检用。
    declared: u64,
}

/// EntryName 别名（承载于 hzip-error）。
type EntryName = hzip_error::EntryName;

/// 打开档案并浏览（02 §9.10 B→E：open_reader 组合解层 → entries 一次读取 →
/// 名字修复 → Browser）。头加密档案 `entries()` 报 `Password{Header}` 3102
/// 原样上抛（CLI/GUI 提示后带密码重试）。
pub fn browse(
    registry: &FormatRegistry,
    src: Source,
    opts: &OpenOptions,
) -> Result<Browser, HzipError> {
    // Source 传入 open_reader 即被 move——显示名先留副本（日志三次复用）
    let src_disp = src.display().to_string();
    // **组合档延迟展开**：打开只嗅探（毫秒级），内层容器的装配/落盘/索引推迟到
    // 用户双击那一层。见 [`Browser::deferred`] 与 `enter_deferred`。
    let src = if opts.defer_composite {
        match deferred_composite(registry, src)? {
            Deferred::Yes(b) => {
                tracing::info!(src = %src_disp, "组合档打开（内层容器延迟展开）");
                return Ok(b);
            }
            Deferred::No(src) => src,
        }
    } else {
        src
    };
    // 组合档开档预检（2026-09-24 用户实测）：内层容器要把**解压后的整层**落临时
    // 文件才能随机访问，装不下就该在解压**之前**失败（HZ-1002），而不是让用户等
    // 十来秒后拿到 ENOSPC。判据与放行条件见 `services::space`。
    crate::services::space::precheck_open_temp_space(registry, &src)?;
    let reader = match registry.open_reader(src, opts) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(src = %src_disp, err = %e, "档案打开失败");
            return Err(e);
        }
    };
    let entries = match reader.entries() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(src = %src_disp, err = %e, "档案条目读取失败（open 阶段）");
            return Err(e);
        }
    };
    tracing::info!(src = %src_disp, entries = entries.len(), "档案已打开");
    Ok(Browser {
        reader: Some(reader),
        entries,
        filter: None,
        path: Vec::new(),
        depth: 0,
        deferred: None,
    })
}

/// 组合档（编码层 + 容器尾）→ **延迟展开**的 Browser：列表只放一行内层容器条目。
///
/// 判据（任一不成立都走原路径）：
/// 1. 嗅探为 `Composite{ layers(非空), tail = Container }`（`Raw` 单流不是容器，
///    仍走原路径）；
/// 2. 源**可重放**（`File`/`Volumes`——装配解码链需要重新打开；`Stdin`/`Stream`
///    一次性流不可延迟）；
/// 3. 档案名能推出内层容器名（`hzip_format::exts::composite_inner_name`——
///    与「嵌套容器识别/右键菜单判定」同一张表）。
///
/// 那唯一一行是**合成条目**：`id=0`、`is_dir=false`、名字 = 内层容器名
/// （`init.cpio`）⇒ GUI 侧按既有 `is_container_name(name)` 判定成容器（图标/类型
/// 列/双击路由全部复用既有链路，无需新标记）。`size_raw` 取编码层**声明**大小
/// （gz 尾 ISIZE 等），未知为 0。
/// 延迟判定产物（`No` 把源还回去，调用方继续走原路径——`Source` 不可 Clone）。
enum Deferred {
    Yes(Browser),
    No(Source),
}

fn deferred_composite(registry: &FormatRegistry, src: Source) -> Result<Deferred, HzipError> {
    let Sniffed::Composite { layers, tail, .. } = registry.sniff(&src)? else {
        return Ok(Deferred::No(src));
    };
    if layers.is_empty() || !matches!(tail, CompositeTail::Container(_)) {
        return Ok(Deferred::No(src));
    }
    let (path, packed) = match &src {
        Source::File(p) => (p.clone(), std::fs::metadata(p).ok().map(|m| m.len())),
        // 分卷/一次性流不延迟：路径可重放性不成立（`Source` 不可 Clone，
        // 且分卷首卷不足以重开全链）→ 走原（即时展开）路径。
        Source::Volumes { .. } | Source::Stdin | Source::Stream(_) => {
            return Ok(Deferred::No(src));
        }
    };
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(inner_name) = hzip_format::exts::composite_inner_name(&file_name) else {
        return Ok(Deferred::No(src));
    };
    let handler_id = hzip_format::exts::container_handler(&inner_name)
        .unwrap_or_default()
        .to_string();
    // 声明大小：最内层编码层的输出 = 内层容器解压后字节数（与单流 size_raw 同源）。
    //
    // 精确值要靠 `hzip-fmt-single` 解出压缩流头（gz/bz2/xz/zst）。裁掉
    // `default-formats` 的构建（04 §3.2 发行版合规：只留 tar/zip/7z 等）里那个
    // crate 根本没链进来 —— 此时退化为 0（本函数既有口径里 `packed` 缺失即 0，
    // 显示层按「未知大小」处理），**不能**为了这行把 fmt crate 拉进依赖红线。
    #[cfg(feature = "default-formats")]
    let declared = layers
        .last()
        .and_then(|codec| {
            packed.and_then(|n| hzip_fmt_single::declared_payload_size(codec.as_str(), &path, n))
        })
        .unwrap_or(0);
    #[cfg(not(feature = "default-formats"))]
    let declared = {
        let _ = (&layers, &path, packed);
        0
    };
    let entry = EntryInfo {
        id: 0,
        name: hzip_error::EntryName::new(inner_name.clone()),
        is_dir: false,
        is_symlink: false,
        symlink_target: None,
        size_raw: declared,
        size_packed: packed.unwrap_or(0),
        mtime: None,
        mode: None,
        attributes: Default::default(),
        encrypted: false,
        header_encrypted: false,
        crc32: None,
        codec: layers.last().cloned(),
        name_encoding: hzip_format::NameEncoding::Utf8,
        encryption: None,
        compression_level: None,
    };
    Ok(Deferred::Yes(Browser {
        reader: None,
        entries: vec![entry],
        filter: None,
        path: Vec::new(),
        depth: 0,
        deferred: Some(DeferredComposite {
            path,
            handler_id,
            inner_name,
            declared,
        }),
    }))
}

impl Browser {
    /// 条目视图（filter 生效后；返回缓存切片的过滤拷贝——无 IO）。
    ///
    /// glob 段级匹配（与 T33 walk 过滤链同语义）：整名匹配 **或** 任一
    /// 路径段匹配即命中（`*.txt` 匹配 `a.txt` 与 `sub/c.txt` 的段——
    /// 但 T34 过滤的「命中文件祖先保留」语义见 [`tree`]）。
    pub fn entries(&self) -> Vec<&EntryInfo> {
        match &self.filter {
            None => self.entries.iter().collect(),
            Some(p) => self
                .entries
                .iter()
                .filter(|e| {
                    let rel = e.name.as_str();
                    p.matches(rel)
                        || (rel.contains('/') && rel.split('/').any(|seg| p.matches(seg)))
                })
                .collect(),
        }
    }

    /// 原始条目数（不过滤）。
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// 名称过滤：glob 段级匹配（即时——对缓存 entries 匹配，调 0 次 reader）。
    /// 非法 pattern → `Err`。
    pub fn set_filter(&mut self, pattern: Option<&str>) -> Result<(), HzipError> {
        self.filter = match pattern {
            None => None,
            Some(p) => Some(glob::Pattern::new(p).map_err(|e| HzipError::Unsupported {
                what: format!("hz-5003 invalid-glob {p}: {e}").into(),
            })?),
        };
        Ok(())
    }

    /// 当前过滤 pattern（诊断）。
    pub fn filter(&self) -> Option<&str> {
        self.filter.as_ref().map(|p| p.as_str())
    }

    /// 取单条目（id 越界 → None）。
    pub fn entry(&self, id: EntryId) -> Option<&EntryInfo> {
        self.entries.get(id)
    }

    /// 重读（外部修改档案后的刷新语义）：以给定 source 重开 reader +
    /// entries。嵌套浏览器（enter 产物，流式源）不可重开——返回
    /// `Err Unsupported`（嵌套刷新由上层重新 enter）。
    ///
    /// 签名较任务卡增 source 参数（Browser 不持原始 Source——
    /// `Source::Stream` 不可 clone，T20 契约）；rustdoc 注明 T34 定签名。
    pub fn refresh(&mut self, registry: &FormatRegistry, src: Source) -> Result<(), HzipError> {
        if self.depth > 0 {
            return Err(HzipError::Unsupported {
                what: "hz-5003 refresh-nested-browser".into(),
            });
        }
        let reader = registry.open_reader(src, &OpenOptions::default())?;
        let entries = reader.entries()?;
        self.reader = Some(reader);
        self.entries = entries;
        Ok(())
    }

    /// 目录树（§3.2 算法；filter 生效于叶——命中文件的祖先目录保留可见）。
    pub fn tree(&self) -> TreeNode {
        let visible: Vec<&EntryInfo> = self.entries();
        TreeNode::build(&visible)
    }

    /// 嵌套容器条目进入（§3.4：read_entry 流 → Source::Stream 递归装配）。
    /// 深度 = self.depth + 1 > `COMPOSITE_DEPTH_MAX` → Err Quota{MaxEntries} 4504。
    ///
    /// `pw`：会话密码（外层加密档案读内层容器条目用；`PasswordRef::none()`
    /// = 未加密/未缓存）。
    pub fn enter(
        &mut self,
        entry_id: EntryId,
        registry: &FormatRegistry,
        pw: PasswordRef<'_>,
    ) -> Result<Browser, HzipError> {
        let info = self.entry(entry_id).ok_or_else(|| HzipError::Unsupported {
            what: format!("hz-5003 no-entry-{entry_id}").into(),
        })?;
        if info.is_dir {
            return Err(HzipError::Unsupported {
                what: format!("hz-5003 enter-dir-{}", info.name).into(),
            });
        }
        if self.depth + 1 > hzip_format::consts::COMPOSITE_DEPTH_MAX {
            return Err(HzipError::Quota {
                quota: hzip_error::QuotaKind::MaxEntries,
            });
        }
        let stream = self.reader().read_entry(entry_id, pw)?;
        // registry.open_reader 对 Stream 源不支持嗅探（一次性流不可重放，
        // T20 实现约束）——改按条目扩展名选 handler 直开 Stream（T34 rustdoc
        // 注明：enter 的格式判定走扩展名回退，非魔数嗅探）。
        //
        // 组合条目（`.tar.gz`/`.tgz`/`.tbz2`/`.txz`/`.tzst`）需**先解压缩层**：
        // 容器 handler（tar）拿到的是原始 tar 字节，而条目流是压缩后的。
        // 编解码器的 feature 门控知识复用 `hzip_format::sniffer::wrap_layer_decoder`
        // （与组合嗅探同一份构造），避免在 core 里复制一份。
        let (handler_id, codec) = hzip_format::exts::container_and_codec(info.name.as_str())
            .ok_or_else(|| HzipError::Unsupported {
                what: format!("hz-5001 enter-unsupported-container: {}", info.name).into(),
            })?;
        let handler = registry
            .handler(handler_id)
            .ok_or_else(|| HzipError::Unsupported {
                what: format!("hz-5001 handler-{handler_id}-not-registered").into(),
            })?;
        let input: Box<dyn std::io::Read + Send> = match codec {
            Some(id) => hzip_format::sniffer::wrap_layer_decoder(id, stream)
                .map_err(|source| HzipError::Io { source, path: None })?,
            None => stream,
        };
        let reader = handler.open_reader(Source::Stream(input), &OpenOptions::default())?;
        let entries = reader.entries()?;
        let mut path = self.path.clone();
        path.push(info.name.clone());
        Ok(Browser {
            reader: Some(reader),
            entries,
            filter: None,
            path,
            depth: self.depth + 1,
            deferred: None,
        })
    }

    /// 内层 reader（延迟展开的组合档尚未展开 → `Unsupported`）。
    ///
    /// 唯一的取用点：任何真正读数据的操作都经它——延迟态下这些操作本就不该被
    /// 调用（列表/树只用 `entries` 缓存），真被调到时给出明确原因而非 panic。
    fn reader(&self) -> &dyn ArchiveReader {
        match &self.reader {
            Some(r) => r.as_ref(),
            // 调用方（preview/read_entry）返回 Result，这里以 panic 不可接受——
            // 故延迟态由上层先 `enter_deferred`；本 fn 仅服务已展开路径。
            None => unreachable!("延迟展开的组合档不得直接读条目（先 enter_deferred）"),
        }
    }

    /// **待展开**的内层容器名（非延迟档 → `None`）。
    ///
    /// GUI 侧据此把「双击那一行」路由到 [`Self::enter_deferred`]（而非按条目
    /// 物化的嵌套容器路径）。
    pub fn deferred_layer(&self) -> Option<&str> {
        self.deferred.as_ref().map(|d| d.inner_name.as_str())
    }

    /// 内层容器**声明**大小（延迟档；未知 0）——列表大小列/临时盘预检数据面。
    pub fn deferred_declared_size(&self) -> u64 {
        self.deferred.as_ref().map(|d| d.declared).unwrap_or(0)
    }

    /// **展开**延迟的组合档内层容器（用户口径 2026-09-24：双击 `init.cpio` 才加载）。
    ///
    /// 这里是唯一耗时/占临时盘的一步：装配解码链（gz 解压）→ 内层容器 handler
    /// 落 spool → 索引。故先跑临时盘预检（HZ-1002 快速失败），再由 registry 以
    /// **原源**走完整组合路径（与「打开即展开」时代的产物逐字相同）。
    ///
    /// **只读借用（`&self`）：本层延迟态**不**被消费**（用户实测 2026-10-14）。
    /// GUI 把本层会话整体压栈后要能原样退回**并再进**（退回 `.tgz` 层再双击那
    /// 一行）。旧实现 `self.deferred.take()` 一被调用，外层帧就永久成「无浏览
    /// 态」：`deferred_layer()` 变 `None`，同一行走**按条目物化**的兜底路径，
    /// 而那张合成行的 `id = 0` 对应的是重开 reader 的**内层容器条目空间**——
    /// `read_entry(0)` 解出来的是内层容器的首条目，于是父层直接显示子压缩包的
    /// 内容（`bh_auto_ops.itr2.x86_64.tgz` 双击 `….tar` 那行直接进 `ops/`）。
    ///
    /// 失败可重试：展开是「按需加载」，一次失败（磁盘满、档案被删）不该把会话
    /// 变成打不开的死局。本方法纯读——失败时本层状态逐字不变，再点一次即可重试
    /// （存路径而非 `Source` 正是为此：源可重放）。
    ///
    /// 非延迟档 → `Err Unsupported`（GUI 的嵌套容器路径不该走到这）。
    pub fn enter_deferred(&self, registry: &FormatRegistry) -> Result<Browser, HzipError> {
        let Some(d) = self.deferred.as_ref() else {
            return Err(HzipError::Unsupported {
                what: "hz-5003 not-deferred-composite".into(),
            });
        };
        let src = Source::File(d.path.clone());
        let reader = crate::services::space::precheck_open_temp_space(registry, &src)
            .and_then(|()| registry.open_reader(src, &OpenOptions::default()))?;
        let entries = reader.entries()?;
        tracing::info!(
            layer = %d.inner_name,
            handler = %d.handler_id,
            declared = d.declared,
            entries = entries.len(),
            "组合档内层容器已展开"
        );
        let mut path = self.path.clone();
        path.push(hzip_error::EntryName::new(d.inner_name.clone()));
        Ok(Browser {
            reader: Some(reader),
            entries,
            filter: None,
            path,
            depth: self.depth + 1,
            deferred: None,
        })
    }

    /// 回溯链（GUI 返回上级用）。
    pub fn path(&self) -> &[EntryName] {
        &self.path
    }

    /// 当前组合深度。
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// 预览委托（§4.4 三模式）。
    ///
    /// `pw`：会话密码（加密条目预览用；`PasswordRef::none()` = 未加密/未缓存）。
    pub fn preview(
        &self,
        entry_id: EntryId,
        mode: PreviewMode,
        max_bytes: Option<u64>,
        pw: PasswordRef<'_>,
    ) -> Result<PreviewData, HzipError> {
        crate::services::preview::preview(self.reader(), entry_id, mode, max_bytes, pw)
    }

    /// 读单条目数据流（T-MENU8 复制物化用；与 `preview` 同源 reader）。
    ///
    /// 独立/乱序/重复可用（红线 2 由 fmt 实现保障）——调用方每个条目调一次，
    /// 把流灌进 `TempArea::materialize`。
    ///
    /// `pw`：会话密码（加密条目用；`PasswordRef::none()` = 未加密/未缓存）。
    pub fn read_entry(
        &self,
        entry_id: EntryId,
        pw: PasswordRef<'_>,
    ) -> Result<Box<dyn std::io::Read + Send>, HzipError> {
        self.reader().read_entry(entry_id, pw)
    }

    /// **卷内顶层行（分区）的元数据**（磁盘镜像会话才有；其它格式为空）。
    ///
    /// 界面据此把分区行画成「分区」（专属图标）、类型列显示文件系统、无标签时按语言
    /// 显示「分区 N」——**不靠名字猜**。名字本身保持 ASCII 稳定（树路径/剪贴板/日志）。
    #[must_use]
    pub fn volume_rows(&self) -> Vec<hzip_format::VolumeRowMeta> {
        self.reader().volume_rows()
    }
}

/// 密码预检（设计稿 11 §3.5 分级；用户口径 2026-09-28 会话「解密中」态）。
///
/// 用途：GUI 密码门提交后、放行任何读操作之前判定密码正确性——**不产出任何
/// 明文字节**即得结论（红线 3「错密码必须返回 Password，禁止静默乱码流」）。
///
/// 分级（与 `extract::resolve_early_password` 同一判据）：
/// 1. **头加密档案**：`open_reader` 带密码解头，成败即结论（精确）；
/// 2. **数据加密（头明文）**：取首个「加密且 `size_packed ≤ PREFLIGHT_MAX_BYTES`」
///    条目真读一次——`read_entry` 在**流交付前**完成密码验证（zip KV/校验头、
///    7z 解码、rar 密钥），错密码立即 `Password` 错；
/// 3. 无有界条目（7z 固实超限 / 无廉价通道）→ `Ok(())` **不验**（降级：放行后
///    由读操作的 `Password` 错兜底重问）。
///
/// 返回 `Err(HzipError::Password { .. })` = 密码错误（唯一失败语义）；
/// 其余 IO/格式错误原样上抛（不是「密码错」）。
pub fn preflight_password(
    registry: &FormatRegistry,
    path: &std::path::Path,
    pw: &zeroize::Zeroizing<String>,
) -> Result<(), HzipError> {
    let opts = OpenOptions {
        password: Some(pw.clone()),
        ..Default::default()
    };
    let reader = registry.open_reader(Source::File(path.to_path_buf()), &opts)?;
    let entries = reader.entries()?;
    let bounded = entries
        .iter()
        .find(|e| e.encrypted && e.size_packed <= PREFLIGHT_MAX_BYTES);
    if let Some(info) = bounded {
        // 流交付 = 密码已验（读侧预验在交付前完成）；立即丢弃。
        let _ = reader.read_entry(info.id, PasswordRef::new(pw))?;
    }
    Ok(())
}

/// 有界预检上限（与 `extract::PREFLIGHT_MAX_BYTES` 同值；超限跳过预检=降级）。
pub const PREFLIGHT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// 目录树节点（02 §9.10；名称稳定序 + 隐式目录）。
#[derive(Debug)]
pub struct TreeNode {
    /// 节点名（root 为空）
    pub name: SmolStr,
    /// 目录条目 id（隐式目录为 None）
    pub entry: Option<EntryId>,
    /// 子节点（BTreeMap 名称排序，跨平台一致）
    pub children: BTreeMap<SmolStr, TreeNode>,
}

impl TreeNode {
    /// 平面条目 → 树，O(n·depth) 一次遍历（§3.2 算法：逐条目沿 `/` 段下行，
    /// BTreeMap::entry 原地取或建隐式目录；文件/目录同名冲突以目录为准）。
    pub fn build(entries: &[&EntryInfo]) -> TreeNode {
        let mut root = TreeNode {
            name: SmolStr::new_inline(""),
            entry: None,
            children: BTreeMap::new(),
        };
        for e in entries {
            let segs: Vec<&str> = e
                .name
                .as_str()
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            if segs.is_empty() {
                continue;
            }
            let mut cur = &mut root;
            for (i, seg) in segs.iter().enumerate() {
                let is_last = i == segs.len() - 1;
                let seg = SmolStr::new(*seg);
                if is_last {
                    let child = cur.children.entry(seg.clone()).or_insert_with(|| TreeNode {
                        name: seg.clone(),
                        entry: None,
                        children: BTreeMap::new(),
                    });
                    // 文件/目录冲突：目录条目优先；文件叶覆盖仅当无 entry
                    if child.entry.is_none() || e.is_dir {
                        child.entry = Some(e.id);
                    }
                } else {
                    let child = cur.children.entry(seg.clone()).or_insert_with(|| TreeNode {
                        name: seg,
                        entry: None,
                        children: BTreeMap::new(),
                    });
                    // 中间段若对应目录条目 → 记 entry
                    if e.is_dir {
                        // 仅当该段确实是目录条目本身（条目名恰以该段结尾）
                    }
                    cur = child;
                }
            }
        }
        root
    }

    /// 节点总数（含根；测试与诊断用）。
    pub fn node_count(&self) -> usize {
        1 + self
            .children
            .values()
            .map(|c| c.node_count())
            .sum::<usize>()
    }
}
