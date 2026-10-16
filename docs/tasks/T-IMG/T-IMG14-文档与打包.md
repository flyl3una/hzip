# T-IMG14：文档、CI 与打包

| 项目 | 内容 |
|---|---|
| 阶段 | P5（收口；文档与实现同批落地，不留「以后补」） |
| 依赖 | T-IMG13 |
| 状态 | 未开工 |
| 目标 | 把 T-IMG 的结论沉淀为仓库的**长期事实源**，并把依赖/许可/体积/CI 一次交代清楚 |

## 交付物

| 动作 | 文件 | 职责 |
|---|---|---|
| 新 | `docs/design/13-磁盘镜像格式支持设计.md` | 正式设计文档：分层、契约（`hzip-vol`）、能力矩阵、写回三路径、安全策略、与上游 crate 的边界 |
| 改 | `docs/design/01-总体架构设计.md` | §1.1 非目标（删「VHD/VHDX 不做」的绝对口径，改「已实现 qcow2/vmdk/raw；vhd/vhdx 待评估」）、§2.2 扩展只读格式表、§10.4 可行性表、§D10/D11 决策行 |
| 改 | `docs/design/02-程序详细设计.md` | 新增 §7.7「卷层契约（`hzip-vol`）」；**§7.8「组合写与嵌套逐层冒泡」**（`services/codec_chain.rs`、core 侧组合编辑装配、`chain_editable`、`SPOOL_PREFIX` 约定——把 T-IMG0 的结论写进正式设计）；§8.8「`hzip-fmt-{raw,qcow2,vmdk}` 与 `hzip-fs-{ext4,ntfs,fat}`」；§9.9 编辑原子性补「就地编辑由格式自持」；错误码表补 `vol.*` 键 |
| 改 | `docs/design/03-测试方案及用例.md` | 新增 `TC-VOL-*` 用例族（探针/分区/读写/拒写/提交/取消），与既有 `TC-CONF-*` 并列 |
| 改 | `docs/design/04-编译与部署.md` | §许可：新增依赖清单（MIT/Apache，`deny.toml` 无需改动）+ **明确「不依赖任何外部二进制」**；§体积：前后对比；§构建：`volume-formats` feature 说明 |
| 改 | `docs/design/05-实施任务规划.md` | 登记 T-IMG1..14 |
| 改 | `README.md` | 支持格式表 + 一句话定位（「可写镜像内的文件系统」）+ 已知限制（streamOptimized 只读、NTFS 目录上限等） |
| 改 | `docs/todos/功能项目.md` | 把 `[ ] 增加对 qcow2, vmdk, raw(img) 格式的解压及预览支持` 收口为已实施条目（含关键决策与限制） |
| 改 | `build/package/metadata.toml` | 描述串（可选：功能清单里加「磁盘镜像」） |
| 改 | `.github/workflows/ci.yml` | 与 T-IMG13 合并收口（新 crate 测试 + deny + deps-direction + 裁剪构建） |

## 步骤 1：写文档（逐份，先骨架后细节）

### 1.1 `docs/design/13-磁盘镜像格式支持设计.md`

结构（体例照 11/12 号文档）：

```text
| 项目 | 内容 |  ← 文档版本/日期/状态/读者
1. 目标与非目标（含「不做：LUKS/md/LVM 写/qcow2 create-resize/镜像修复/BitLocker/WOF 写/exFAT 写」）
2. 分层与数据流（外包装 → 镜像 → 分区 → 文件系统；设备链三种末端）
3. 契约
   3.1 hzip-format 增项（probe / probe_head_bytes / supports_in_place_edit / Confidence::Medium / write_in_place / VolumeWriteMode / CreateOptions.volume_write）
   3.2 hzip-vol（VolumeHandler / Volume / FsDriver / FsVolume / OverlayDevice / Chain）
   3.3 条目投影（扁平全树；分区层 = 顶层目录；为什么不能做成嵌套会话层）
4. 上游依赖（版本、许可、能力边界、交叉验证方式、升级/替换预案）
5. 能力矩阵（镜像 × 文件系统 × 操作；含每个拒写理由的文案键）
6. 写回三路径与原子性口径（A/B/C；`TC-CONF-009` 的重新表述）
7. 安全与数据完整性（脏卷、日志重放、覆盖层、提交失败、临时区与配额）
8. 测试与验收（TC-VOL 族、夹具策略、交叉验证脚本、性能基线）
9. 未决与后续（VHD/VHDX、只读回退栈、exFAT、LVM/md 只读装配）
```

