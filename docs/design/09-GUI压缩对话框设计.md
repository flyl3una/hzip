# hzip 压缩对话框设计（gpui-kit）

| 项目 | 内容 |
|---|---|
| 文档版本 | v0.1（供评审：先定弹窗形态与参数集，通过后再落实施任务） |
| 状态 | **设计稿**——未改动任何代码 |
| 上游文档 | [02-程序详细设计.md](02-程序详细设计.md) §6.3/§7.2/§9.5、[06-GUI-gpui-kit界面设计.md](06-GUI-gpui-kit界面设计.md) §4.4（现行压缩对话框原型）、[07-GUI代码组织与分层约定.md](07-GUI代码组织与分层约定.md) |
| 参照产品 | Bandizip「压缩」对话框、7-Zip「添加到压缩包」对话框 |
| 视觉稿 | [`assets/09-compress-dialog-mockup.png`](assets/09-compress-dialog-mockup.png)（源文件 [`assets/09-compress-dialog-mockup.html`](assets/09-compress-dialog-mockup.html)） |
| 技术底座 | gpui-kit `=0.7.1`（内部 gpui-component 0.7.1 / gpui-base 0.7.1 / gpui-pre 0.3.8） |
| 审阅范围 | §2 弹窗结构、§3 字段规格、§5 压缩方式矩阵、§6 比例/速度、§10 数据面缺口、§13 待决策项 |

---

## 1. 目标与参照分析

### 1.1 用户诉求 → 参数映射

诉求「文件名、格式、压缩方式、压缩比例/速度」四项，逐一落到具体控件：

| 诉求 | 设计落点 | 与两参照产品的关系 |
|---|---|---|
| **文件名** | 「文件名」独立字段（只填名字）+ 与格式联动的扩展名后缀 + 「保存到」目录行 | Bandizip / 7-Zip 均为「压缩包名（含扩展名）」单字段；本设计拆名/目录，使**换格式时扩展名自动跟随**，消掉「选了 7z 但输出仍叫 .zip」这类陈旧值 |
| **格式** | `Select`：zip / 7z / tar / gz / bz2 / xz / zst（沿用 `labels::FORMAT`） | 7-Zip 只列可写格式；Bandizip 列表更长（zipx/exe/iso 等）——hzip 只列已交付格式 |
| **压缩方式** | `Select`，候选由「格式 × 已实现 codec」派生（§5） | 7-Zip 的 `Compression method`（LZMA2/LZMA/PPMd/BZip2/Deflate/Copy）同构；Bandizip 收在「更多选项」内 |
| **压缩比例/速度** | **6 档离散滑块**（存储/最快/快速/标准/最大/极限）+ 实时实参提示行（§6） | 即两家的「压缩等级/压缩级别」；7-Zip 用下拉、Bandizip 用下拉 + 视觉，本设计改用滑块把「速度 ↔ 比例」的权衡直接画出来 |

### 1.2 两参照产品的字段集对照

| 7-Zip「添加到压缩包」 | Bandizip「压缩」 | hzip 本设计 | 说明 |
|---|---|---|---|
| Archive（名称+路径） | 压缩包名称 | 文件名 + 保存到 | ✅ 四项核心之一 |
| Archive format | 压缩格式 | 格式 | ✅ |
| Compression level | 压缩级别 | 压缩级别（滑块） | ✅ |
| Compression method | 压缩方法 | 压缩方式 | ✅（需扩数据面，见 §10） |
| Dictionary size | — | 字典大小 | 仅 7z/xz |
| Word size | — | ✗ 不设 | 7-Zip 专有；hzip codec 无该调参入口 |
| Solid Block size | — | 固实块 | 仅 7z（`caps.solid`） |
| Number of CPU threads | 多线程 | 线程数 | `CompressSpec.threads` |
| Split to volumes | 压缩分卷 | 分卷大小 | `CompressSpec.split` |
| Update mode | 更新方式 | 更新方式 | 接 `CreateOptions.append`（仅 zip/tar） |
| Parameters（PPMd） | — | ✗ 不设 | 无 PPMd 实现 |
| Encryption（密码/方法/加密文件名） | 加密 | 加密区 | 见 §10 现状门控 |
| Compress shared files / Delete after / SFX | 压缩后动作 | ✗ 本期不设 | 见 §14 |
| — | — | 排除规则 / 保留属性 / 完成后校验 / 单条失败继续 | hzip 既有 `CompressSpec` 字段，予以暴露 |

### 1.3 取舍原则

1. **打开即决策**：核心四参数（文件名/格式/压缩方式/压缩级别）常显，其余全部收进两个折叠区——避免 20+ 控件铺满一屏。
2. **能力驱动**：每个控件的可用性由 `FormatCaps` 决定，不支持即禁用 + 一行灰字说明原因，不给「点了才报错」的路径（§4）。
3. **零新概念**：新增控件全部对应 `CompressSpec` / `CreateOptions` 已有字段，唯一例外是「压缩方式」（§10 缺口 ①）。
4. **不静默覆盖**：目标已存在时沿用 `unique_out_path` 追加序号，并在弹窗内**先行告知**（§7）。

---

## 2. 弹窗结构

### 2.1 线框（默认态）

```text
        ┌─ 压缩 ─────────────────────────────────────────────────── ✕ ─┐
        │  ▣ F:\data\project                                             │
        │    3 个文件 · 2 个目录 · 48.2 MB                               │
        │  ── 基本 ─────────────────────────────────────────────────────  │
        │  文件名   [ project                    ] .zip    [ 浏览… ]      │
        │  保存到   [ F:\data\project                                 ]   │
        │  格式     [ zip              ▾ ]                                │
        │  压缩方式 [ Deflate          ▾ ]                                │
        │  压缩级别 更快 ●━━━━━●━━━━━●━━━━━●━━━━━●━━━━━● 更慢            │
        │           存储   最快   快速   标准   最大   极限                │
        │           Deflate 级别 6 · 速度 中 · 预计压缩率 ~68%            │
        │  ▸ 高级选项                    分卷 不分卷 · 线程 自动 · 固实 关  │
        │  ▸ 加密                        未加密                            │
        │  ┌ 摘要 ─────────────────────────────────────────────────────┐  │
        │  │ zip · Deflate · 标准 · 单文件 · 无加密 · 无排除规则        │  │
        │  └───────────────────────────────────────────────────────────┘  │
        │                                          [ 取消 ]  [ 开始压缩 ] │
        └─────────────────────────────────────────────────────────────────┘
```

- 宽度 **560 px**（窗口默认 560×660，可缩放；滑块刻度需要横向空间）。
- **2026-09 定稿（用户报障后返工）**：界面**不再是 `Dialog` 弹层**，而是**参数窗的
  内容视图**（窗口级三段式）——头部/底部固定，正文 `flex_1 + min_h_0 +
  overflow_y_scrollbar` 滚动，窗口多小都能滚、底部按钮永远可见。旧稿的「弹层
  max-height ≈ 60 vh」方案作废：`Dialog` 弹层高度由内容驱动（无 `max_h`），展开态
  会长出窗口底部（正文滚不动、按钮点不到）。承载实现见
  `ui/dialogs::panel_shell`，回归锚点见 `op_window` 的两个 gpui 测试。
- 视觉稿见 [`assets/09-compress-dialog-mockup.png`](assets/09-compress-dialog-mockup.png)（A 默认态浅色 / B 展开态 / C 深色 / D 视觉层次对照）。

### 2.2 分区

| 区 | 内容 | 默认 | 实现载体 |
|---|---|---|---|
| 头部 | 标题「压缩」+ 对象摘要/统计（固定行） | — | `panel_shell` 头部（`v_flex` + `flex_shrink_0`） |
| 源摘要行 | 对象路径 + 「N 个文件 · M 个目录 · 体积」 | 常显 | 面板头部副标题（不随正文滚走） |
| 基本 | 文件名 / 保存到 / 格式 / 压缩方式 / 压缩级别 | 常显 | `Form::horizontal` + `FormField` |
| 高级选项 | 分卷 / 字典 / 固实 / 线程 / 更新方式 / 排除 / 五个开关 | 收起 | `Accordion` |
| 加密 | 密码 / 确认密码 / 加密方式 / 加密文件名 | 收起 | `Accordion` |
| 摘要 | 参数实时预览单行 | 常显 | inset 卡片（`DescriptionList` 备选） |
| 底部 | 提示/错误行 + 取消 + 开始压缩 | — | `panel_shell` 底部（`flex_shrink_0`）+ `Button` |

折叠区标题右侧显示**就地摘要**（如「分卷 100 MB · 线程 自动 · 固实 关」），折叠状态下也能读到关键取值——不必展开确认。

---

## 3. 字段规格

### 3.1 基本区

| # | 字段 | 控件 | 绑定 | 默认 | 校验 / 行为 |
|---|---|---|---|---|---|
| 1 | 压缩对象 | 只读卡片 | props 摘要 | — | 多选显示「首项 等 N 项」；同时给文件数/目录数/总字节 |
| 2 | **文件名** | `Input` + 扩展名后缀 chip + `浏览…` | `CompressDialogVm.out`（拆名/目录两段） | 由 `default_out_path` 派生 | 空 → 禁用提交；含 `\ / : * ? " < > \|` → 行内 danger；后缀 chip **只读**，随格式自动改写 |
| 3 | 保存到 | `Input` | 同上（目录段） | 源所在目录 | `浏览…` 走 `rfd` 原生保存框，一次同时回填目录与文件名 |
| 4 | **格式** | `Select`（7 项） | `CompressDialogVm.format` | `zip`（`DEFAULT_FORMAT`） | 变更触发三处联动：后缀 chip / 压缩方式候选 / 高级区与加密区能力（§7.1） |
| 5 | **压缩方式** | `Select`（候选随格式） | 新增 `method`（§10 缺口 ①） | 按格式 | 单候选格式 → 禁用 + 灰字「该格式固定使用 BZip2」 |
| 6 | **压缩级别** | `Slider`（6 档离散）+ 刻度名 + 实参提示行 | `CompressDialogVm.level` | `normal` | 见 §6；`←/→` 步进一档，`Home/End` 到两端 |

### 3.2 高级选项区

| # | 字段 | 控件 | 绑定字段 | 默认 | caps 门控 |
|---|---|---|---|---|---|
| 7 | 分卷大小 | `Select`（不分卷 / 10 / 100 / 700 MB / 1 / 4 GB / 自定义…）+ `NumberInput` + 单位 `Select` | `CompressSpec.split` | 不分卷 | `caps.split`（zip ✓ / 7z ✓ / 其余 ✗） |
| 8 | 字典大小 | `Select`（自动（随级别）/ 64 KB / 1 / 4 / 16 / 64 / 256 MB / 1 GB） | 新增（`Lzma2Params.dict` 覆写） | 自动（随级别） | 仅 7z / xz |
| 9 | 固实块 | `Select`（非固实 / 4 / 16 / 64 / 256 MB / 1 / 4 GB） | `CompressSpec.solid` | 非固实 | `caps.solid`（仅 7z，feature `solid`） |
| 10 | 线程数 | `Select`（自动 / 1 / 2 / 4 / 8 / 16 / …） | `CompressSpec.threads` | 自动（0） | 上限取 `available_parallelism`；**7z 实际单线程**（§12 问题 ⑤） |
| 11 | 更新方式 | `Select`（新建档案 / 追加到现有档案） | `CreateOptions.append` | 新建档案 | `caps.append`（zip ✓ / tar ✓ / 7z ✗） |
| 12 | 排除规则 | `Input`（逗号分隔 glob） | `CompressSpec.exclude_globs` | 空 | `*.tmp` 按段匹配、`sub/*` 剪枝（02 §9.5 B） |
| 13 | 排除隐藏文件 | `Checkbox` | `exclude_hidden` | 关 | — |
| 14 | 跟随符号链接 | `Checkbox` | `follow_symlinks` | 关（链接记 target） | — |
| 15 | 保留文件属性 | `Checkbox` | `keep_metadata` | **开** | 时间/权限/属性 |
| 16 | 完成后校验档案 | `Checkbox` | `verify_after` | **开** | 大档案可关以省一次全读 |
| 17 | 单条失败继续打包 | `Checkbox` | `best_effort` | 关（fail-fast） | 失败明细进任务中心 |
| 18 | 档案注释 | `Input` | `comment` | 空 | `caps.comment`——**当前无格式声明支持**，禁用 + 灰字 |

### 3.3 加密区

| # | 字段 | 控件 | 绑定 | 状态 |
|---|---|---|---|---|
| 19 | 密码 | `Input`（密文）+「显示」开关 | `CompressDialogVm.password`（`Zeroizing<String>`） | 见下 |
| 20 | 确认密码 | `Input`（密文） | — | 不一致 → danger 行内提示 |
| 21 | 加密方式 | `Select` | `EncryptionRequest` | 候选由 `caps.encryption` 派生：zip → ZipCrypto；7z → AES-256（头加密）；其余 → 无 |
| 22 | 加密文件名 | `Checkbox` | — | 仅 7z 头加密（`EncryptionCaps::AesHeader`）；勾选后列出条目需密码 |

> **现状门控（必须保留提示）**：各 fmt `create_writer` 对 `CreateOptions.encryption` 尚未实现（zip 写加密归 T45、7z 头加密写归 T46）。因此本区当前为**可编辑但提交时告知**的形态，底部灰字沿用既有 i18n key `dlg.compress.write_encryption_disabled`（「写侧加密未启用」），并在摘要行标「无加密」。是否改为整区禁用（只读）是 §13 待决策项 ③。

### 3.4 底部

| 元素 | 内容 |
|---|---|
| 提示行 | 目标已存在 → warn 色「目标已存在：F:\data\project.zip → 将保存为 F:\data\project_1.zip」 |
| 摘要行 | 参数实时预览：`格式 · 方式 · 级别 · 分卷 · 字典 · 固实 · 线程 · 排除数 · 加密` |
| 错误行 | danger 色（复用现行 `error_line`） |
| 按钮 | `[取消]`（secondary）+ `[开始压缩]`（primary）；提交后 primary → 禁用 + `Spinner` |