### 1.2 01 号文档的修订点（**逐条列出**，避免"看起来改了"）

| 位置 | 现在 | 改为 |
|---|---|---|
| §1.1 非目标 | 「**VHD / VHDX** 虚拟磁盘内文件提取（需完整 FAT/NTFS 文件系统解析器，成本远超收益，远期评估）」 | 「VHD/VHDX：待评估（qcow2/vmdk/raw 已于 2026-10 支持，见 13 号文档）；LUKS/md/LVM 写、镜像修复：不做」 |
| §2.2 只读格式 | 「iso/img 光盘镜像（P2）… vhd/vhdx 与 egg 不做」 | 「iso/img（P2）；**磁盘镜像 qcow2/vmdk/raw：读写，见 13 号文档**；vhd/vhdx 待评估；egg 不做」 |
| §10.4 表 | vhd/vhdx 一行「❌ 不做」 | 拆两行：qcow2/vmdk/raw「✅ 读写（2026-10 落地，纯 Rust 栈）」；vhd/vhdx「🟡 待评估（上游有 reader/writer，架构留位）」 |
| §D10/D11 | 「不做 vhd/vhdx」 | 「卷容器路径已就位；新增镜像格式 = 一个 `hzip-fmt-*` + 一个 `FsDriver`」 |

### 1.3 `02` 的 §9.9 补充（原子性口径，**必须写清**）

> 既有口径：编辑保存 = 「临时档案 → `rewrite` → 原子替换」（`TC-CONF-009` 用前后 sha256 相等做锚点）。
> 卷容器补充：当 handler 的 `supports_in_place_edit()` 为真且源是常规文件（且非组合链）时，core 走 `write_in_place`——
> **原子性由格式自持**：卷格式以覆盖层暂存变更，提交时才把脏块回放到原文件；提交前原文件
> 字节不变（等价于 `TC-CONF-009` 的"失败/取消后原档不变"），提交中断则如实报告部分提交状态。

## 步骤 2：CI 与打包

```yaml
# ci.yml 要点
- 新增作业：`vol-tests`（debug + release）、`vol-fixtures-selftest`（夹具自校验）
- 既有作业并入：`cargo deny check`、`python build/checks/deps-direction.py`
- 裁剪：`cargo build --no-default-features -p hzip-core`（`volume-formats` 关闭）
- 编译时间：记录前后（`cargo build --timings` 摘要写入提交信息）
```

打包：

```bash
python build/package/package.py --all      # 记录产物体积（便携 tar.gz/zip、MSI、dmg、deb/rpm）
# 体积对比写入 docs/design/04 §体积；许可随附清单已由 cargo-deny 白名单覆盖（MIT/Apache）
```

## 步骤 3：验收

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
python build/checks/deps-direction.py          # violations=0
cargo build --no-default-features -p hzip-core # 裁剪通过
bash script/vol-oracle.sh                      # 开发机（可选）
```

### 步骤 4：提交

```bash
git add docs README.md build/package/metadata.toml .github/workflows/ci.yml
git commit -m "docs(vol): 13 号设计文档 + 01/02/03/04/05 同步 + CI 与打包登记"
```

## 验收清单

- [ ] 13 号文档成文并与实现一致（能力矩阵与代码里的 `ReadOnlyReason` 逐条对应）
- [ ] 01/02/03/04/05 五份旧文档的修订点**逐条落地**（不是加一句"见 T-IMG"就算）
- [ ] README 支持格式表 + 已知限制
- [ ] `功能项目.md` 的 pending 项收口
- [ ] `cargo deny check` 通过且无需改 `deny.toml`（全部 MIT/Apache）
- [ ] 打包产物体积变化已记录；**产物内无外部二进制依赖**（明确写进 04）
- [ ] CI 全绿，含 feature 裁剪构建

## 风险 / 注意

| 点 | 处置 |
|---|---|
| 文档与实现漂移（本特性横跨 4 个 crate、7 个依赖） | 能力矩阵与 `ReadOnlyReason`/拒绝键**单源化**：文档表里的键必须与 `hzip-vol::caps` 一致，T-IMG10 的 i18n 键测试顺带覆盖 |
| 上游版本升级导致行为变化 | 04 号文档记「升级检查清单」：跑 `script/vol-oracle.sh` + `script/vol-bench.sh`，对比基线后再升级；版本保持 `=` 锁定直到清单通过 |
| 体积/编译时间上涨被"忘记" | 步骤 2 的体积与 timings 摘要必须进提交信息（可复盘） |