---

## 4. 能力矩阵（格式 × 参数）

依据 `crates/hzip-format/src/caps.rs` 的 `FormatCaps` 与各 handler 实装值；7z 诸项由 feature 门控且 **default features = `["aes","solid","split","bcj2"]` 全开**。

| 参数 | zip | 7z | tar | gz / bz2 / xz / zst |
|---|---|---|---|---|
| 压缩方式可选 | Deflate | LZMA2 | —（不压缩） | 单一固定 |
| 分卷 `caps.split` | ✓ | ✓ | ✗ | ✗ |
| 固实 `caps.solid` | ✗ | ✓ | ✗ | ✗ |
| 加密 `caps.encryption` | `ZipCrypto` | `AesHeader` | `None` | `None` |
| 加密文件名 | ✗ | ✓ | ✗ | ✗ |
| 追加 `caps.append` | ✓ | ✗ | ✓ | ✗ |
| 注释 `caps.comment` | ✗ | ✗ | ✗ | ✗ |

**统一交互**：任一控件不可用时**不隐藏**（隐藏会让参数集随格式跳动），而是禁用 + 保持占位，并在行下给一行灰字原因（如「分卷：该格式不支持」「线程数：7z 的 LZMA2 当前为单线程编码」）。折叠区标题右摘要同步显示「不支持 / 该格式不支持加密」，不展开即可见。

---

## 5. 压缩方式矩阵（需扩数据面）

### 5.1 现状

`CreateOptions`（`crates/hzip-format/src/handler.rs:90`）与 `CompressSpec` 均**无 method 字段**；格式到编码器的映射是硬编码的：

| 格式 | 实装编码器 | 证据 |
|---|---|---|
| zip | Deflate（`deflate_level(level)`） | `hzip-codec::level` + `hzip-fmt-zip` |
| 7z | LZMA2（唯一，写死） | `hzip-fmt-7z/src/writer.rs:293` `SevenZMethod::LZMA2` |
| tar | 无（仅打包） | `hzip-fmt-tar/src/caps.rs`（无压缩层） |
| gz | Deflate（gzip 容器） | `hzip-fmt-single` |
| bz2 | BZip2 | 同上 |
| xz | LZMA2（xz 容器） | 同上 |
| zst | Zstd | 同上 |

`hzip-codec` 已具备的编码器全集：`store` / `deflate` / `bzip2` / `xz`(LZMA2) / `zstd` + `filter::{bcj, bcj2}`。

### 5.2 本设计主张

| 格式 | 「压缩方式」候选 | 默认 | 说明 |
|---|---|---|---|
| zip | Deflate | Deflate | 后续可加 BZip2 / Zstd（codec 已有，需 zip 容器侧接线） |
| 7z | LZMA2 | LZMA2 | 后续可加 BZip2 / Deflate / Zstd |
| tar | —（禁用，显示「不压缩（仅打包）」） | — | tar 无压缩层 |
| gz | Deflate（锁定） | Deflate | 单候选 → 禁用 + 灰字 |
| bz2 | BZip2（锁定） | BZip2 | 同上 |
| xz | LZMA2（锁定） | LZMA2 | 同上 |
| zst | Zstd（锁定） | Zstd | 同上 |

> **「存储」不在本轴**：hzip 把「不压缩」表达在**级别轴**上（`Level::Store` → `deflate_level=0`、`lzma2_params` 取 `Mode::Copy`、字典 0）。7-Zip 的 method 列表里有 `Copy`，本设计不重复列，避免同一语义两个入口。

**实现路径**（供后续任务卡）：`FormatCaps` 增 `methods: &'static [CodecMethod]`；`CreateOptions`/`CompressSpec` 增 `method: Option<CodecMethod>`（`None` = 沿用格式默认）；`hzip-codec` 增 `CodecMethod` 枚举与 `level → 各编码器实参` 的分派。缺口登记见 §10。

---

## 6. 压缩级别：比例 ↔ 速度

### 6.1 交互设计

- **控件**：gpui-kit `Slider`，`SliderState::new().min(0.).max(5.).step(1.)`（`gpui-base::slider::SliderState` 提供 `step(f32)`，离散档位原生支持）。
- **刻度**：6 个档位名（存储/最快/快速/标准/最大/极限，复用 `labels::LEVEL`）居中于各自档点，选中项以 accent 色加粗。
- **两端语义**：轨道上方左端「更快 · 体积大」、右端「更慢 · 体积小」——把「比例/速度」的权衡直接画出来。
- **实参提示行**：滑块下方一行灰字，显示该档位在**当前格式**下真正生效的参数 + 预估值，例如
  - zip：`Deflate 级别 6 · 速度 中 · 预计压缩率 ~68%`
  - 7z：`LZMA2 字典 64 MB · 模式 normal · 压缩内存 ~700 MB · 预计压缩率 ~31%`
  - zst：`zstd 级别 5 · 速度 快 · 预计压缩率 ~54%`
  - tar：`不压缩（仅打包）`
- **键盘 / 无障碍**：`←/→` 步进一档，`Home/End` 到两端；`Slider` 自带 `aria_numeric_value_step`。若评审认为滑块牺牲精确性，退路是「档位名 `Select` + 滑块」（同一 `Level` 值两个投影，纯 UI 改动）。

> 级别滑块**不需要改数据面**：`CompressDialogVm.level` 就是 6 值枚举 `Level`，滑块只是它的一个可视化投影。

### 6.2 级别 → 实参映射（`hzip-codec/src/level.rs` + `consts.rs` 实值）

| 级别 | zip deflate | 7z LZMA2 字典 / 模式 | xz preset | zstd | bzip2 块 |
|---|---|---|---|---|---|
| 存储 store | 0 | 0 / Copy | 0 | 0 | 0 |
| 最快 fastest | 1 | **64 KB** / Fast | 1 | 1 | 1 |
| 快速 fast | 3 | **1 MB** / Normal | 3 | 5 | 3 |
| 标准 normal | 6 | **16 MB** / Normal | 6 | **9** | 6 |
| 最大 maximum | 9 | **64 MB** / Normal | 9 | **16** | 9 |
| 极限 ultra | 9 | **256 MB** / Normal | 9 + extreme | **22** | 9 |

> 注：`Level::Store` 在 zstd / bzip2 下 codec 层不可用（档案层改走 `hzip-codec::store` 恒等路径）。
> 字典 256 MB（ultra）在 xz 解码侧有 ~512 MB 峰值内存风险（`xz.rs` rustdoc 已标注），提示行应把内存代价显式写出。

---

## 7. 联动与校验规则

### 7.1 格式变更的联动（一次变更四处跟随）

| 跟随项 | 规则 |
|---|---|
| 扩展名 chip | zip→`.zip`、7z→`.7z`、tar→`.tar`、gz→`.gz`、bz2→`.bz2`、xz→`.xz`、zst→`.zst`；多文件/目录 + 单流格式 → `.tar.gz` / `.tar.bz2` / `.tar.xz` / `.tar.zst`（见 7.2） |
| 压缩方式候选 | 按 §5.2 重建；原选中项不在新候选中 → 回落该格式默认 |
| 高级区门控 | 按 §4 重算禁用态与灰字原因 |
| 加密区门控 | 按 §4 重算；`caps.encryption != None` 才可编辑 |
| 摘要行 | 全量重算 |

### 7.2 单流格式的包装语义（需 core 支持）

`gz/bz2/xz/zst` 是**单文件流**格式。当前压缩路径 `writer_for` 直接按 `c.format` 取 handler（`crates/hzip-core/src/services/compress.rs:255`），**没有 tar 组合写**（组合写只存在于 `services/convert.rs`）。因此：

- **单文件输入** → `project.zst`，当前即可。
- **多文件 / 目录输入** → 期望 `project.tar.zst`（Bandizip/7-Zip 观感），**当前不成立**，需把 convert 服务的组合写管线接入 compress。

设计上把这条规则**显式告知用户**（格式行下灰字）：「zst 是单文件流格式：多文件 / 目录将先打包为 tar（project.tar.zst）」。是否在本期实现 tar 组合写，见 §13 待决策项 ②。

### 7.3 校验

| 时机 | 规则 |
|---|---|
| 实时 | 文件名空 → primary 禁用；非法字符 → 行内 danger；确认密码 ≠ 密码 → 行内 danger；分卷自定义值 ≤ 0 或超输出预期 → warn |
| 提交前 | 目标已存在 → 走 `unique_out_path` 追加 `_1`/`_2`… 并在弹窗内先行告知（**绝不静默覆盖**，既有语义） |
| 提交时 | `CompressDialogVm::build_spec` 现有 5003 语义校验保持（缺 format / 空 inputs）；选择结果 → `JobSpec` |

---

## 8. 状态

| 状态 | 表现 |
|---|---|
| 默认 | 基本区 + 源卡片 + 摘要；两折叠区收起 |
| 展开 | 折叠区展开；正文区滚动（`flex_1 + min_h_0 + overflow_y_scrollbar`），头/脚固定 |
| 格式受限 | 不可用控件禁用 + 灰字原因；折叠区标题右摘要写「不支持」 |
| 加密未启用 | 灰字提示（沿用 `dlg.compress.write_encryption_disabled`）+ 摘要标「无加密」 |
| 目标已存在 | warn 提示行（先行告知改名结果） |
| 校验失败 | danger 行内提示 + primary 禁用 |
| 提交中 | primary 禁用 + `Spinner`；取消按钮 → 「中止」语义（停止入队，非中断运行中任务） |
| 深色主题 | 同一批 token 槽位反色（`theme.rs::dark`），控件语言不变（视觉稿 C 面板） |

---

## 9. gpui-kit 组件映射与装配

| 设计元素 | gpui-kit 组件 | 备注 |
|---|---|---|
| 面板外壳 | `panel_shell`（窗口内容视图：头部/滚动正文/底部） | **2026-09 返工**：参数界面不得用 `Dialog` 弹层（无 `max_h` → 底部按钮被顶出窗外、正文滚不动）。Esc 关窗由根视图 `on_key_down` 接管；`Root` 仍是窗口根（Select 下拉/native menu 悬浮层） |
| 表单布局 | `Form::horizontal().label_width(px(96.))` + `FormField` | 沿用现行 `dialog_form()` |
| 格式 / 压缩方式 / 分卷 / 字典 / 固实 / 线程 / 更新方式 | `Select` + `SelectState<SearchableVec<String>>` | 沿用 `select_string_state` 预构建通道（窗口亲和） |
| 文件名 / 保存到 / 排除 / 密码 / 确认密码 | `Input` + `InputState` | 沿用 `input_state`；密码走 `masked` + 提交即清空 |
| 自定义分卷值 | `NumberInput` + `MaskPattern::number` | 沿用 `number_input_state` |
| **压缩级别** | **`Slider` + `SliderState`（`min/max/step`）** | 新增使用；`SliderState` 在 `gpui-base::slider`，经 `gpui_kit` 再导出 |
| 两个折叠区 | `Accordion`（或 `Collapsible`） | `gpui-component/src/accordion.rs` / `collapsible.rs` |
| 五个开关 | `Checkbox` / `Switch` | 现解压对话框用 `Switch`，压缩区多开关建议 `Checkbox` 两列排布 |
| 源摘要卡 / 摘要行 | 自组 div（inset 底色 + 1px 边框 + `radius` 6） | kit 无对应件；`DescriptionList` / `GroupBox` 为备选 |
| 按钮 | `Button`（`primary()` / secondary） | 沿用 |
| 内容区滚动 | gpui-kit `ScrollableElement::overflow_y_scrollbar()` | 挂在**正文自身**（`flex_1 + min_h_0`，滚动根保留调用方尺寸）；头/脚在滚动容器之外 |

**装配契约**：面板渲染函数 `fn(cx: &mut App, props: &CompressPanelProps, on: impl Fn(DialogEvent)) -> impl IntoElement` 保持纯 props 形态（返回的是**窗口内容视图**，不是 `Dialog`）；`SelectState`/`InputState`/`SliderState` 实体由装配方（`views::op_window::build_controls`）在**参数窗 builder** 内预构建后放入 props（窗口亲和）。

**新增 `DialogEvent` 变体**：现有 `CompressCancel` / `CompressStart` / `CompressBrowse` 之外，需增 `CompressFormat(usize)`、`CompressMethod(usize)`、`CompressLevel(usize)`、`CompressSplit(u64)`、`CompressDict(usize)`、`CompressSolid(bool)`、`CompressThreads(u32)`、`CompressUpdateMode(bool)`、`CompressComment(String)` 及五个开关的布尔事件。

---

## 10. 数据面缺口与需新增项

| # | 缺口 | 影响 | 建议落点 |
|---|---|---|---|
| ① | **无「压缩方式」字段** | 「压缩方式」Select 无处落值（7z 写死 LZMA2、zip 写死 Deflate） | `FormatCaps.methods: &'static [CodecMethod]`；`CreateOptions.method` / `CompressSpec.method: Option<CodecMethod>`；`hzip-codec` 增 `CodecMethod` + 分派 |
| ② | **无 tar 组合写（压缩路径）** | 选 gz/bz2/xz/zst + 目录 → 拿不到 `.tar.gz` | 把 `services/convert.rs` 的组合写管线接入 `compress.rs::writer_for` |
| ③ | **无字典 / 固实块显式覆写** | 字典只能随级别派生 | `CreateOptions` 增 `dict_size: Option<u64>` / `solid_block: Option<u64>`（`Lzma2Params` 已可承载） |
| ④ | **加密写路径未交付** | 加密区只能是「记录参数 + 提示」 | T45（zip 写加密）/ T46（7z 头加密写）；本设计按其交付前景留位 |
| ⑤ | 档案注释无格式支持 | 该控件恒禁用 | 保持禁用并在 §13 决策 ④ 定去留 |

**i18n key 新增**（沿用 `locales/dialogs.toml` + `locales/options.toml` 的 `en` / `zh-CN` 双列）：

- 字段标签：`field.filename`、`field.out_dir`、`field.method`、`field.split_size`、`field.dict_size`、`field.solid_block`、`field.threads`、`field.update_mode`、`field.comment`、`field.password_confirm`、`field.encryption_method`、`field.encrypt_names`
- 分区标题：`dlg.compress.sec_basic`、`dlg.compress.sec_advanced`、`dlg.compress.sec_encryption`
- 提示：`dlg.compress.hint_stream_wrap`、`dlg.compress.hint_split_names`、`dlg.compress.hint_solid`、`dlg.compress.hint_threads_7z`、`dlg.compress.hint_update_append`、`dlg.compress.hint_comment_unsupported`、`dlg.compress.hint_encrypt_disabled_fmt`、`dlg.compress.exists_renamed`
- 摘要：`dlg.compress.preview`（改写为多槽位）
- 下拉候选（`app::labels` 新增表，遵循「一处定义、双向映射、测试互锁」）：`opt.method.*`、`opt.split.*`、`opt.dict.*`、`opt.solid.*`、`opt.threads.*`、`opt.update.*`

**主题 token 覆写**（`ui/theme.rs::theme_json`）：需补 `slider.bar.background` / `slider.thumb.background`，否则滑块落在 gpui-component 默认「黑条 + 白手指」（见 §12 问题 ②）。

---

## 11. 与现状的差异清单（增量改动面）

| 文件 | 现状 | 本设计 |
|---|---|---|
| `ui/dialogs/compress.rs` | 8 个 `FormField`（格式/级别/输出/分卷/排除/密码/对象/预览） | 重写为分区布局 + 两折叠区 + 滑块；props 扩至 ~20 字段 |
| `ui/dialogs/mod.rs` | `dialog_form()` label_width 96、`read_only_field`、`error_line` | 增 inset 卡片与提示行 helper；`read_only_field` 底色修正（§12 问题 ①） |
| `viewmodel/dialog_vm.rs` | `CompressDialogVm` 6 字段 | 扩为 `filename` + `out_dir` 拆分、`method`、`dict`、`solid_block`、`update_mode`、`comment`、五个开关 |
| `app/labels.rs` | `FORMAT`(7) / `LEVEL`(6) / … | 增 `METHOD` / `SPLIT` / `DICT` / `SOLID` / `THREADS` / `UPDATE_MODE` 六张表 |
| `ui/theme.rs` | 未覆写 slider token | 增 `slider.*` 两槽位（浅深两套） |
| `locales/*.toml` | —— | 增 §10 所列 key |
| `ui/contracts.rs` | `DialogEvent` 3 个 compress 变体 | 增 ~13 个参数变更变体 |
| **core / format / codec** | —— | 仅 §10 缺口 ①②③ 需要；**其余纯 GUI 面** |

---

## 12. 设计过程中发现的问题（建议随实施一并修）

| # | 问题 | 证据 | 建议 |
|---|---|---|---|
| ① | **浅色下三面同色，视觉层次缺失**：`Dialog` 卡片用 `tokens.background`，浅色 `input_background()` 也返回 `theme.background`，`read_only_field` 同样用 `theme().background`——卡片、输入框、只读框全是 `#f7f7f5`，只靠 1 px 边框区分 | `gpui-component/src/dialog/dialog.rs:577`、`theme/mod.rs:340-346`、`ui/dialogs/mod.rs:92-104` | 卡片改用 `popover.background`（hzip 已挂 `#ffffff`）；只读框/摘要条改用 `secondary.background`（`#eef0f2`）形成凹面。视觉稿 D 面板为对照 |
| ② | **滑块 token 未覆写**（**v0.2 修正**：原判断有误） | 真实 serde key 是 `slider.background` / `slider.thumb.background`（`gpui-component/src/theme/schema.rs:534-538`），而 `default-theme.json` 里写的 `slider.bar.background` **是无效 key**（serde 不拒绝未知 key，静默丢弃）。该槽未配置时回落 `tokens.primary` / `primary_foreground`——hzip 已挂 `primary.background=ACCENT`，故滑块**本来就是主色**，并非黑条白手指 | 仍建议**显式声明**以免依赖 fallback：`theme.rs` 增 `"slider.background": accent`、`"slider.thumb.background": surface`（v0.2 已实施） |
| ③ | 摘要/对象行用 `preview_text`/`summary` 纯字符串拼装 | 现行 props 已有 `preview_text`/`summary` | 摘要改为结构化的槽位列表（键值对），便于 i18n 与逐项着色（warn 项突出） |
| ④ | 分卷数量与命名未提示 | `CompressSpec.split` 仅字节数 | 提示行写出产物命名（`project.7z.001 / .002 …`；zip 为 `.z01`） |
| ⑤ | 7z 线程数控件会误导 | `hzip-fmt-7z/src/writer.rs:28` 注释：sevenz-rust compress 为单线程 lzma2 | 7z 下线程控件禁用 + 灰字「LZMA2 当前为单线程编码」 |
| ⑥ | 写侧加密未启用 | `dialog_vm.rs` `build_spec` rustdoc + `dlg.compress.write_encryption_disabled` | 保持提示；§13 决策 ③ 定「可编辑」还是「整区禁用」 |

---

## 13. 待评审决策项

1. **弹窗宽度 560 px 是否接受**（现 480）？滑块刻度需要横向空间；若坚持 480，则刻度名改为仅显示当前档位名 + 两端箭头。
2. **单流格式的 tar 包装（`.tar.gz`）本期做不做**？做 → 需接 core 组合写（§10 缺口 ②）；不做 → 多文件 + gz/bz2/xz/zst 组合在该弹窗内**禁用并说明**，避免产出语义不明的裸流。
3. **加密区形态**：可编辑但提交时提示（本设计现行稿）／ 整区禁用只读（更保守，杜绝用户以为已加密）。
4. **档案注释**控件去留：当前无格式声明 `caps.comment`，是保留禁用位（预留能力）还是直接删掉。
5. **「压缩方式」是否本期落数据面**（§10 缺口 ①）？若不落，则该 `Select` 只能显示格式固定方法（只读），形态退化但保留位置。
6. **压缩级别用滑块还是下拉**？滑块直观（本设计主张）；下拉更精确、键盘友好。两者可并存（滑块 + 档位名 Select）。
7. **「保存到」是否需要独立浏览按钮**？本设计只在文件名行放一个「浏览…」（原生保存框一次回填目录+文件名）。
8. **多选时的文件名默认值**：沿用 `default_out_path` 的「取首项所在目录名」（`dir.zip`）是否合适，还是改为「首项名 + 等 N 项」或让用户强制输入。

---

## 14. 不在本期范围（登记备查）

| 项 | 理由 |
|---|---|
| 压缩后动作（保留/删除源文件、完成后打开目录） | `CompressSpec` 无对应字段；属 shell 行为，与 Bandizip 的差异留待后续评估 |
| SFX 自解压包 | 无实现 |
| PPMd / 单词大小 / Deflate64 | `hzip-codec` 无对应编码器 |
| 更新方式细分为 添加并替换 / 仅更新已有 / 同步 | `CreateOptions.append` 是布尔，细分需 core 扩展 |
| 参数预设保存/命名（「我的压缩配置」） | 需配置面扩展；本次先把参数集与布局定下来 |
| 预估压缩率/内存的精确计算 | 视觉稿中的「预计压缩率 ~68%」「压缩内存 ~700 MB」当前为**示意值**；落地需基于样本探测或历史统计，属独立需求 |

---

## 15. 实施记录（v0.2 · 已落地）

> 本节记录本设计的**实施结果**：改了哪些文件、§13 八个待决策项怎么定的、
> 与设计稿的偏差及理由、实施中发现并修掉的缺陷。设计稿正文（§1~§14）保持
> 原样作为评审依据，仅对 §12 问题 ② 做了事实更正。

### 15.1 改动面

| 层 | 文件 | 改了什么 |
|---|---|---|
| codec | `hzip-codec/src/traits.rs` | 新增 `CodecMethod`（Deflate/BZip2/Lzma2/Zstd 闭合枚举；`as_str`/`from_str`/`codec_id`/`display`）+ 2 个单测 |
| codec | `hzip-codec/src/level.rs` | `Mode::as_str()`（提示行展示用） |
| format | `hzip-format/src/caps.rs` | `FormatCaps::methods: &'static [CodecMethod]`（默认 `&[]`）；re-export `CodecMethod` 使公开字段类型可命名 |
| format | `hzip-format/src/handler.rs` | `CreateOptions::method: Option<CodecMethod>`（默认 `None`） |
| format | zip / 7z / tar / single / rar | 各自声明 `methods`（single 按变体分化：gz→Deflate、bz2→BZip2、xz→LZMA2、zst→Zstd） |
| core | `services/compress.rs` | `CompressSpec::method`；**唯一校验点** `writer_for`：越集 → HZ-5001 |
| GUI | `app/labels.rs` | 新增 `METHOD`/`SPLIT`/`THREADS`/`ENC_METHOD` 四张白名单表 |
| GUI | `locales/dialogs.toml` + `options.toml` | 新增字段标签 / 分区标题 / 能力原因行 / 级别实参模板 / 四组下拉候选 |
| GUI | `ui/theme.rs` | 显式声明 `slider.background` / `slider.thumb.background` |
| GUI | `ui/contracts.rs` | `DialogEvent` 增 11 个压缩参数变体 + 2 个折叠区变体 |
| GUI | `viewmodel/dialog_vm.rs` | `CompressDialogVm` 由 6 字段扩到 16；`Default` 改为**逐字段取 `CompressSpec::default`**；新增 `default_ext`/`is_stream_format`/`invalid_name_char`/`split_preset_bytes`/`split_custom_bytes`/`threads_available`/`level_detail`(`LevelDetail`) |
| GUI | `ui/dialogs/mod.rs` | `dialog_shell` 收 `impl IntoElement`（正文不再是单个 Form）；新增 `HintTone`/`hint_line`/`field_col`/`section_header`/`summary_card`/`inset_surface`/`level_detail_text`/`level_speed_key`/`level_slider_state`；`read_only_field` 底色修正 |
| GUI | `ui/dialogs/compress.rs` | 整体重写：源摘要卡 + 基本区 + 两折叠区 + 摘要卡 + 提示行；560px；props 由 9 字段扩到 39 |
| GUI | `ui/views/op_window.rs` | 控件实体扩到 15 个；`ParamsForm` 扩到 24 字段（输出改「文件名 + 保存到」派生）；新增 `format_caps`/`default_method_ix`/`renamed_target`/`compress_props`/`resync_controls`；事件全量接线 |

### 15.2 §13 待决策项的处置

| # | 决策项 | 本次处置 |
|---|---|---|
| ① | 弹窗宽度 560 | **采用 560**（与参数窗本体 560×660 同宽） |
| ② | 单流格式 tar 包装 | **不做 core 组合写**；改为**界面据实提示**（`dlg.compress.hint_stream_wrap`：多文件/目录 + gz/bz2/xz/zst 时明确说明「按单流写出、不含目录结构」），扩展名 chip 也据实显示 `.gz` 而非 `.tar.gz`——不承诺写不出的名字。组合写仍挂账（§15.6） |
| ③ | 加密区形态 | **可编辑 + 明确提示**（`dlg.compress.write_encryption_disabled`），并在摘要行标「已设密码（暂不写出）」；`build_spec` 不携带密码/加密请求（携带即 Unsupported 失败）。理由：整区禁用会让用户以为「这功能没有」，而提示能表达「已记录、未生效」 |
| ④ | 档案注释控件 | **删掉**（不渲染）。当前无格式声明 `caps.comment`，渲染一个恒禁用的控件是死重 |
| ⑤ | 压缩方式是否落数据面 | **落**（§15.1）——四参数之一不能是假的；`FormatCaps.methods` + `CreateOptions.method` + `CompressSpec.method` 全通，越集由 core 501 拒绝 |
| ⑥ | 级别滑块 or 下拉 | **滑块**（`SliderState.min(0).max(5).step(1.)`）；档位名排成一行、选中档高亮。`DialogEvent::CompressLevel` 保留未用，作为将来「下拉后备」的注入口 |
| ⑦ | 保存到是否独立浏览按钮 | **不独立**；仅文件名行一个「浏览…」（原生保存框一次回填目录 + 文件名，且若所选扩展名命中白名单格式则**顺带切格式**） |
| ⑧ | 多选默认文件名 | 沿用 `default_out_path`（多选取首项所在目录名）；**去重时机改了**：不再预填时加序号，而是提交点落实 + 界面先行告知（§15.4） |

### 15.3 与设计稿的偏差（登记）

| 设计稿 | 实装 | 理由 |
|---|---|---|
| 折叠区用 gpui-kit `Accordion` | **自绘可点击标题行** | `AccordionItem::on_toggle_click` 是 crate 私有（`accordion.rs:250`），外部只能拿 `Accordion::on_toggle_click(&[usize])`；而 `AccordionItem::open` 是**纯 prop**（无内部态、无 `use_keyed_state`），折叠态必须外部驱动，切换回调又依赖 gpui 的子→父冒泡顺序——该顺序无法离线验证。自绘把「点击 → 事件 → props → 重渲染」闭环握在手里 |
| 滑块下逐档对位刻度名 | **一行档位名 + 选中高亮** | gpui 无 `translate`/负边距，逐档对位会与轨道刻度错开约半个标签宽（两端最明显）。语义（有哪些档、当前在哪档）不丢，且不引入与控件宽度耦合的魔法值 |
| 提示行显示「预计压缩率 ~68%」 | **不显示** | 该数字无可信来源（§14 独立需求）。宁可只显示能兑现的实参（Deflate 级别 6 / LZMA2 字典 64 MB），也不编一个像模像样的百分比 |
| 不可用控件一律「禁用 + 原因行」 | 下拉/输入**禁用 + 原因行**；复选框**不渲染 + 原因行** | gpui-kit 的 `Checkbox`/`Switch` **无 `disabled`**（`Select`/`Input`/`Button` 有）。渲染一个点不动的勾选框比不渲染更糟 |
| 高级区含 字典大小 / 固实块 / 更新方式 | **未实装**（挂账 §15.6） | 三者都需扩数据面：`dict_size`/`solid_block` 需接 7z 编码器参数，更新方式需 `CompressSpec.append` 并验证 fmt 追加路径。为守住「界面上的每个控件都真的生效」，宁可这版不做 |
| 摘要行用 `DescriptionList` | 自绘 inset 卡 | 摘要是一行文本而非键值对列表；`DescriptionList` 的多行语义在这里反而绕 |
| 源摘要卡显示「3 个文件 · 2 个目录 · 48.2 MB」 | 单文件显示体量；单目录显示「目录 · 路径」；多选显示「首项 等 N 项」 | 递归统计整棵树要在渲染帧里做目录遍历——渲染帧不能做 IO |

### 15.4 实施中发现并修掉的缺陷

| # | 缺陷 | 怎么发现的 | 修法 |
|---|---|---|---|
| ① | **`CompressDialogVm` 若保留 `#[derive(Default)]`，`keep_metadata`/`verify_after` 会从「开」变「关」** ——用户什么都没动，压缩却不再保留时间戳、不再自检 | 扩字段时对照 `CompressSpec::default()` 才发现两处默认值要一致 | `Default` 改为**逐字段取 `CompressSpec::default()`**（只覆盖 `format`/`out` 两处对话框语义差异）+ 守护测试 `compress_vm_defaults_mirror_spec_defaults` |
| ② | **tar 选中时表单会发 `method: Some(Deflate)` → core 以 5001 拒绝整个压缩任务**（手改会直接坏掉 tar 压缩） | 新写的 `params_form_matches_quick_path` 断言「方式 ∈ caps.methods」时抓到 | `ParamsForm.method_ix` 改 `Option<usize>`，**`None` = 不显式指定**（安全默认）；装配方按 `caps.methods` 派生，空集合即 `None` |
| ③ | 参数窗 ✕/Esc/遮罩**恒发 `ExtractCancel`**（两种对话框都发同一个事件） | 改取消分支时读到 | 按种类发 `CompressCancel`/`ExtractCancel` |
| ④ | `read_only_field` 底色用 `theme().background`，与对话框卡片**同色**，只读框除 1px 边框外看不见 | §12 问题 ① 的落实 | 改 `secondary` 面（#eef0f2），并抽出 `inset_surface` 统一源摘要卡/摘要卡/只读行 |
| ⑤ | 「重名加序号」在**预填时**做 → 用户改名后序号失效/错位，且与提交实际用路径可能不一致 | 实施「文件名 + 保存到」拆分时暴露 | 去重移到**提交点**（`OpState::start`）落实，界面用 `renamed_target` **先行告知**「将保存为 x_1.zip」；`unique_out_path` 幂等 |
| ⑥ | 输入框/下拉各持值：切格式、浏览选路径改了表单却不同步实体 → 界面显示旧值、执行新值 | 实施切格式联动时暴露 | 新增 `resync_controls`，凡**非用户键入**造成的表单变更一律回灌实体 |
| ⑦ | 八个输入框的订阅逻辑复制八份，漏一个就是「能打字、参数没跟上」的静默缺陷 | 扩到 8 个输入框时 | 抽出 `bind_input` 收口 |

### 15.5 新增/强化的守护测试

| crate | 测试 | 守什么 |
|---|---|---|
| hzip-codec | `codec_method_str_roundtrip` / `codec_method_from_str_is_strict` | 标识往返；别名不宽容（`lzma`≠`lzma2`、`zst`≠`zstd`、`store` 不属方式轴） |
| hzip-fmt-single | `caps_methods_are_per_variant_single` | 四变体各锁唯一编码器（gz→Deflate、bz2→BZip2、xz→LZMA2、zst→Zstd），且不污染共享常量 |
| hzip-core | `compress_method_outside_caps_rejected` | tar + 任意方式 / 7z + Zstd → 5001，且**不留半成品档案** |
| hzip-core | `compress_method_within_caps_accepted` | 在册方式贯通；`None` 走格式默认（既有行为不变） |
| hzip-gui | `compress_vm_defaults_mirror_spec_defaults` | VM 默认 ≡ spec 默认（防 §15.4 ① 回归） |
| hzip-gui | `level_detail_follows_codec_tables` | 界面显示的字典/级别 ≡ `hzip_codec::level` 映射表（防档位错位） |
| hzip-gui | `default_ext_and_stream_formats` / `invalid_name_char_detects_windows_reserved` / `split_bytes_from_preset_and_custom` | 扩展名/单流判定、非法字符集、分卷换算 |
| hzip-gui | `compress_build_spec_carries_new_fields` | 新字段逐项落进 spec，且**密码/加密请求不进 spec** |
| hzip-gui | `params_form_matches_quick_path`（重写强化） | 共享子集一致 + 参数窗未动的旋钮 == VM 默认 + 方式 ∈ 格式 caps 且为默认（§15.4 ② 的来源） |
| hzip-gui | `param_window_defaults_follow_config` / `compress_form_encodes_empty_inputs_as_none`（改写） | 初始值随配置；输出路径派生与裁剪；`0`/空 → 不分卷 |
| hzip-gui | `theme_json` 两测（扩展） | `slider.*` 两槽浅深两套齐备；四张新白名单表与 `labels` 互锁 |

### 15.6 尚未实施 / 挂账

| 项 | 卡在哪 |
|---|---|
| ~~字典大小~~ | **v0.3 已实施**（见 §15.8 ①） |
| ~~更新方式（新建/追加）~~ | **v0.3 已实施**（见 §15.8 ①） |
| **固实块大小（7z solid）** | **v0.3 未实施，且已把「超声明」修掉**（见 §15.8 ②）——`caps.solid` 此前为 true 而 writer 从不消费 `opts.solid`。接通需改用 `SevenzWriter::push_archive_entries` 把整块作为一个 folder 压缩，并保留目录条目 `has_stream=false` 语义；`hzip-fmt-7z/src/solid.rs` 块模型（默认 64 MiB）已就绪，缺的是 writer 集成与互操作一致性测试。属**独立写侧卡片**，不宜与对话框改动同批交付 |
| tar 组合写（`.tar.gz` 等） | 需把 `services/convert.rs` 的组合写管线接入 `compress.rs::writer_for`（§10 缺口 ②）；本版界面据实提示而非承诺 |
| 写侧加密真正生效 | T45（zip 写加密）/ T46（7z 头加密写） |
| 预计压缩率 / 压缩内存预估 | 需样本探测或历史统计（§14） |
| 折叠区内容区滚动 | 本版展开态整体变高、由窗口承载（参数窗可缩放）；「内容区滚动 + 头脚固定」需再包一层滚动容器 |

### 15.7 实施中发现的环境 / 仓库缺陷（**非本设计范围**，建议单独修）

跑全量 `cargo test --workspace` 时发现 4 处与本次改动**无关**的失败；已逐一定位根因：

| 现象 | 根因 | 建议 |
|---|---|---|
| `hzip-cli::e2e::tc_cli_009_password_channels`、`hzip-fmt-zip::zipcrypto::zipcrypto_correct_password` 断言「明文不一致」 | 期望值文件是**文本夹具**，仓库内为 LF（blob 2720 B），而工作区因 `core.autocrlf=true` 且无 `.gitattributes` 被检出为 CRLF（2760 B，+40 个 CR）；解出的档案内容是 LF → 逐字节比较必然不等 | 加 `.gitattributes` 把夹具目录标为 `-text`（如 `crates/*/tests/fixtures/** -text`、`*.bin -text`），并 `git add --renormalize` 后重新检出。**夹具必须逐字节稳定**，行尾转换会破坏「档案内字节 == 期望字节」这类断言 |
| ~~`hzip-core::malicious_extract::{sec_005,sec_006}` panic「样本缺失」~~ | `tests-assets/` 被 `.gitignore` 忽略，样本需 `python tests-assets/malicious/build.py` 生成 | **已修（2026-10-13）**：两条用例改为**在测试内自造等价样本**（`build_ratio_bomb`/`build_total_bomb`），不再依赖 `tests-assets/`；CI 里对应的 `--skip` 已删除 |
| `hzip-platform --lib` 5 个 assoc/context_menu 用例 | 写 `HKCU\Software\Classes` 被拒（os error 5）——注册表权限受限环境 | 环境相关；CI 需可写 HKCU 或跳过 |

本次交付的验证口径：**排除上述 4 项**（均与改动无关）后，`cargo test --workspace` 64 个测试结果行全绿、0 失败；`cargo clippy --workspace --all-targets` 无警告。

---

### 15.8 v0.3 追加：两项落账 + 两个缺陷修复

#### 15.8.1 本轮落地（§15.6 三项挂账之二）

| 项 | 数据面 | 写侧（**此前已实装，本轮只是接通**） | 行为测试 |
|---|---|---|---|
| **字典大小** | `CreateOptions.dict_size: Option<u64>`；`CompressSpec.dict_size`；`labels::DICT` 白名单 | `hzip-fmt-7z::coder::lzma2_options` 增第三参 `dict_override`：`Some` **取代**级别派生字典，越界钳到 `[4096 B, 256 MiB]`；AES 惰性装配路径同用一份（否则开了加密字典会悄悄回落到级别值） | `compress_dict_override_reaches_encoder`：同输入同级别、仅字典不同 → 产物**逐字节不同**（参数被忽略或被级别盖回时两者会完全相同），且两者均可列出条目 |
| **更新方式** | `CreateOptions.append` 此前被 core 写死 `false`；本轮增 `CompressSpec.append` 并透传 | zip（`ZipWriterImpl::append`）、裸 tar（`append.rs` 尾零块截断法）**已实装**；7z 与单流格式**正确地显式拒绝**（HZ-5001） | `compress_append_keeps_old_entries`：新建 → 追加第二个文件 → 档案内**同时**含旧、新两条（若被误当新建，条目数会变成 1，本断言即失败） |

> 界面落地：高级区新增「字典大小」（仅 7z 可用，其余格式禁用 + 原因行）与「更新方式」（`caps.append`
> 才可选追加，不支持时禁用 + 原因行——**绝不静默新建**）；折叠区就地摘要改为 `分卷 · 线程 · 字典`。
> 设计稿 §3.2 的「固实块」本轮未实装，理由见 15.8.2 ②。

#### 15.8.2 本轮发现并修掉的两个缺陷

| # | 缺陷 | 根因 | 修法 |
|---|---|---|---|
| ① | **右键「添加到压缩文件…」弹出的参数窗一片空白**（用户实测报障） | gpui-component 契约：`Root::render` 只渲染 `view`/tooltip/native_menu，**不挂载对话框层**——该层必须由**根内容视图**自己挂。主窗口早在 `ui/shell/mod.rs` 按同款修过（R-GUI4 #4），**但参数窗的根视图漏了**：`Background` 只渲染一块空背景 → `open_dialog` 把对话框登记进 `Root::active_dialogs` 后无人渲染 → 窗口空白。**登记成功而无人渲染，症状与「根本没打开」完全一样**——这正是它长期未被发现的原因 | `Background::render` 挂上 `Root::render_sheet_layer` + `render_dialog_layer`；新增回归测试 `params_window_mounts_dialog_layer`，断言面取「层**真的被画出来**」（gpui-component 给该层打了 `debug_selector("dialog-layer")`，其源码注释写明该 id 就是留给测试断言的）。**该测试已验证有牙**：临时撤掉挂载即失败，且报的正是本根因 |
| ② | **7z 的 `caps.solid` 是超声明** | `caps.solid` 在 feature 开启时为 true，但 `writer.rs` 从不读 `opts.solid`——`solid.rs` 的 `SolidBuffer` 只被其自身测试引用，条目仍逐条经 `push_archive_entry`（= 一条目一 folder）。即「固实」过去是**静默空操作**：用户勾了以为压缩率会变好，实际一点没变且无从察觉 | cap 如实改为 `false`；`create_writer` 对 `opts.solid=true` **显式拒绝**（接受而忽略比拒绝更坏）；新增 `solid_request_is_rejected` 守护。接通后（`push_archive_entries`）再把 cap 改回 |

> ② 的影响面值得单独记一笔：**7-Zip 的 7z 默认就是固实**，而 hzip 的 7z 输出是非固实的
> （一条目一 folder）。多文件目录树的压缩率因此会明显差于 7-Zip——这不只是「少一个选项」，
> 而是一个**输出质量差距**。建议单独立卡，交付物应含与 7-Zip 产物的互操作一致性用例。

---

## 附：设计稿资产

| 文件 | 说明 |
|---|---|
| [`assets/09-compress-dialog-mockup.png`](assets/09-compress-dialog-mockup.png) | 视觉稿（A 默认态浅色 / B 展开态 / C 深色 / D 视觉层次对照） |
| [`assets/09-compress-dialog-mockup.html`](assets/09-compress-dialog-mockup.html) | 视觉稿源文件（自包含 HTML，色值逐值取自 `ui/theme.rs`，可直接改重渲染） |

> 视觉稿说明：色值、圆角、间距、控件形态按 `ui/theme.rs` 的 token 与 gpui-kit `Dialog`/`Form`/`Input`/`Select`/`Button` 的实测样式绘制；**并非 gpui 真实渲染截图**，实装后以真机渲染为准。
