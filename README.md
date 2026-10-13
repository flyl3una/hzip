# hzip

**归档压缩/解压工具**：原生图形界面 + 可脚本化命令行。读写 zip / 7z / tar / gz / bz2 / xz / zst
（含 `tar.*` 组合档案），只读解压 rar / cpio；解压侧默认带路径穿越净化与配额防线。

| | |
|---|---|
| 开发语言 | **Rust**（edition 2024，MSRV **1.95**，见 `rust-toolchain.toml`） |
| 界面 | GUI：`gpui-kit`（gpui-pre 0.3.8）· CLI：`clap` 4 |
| 平台 | Windows 10+ / Linux（x86_64、aarch64）/ macOS（universal：x86_64 + Apple Silicon） |
| 许可 | **GPL-3.0**（正文见 [`LICENSE`](LICENSE)） |
| 源码 | <https://github.com/flyl3una/hzip> · 问题反馈：<https://github.com/flyl3una/hzip/issues> |

---

## 功能

**图形界面（`hzip-gui`）**

- **浏览**：左树 + 右栏三种渲染（详细列表 / 图标网格 / 紧凑列表），面包屑与「`..` 跳上级」，
  点进内层档案（组合档案 `tar.gz`、嵌套容器），过滤/查找，目录行聚合大小；
- **预览**：文本（自动检测 GBK 等编码，可强制代码页）、图片、HEX；双击条目行为可在设置里选
  「内置预览 / 外部应用」；
- **解压**：冲突策略（询问/覆盖/跳过/重命名/仅更新）、符号链接策略、坏条目「尽力而为」、
  「优雅解压」（多顶层自动包一层档案名目录）、解压后删除档案；
- **新建档案/压缩参数窗**：格式、级别、分卷、排除 glob、跟随符号链接、写后自检、输出路径（入口见「使用」）；
- **就地编辑**：重命名、新建文件夹、删除、添加文件（zip/7z 支持）；
- **复制粘贴**：进程内剪贴板 + 从系统剪贴板（Windows）粘贴外部文件列表，支持目录递归物化；
- **任务与进度**：任务中心、独立进度窗口、速度/剩余时间、可取消；多窗口；
- **外观**：深浅色主题、主题色、字体族/字号、布局密度、行高、工具栏样式、文件类型图标
  （与系统逐类型一致或纯内置）；
- **i18n**：中文/英文双语，界面语言跟随系统或手动指定。

**命令行（`hzip`）**：六个子命令 + `--json` 双流（stdout 结果文档 / stderr NDJSON 进度）+ 冻结的退出码契约：

| 子命令 | 别名 | 作用 |
|---|---|---|
| `compress` | `c` | 压缩（格式/级别/输出/排除/分卷/加密） |
| `extract` | `x` | 解压（目标目录/冲突/符号链接/条目筛选/代码页） |
| `list` | `l` | 列表（`--tree` 目录树 / `--json`） |
| `preview` | `p` | 打印条目内容（截断上限可调） |
| `test` | `t` | 逐条目 CRC 完整性校验 |
| `convert` | `cv` | 格式转换（如 zip → 7z） |

**安全默认（解压不可信输入）**：条目名路径穿越净化（`..`/绝对路径/控制字符/Windows 保留名）、
压缩炸弹防线（压缩比率、条目数、单条目与总解压量上限、磁盘余量预检）、符号链接策略
（`deny-escape` / `skip` / `flatten` / `deny-all`）、原子写入 + 写后自检、失败即回滚。

**系统集成**

| 平台 | 集成 |
|---|---|
| Windows | 资源管理器右键菜单（经典注册 + Windows 11 稀疏包，安装即用）、文件关联与「打开方式」登记、PATH 与 App Paths、MSI 安装向导（含卸载） |
| Linux | `.desktop` + AppStream 元数据、图标主题、Nautilus / Dolphin / Thunar 脚本 |
| macOS | universal `.app` 与 `dmg`、Finder「在 Finder 中显示」与「打开方式」 |

---

## 支持格式

| 格式 | 读 | 写 | 就地编辑 | 追加 | 写侧加密 | 分卷 |
|---|:--:|:--:|:--:|:--:|:--:|:--:|
| `zip` | ✅ | ✅ | ✅ | ✅ | ZipCrypto、AES-128/192/256 | ✅ |
| `7z` | ✅ | ✅ | ✅ | — | AES-256（含**头加密**＝文件名加密） | 读 ✓ / 写 — |
| `tar` | ✅ | ✅ | — | — | — | — |
| `gz` `bz2` `xz` `zst` | ✅ | ✅ | — | — | — | — |
| `tar.gz` `tar.bz2` `tar.xz` `tar.zst` | ✅ | ✅ | — | — | — | — |
| `rar`（4/5、多卷、固实） | ✅ | — | — | — | 读 AES-256 | 多卷读 |
| `cpio`（newc/crc/odc/bin） | ✅ | — | — | — | — | — |

> **分卷**：zip 读写都支持（`--split`）；7z **读**支持多卷（`.7z.001`、`.7z.002`…），写侧分卷尚未接通——`caps.split=false`，CLI/GUI 会直接拒绝（HZ-5001，待办 T46）。
>
> **固实**：7z 固实块打包同样尚未接通（`caps.solid=false`）。

其他能力：BCJ2 可执行文件过滤（7z）、稀疏文件（tar）。

---

## 编译

工具链由 `rust-toolchain.toml` 钉住（**1.95.0**，含 rustfmt/clippy），依赖与服务端/CI 同源，
细节见 [`build/README.md`](build/README.md)「编译环境依赖」。

```bash
git clone https://github.com/flyl3una/hzip.git
cd hzip

# CLI + GUI（release）
cargo build --release -p hzip-cli -p hzip-gui
# 产出：target/release/hzip（CLI）、target/release/hzip-gui（GUI）

# 直接跑起来
cargo run --release -p hzip-gui      # 图形界面
cargo run --release -p hzip-cli -- --help   # 命令行
```

**平台依赖**（摘要；完整对照表见 [build/README.md](build/README.md)）：

| 平台 | 需要 |
|---|---|
| Windows | VS 2022 Build Tools（C++ 生成工具 + Windows SDK）、`nasm` |
| Linux | `build-essential pkg-config cmake nasm perl libfreetype-dev libfontconfig1-dev libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev` |
| macOS | Xcode Command Line Tools（`cmake` 自带；arm64 不需要 `nasm`） |

**测试与检查**（CI 跑的就是同一套，见 [`.github/workflows/ci.yml`](.github/workflows/ci.yml)）：

```bash
cargo test --workspace          # 全工作区测试（gui 用例需串行：-- --test-threads=1）
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
python build/checks/deps-direction.py && python build/checks/layout-conventions.py
```

> 两条**已记录的 flake**（`ui::views::browse::tests::rename_and_new_folder_commands_enter_inline_edit`、
> `hzip-fmt-single::cancel_mid_extract_cleans_partial`）是既有的竞态用例，隔离跑也可能随机报红：
> CI 里对它们加了 `--skip`，逐条理由与恢复条件见 [`.github/workflows/ci.yml`](.github/workflows/ci.yml)
> 的注释与 [`docs/tasks/`](docs/tasks) 下的 T-OPT / T-STD 记录。
>
> **别加 `--release` 跑 GUI 用例**：界面用例靠 gpui-pre 的 `debug_bounds` 取锚点边界，而
> `debug_selector` 在 release 下是**有意的 no-op**（上游注释：省掉每帧记边界的开销）⇒ `cargo test
> --release -p hzip-gui` 会整片报「锚点缺失/未绘制」（实测 55 条，与产品代码无关）。这一组合现在是
> **编译期错误**，原因写在同一处（`crates/hzip-gui/src/lib.rs` 的 `compile_error!`）；要看 release
> 档行为请 `cargo build --release` 后手工跑，release 档下要跑非界面用例用 `cargo test --release -p <crate>`。

**打包安装包**（一条命令出一个平台的全部产物，不构建二进制）：

```bash
cargo build --release --target <triple> -p hzip-cli -p hzip-gui

python build/package/package.py --target x86_64-pc-windows-msvc --formats msi,zip
python3 build/package/package.py --target x86_64-unknown-linux-gnu --formats tar.gz,deb,rpm,appimage
python3 build/package/package.py --target x86_64-apple-darwin,aarch64-apple-darwin --formats dmg,tar.gz
```

CI 上打 tag（`vX.Y.Z`）即自动跑全平台矩阵并建 Release；用法与「关掉某一格」的开关见
[build/package/README.md](build/package/README.md) 与 [04-编译与部署.md](docs/design/04-编译与部署.md) §6。

---

## 使用

### 图形界面

1. **打开档案**：双击已关联的档案（安装包已登记「打开方式」），或工具栏/菜单 **文件 ▸ 打开压缩文件…**
   （`Ctrl+O`）；「打开▾」下拉里有**最近打开**与清空历史，`Ctrl+Shift+N` 另开一个新窗口；
2. **浏览**：左树 + 右栏（详细/图标/紧凑三视图）；双击目录进入、双击内层档案进入容器（`tar.gz`、
   嵌套档案），点 `..` 返回上级；`Ctrl+F` 过滤，`F5` 刷新；
3. **解压**：选中条目（不选＝全部）按 `Ctrl+E`，或用工具栏「解压▾」选五种方式：自动解压 / 解压到
   当前文件夹 / 解压到「压缩文件名」文件夹 / 解压到桌面 / 自动解压到桌面；冲突、密码、符号链接
   策略在对话框内确认；
4. **就地编辑**（zip / 7z）：右键菜单重命名 / 新建文件夹 / 删除，工具栏「添加」把外部文件加进
   档案内当前目录；提交后原子重写（加密档案需密码）；
5. **新建档案**：入口在系统集成处——资源管理器 / 文件管理器里选中文件 → 右键「添加到 X.zip」
   （按默认格式直接出输出窗）或「添加到压缩文件…」（弹参数窗：格式、级别、分卷、排除规则、输出路径），
   也可直接用命令行 `hzip c`（见下）；
6. **任务与进度**：`Ctrl+J` 任务中心（页签默认隐藏，可在设置里常显），压缩/解压时另有独立进度窗；
   `Ctrl+C` / `Ctrl+V` 在应用内复制粘贴条目，并可粘贴系统剪贴板里的外部文件（Windows）；
7. **设置**（`Ctrl+,`）：七个页签——常规 / 压缩设置 / 解压设置 / 查看器 / 上下文菜单 / 文件关联 / 语言；
   右键菜单扩展与文件关联都在此页点「应用」生效。

| 位置 | Windows | Linux | macOS |
|---|---|---|---|
| 用户配置 | `%APPDATA%\hzip\config.toml` | `~/.config/hzip/config.toml` | `~/Library/Application Support/hzip/config.toml` |
| 日志 | `%LOCALAPPDATA%\hzip\logs` | `~/.local/state/hzip/logs` | `~/Library/Logs/hzip` |

配置模板与全部键的说明见 [`etc/hzip.toml`](etc/hzip.toml)（首次运行自动落盘；`HZIP_CONFIG`
可覆盖路径）。配置文件里的地址/开关都可就地改，改完重启生效。

### 命令行

```bash
# 压缩：把 dir/ 与 a.txt 打成 7z（级别 maximum，排除临时文件）
hzip c -f 7z -L maximum -o out.7z --exclude '*.tmp' dir/ a.txt

# 分卷压缩：目前只有 zip 支持写侧分卷（7z 写侧未接通，会报 HZ-5001）
hzip c -f zip -o split.zip --split 100m dir/ a.txt

# 加密压缩：密码从环境变量取（AES-256；7z 默认连文件名一起加密）
HZIP_PASSWORD=secret hzip c -f 7z -o secret.7z --encryption aes256 docs/
MY_PW=secret hzip c -f zip -o s.zip --password-env MY_PW --encryption aes128 docs/

# 解压：优雅解压 + 冲突重命名 + 只挑几个条目（--select 是**条目名精确匹配**，可多次）
hzip x big.tar.gz -o ./out --smart --on-conflict rename \
  --select 'src/main.rs' --select 'README.md'

# 列表 / 目录树 / JSON（stdout 结果 + stderr NDJSON 进度）
hzip l out.7z --tree
hzip l out.7z --json | jq '.entries | length'

# 预览条目（截断上限可调）、完整性校验、格式转换
hzip p out.7z 'src/main.rs' --max-bytes 4096
hzip t out.7z
hzip cv out.zip --to 7z -L maximum

# 档案路径写 `-` 即从 stdin 读（列表/解压/预览/校验都支持）
curl -sL https://example.com/x.tar.gz | hzip l -
```

**密码三通道**（优先级高 → 低）：`--password-env <VAR>`（变量缺失立刻报 `HZ-5003` 退出 2）→
环境变量 `HZIP_PASSWORD` → 终端隐藏输入（可重试 3 次）。stdin 非 TTY 时**永不**进入交互等待
（读不到即以空密码继续，加密条目最终收敛 `HZ-3101` 退出 2），便于脚本/CI 使用。

**退出码**（冻结契约）：`0` 全成功 · `1` 部分失败（汇总报告）· `2` 失败（格式/校验/IO/安全/配额/密码）
或参数错误 · `130` 用户取消。

---

## 开发语言与项目结构

Rust（edition 2024）。工作区按「格式 / 引擎 / 平台 / 前端」分层，**依赖方向单向**由
`build/checks/deps-direction.py` 在 CI 把关：

| crate | 职责 |
|---|---|
| `hzip-cli` / `hzip-gui` | 两个前端（`hzip` / `hzip-gui` 可执行文件） |
| `hzip-core` | 引擎与任务编排、解压/压缩/编辑/预览服务、配额与净化管线 |
| `hzip-format` | 格式抽象（`FormatHandler` / 注册表 / 能力矩阵 / 路径净化） |
| `hzip-fmt-zip` `-7z` `-tar` `-rar` `-cpio` `-single` | 各格式实现（`single` = gz/bz2/xz/zst 单流） |
| `hzip-codec` | 压缩编码（deflate / LZMA(2) / xz / zstd / bzip2、BCJ2 过滤、级别映射） |
| `hzip-platform` | 平台层：目录、元数据/属性、符号链接、长路径、文件锁、临时区、watch、剪贴板、右键菜单、文件关联、打开方式 |
| `hzip-config` `hzip-log` `hzip-progress` `hzip-error` | 配置、日志、进度事件总线、错误码与文案 |
| `hzip-format-testkit` / `xtask` | 开发用：黄金档案一致性套件、构建/环境检查工具 |

设计文档（架构、格式细节、GUI、打包与合规）在 [`docs/design/`](docs/design)，任务卡与实施记录在
[`docs/tasks/`](docs/tasks)，构建脚本入口见 [`build/README.md`](build/README.md)。

---

## 许可

本项目以 **GNU General Public License v3.0 or later（`GPL-3.0-or-later`）** 发布，正文见仓库根
[`LICENSE`](LICENSE)（GPL-3.0 全文，唯一事实源）；crate 元数据与安装包元数据里的 SPDX 标识同源。
发行包（MSI / deb / rpm / AppImage / dmg / tar.gz / zip）都会自动附带 `LICENSE` 全文。

- **对应的源代码**：<https://github.com/flyl3una/hzip>（发布的 tag 与产物一一对应）。
- **第三方组件**：依赖许可审计由 [`deny.toml`](deny.toml) 约束（`cargo deny check licenses advisories`，
  CI 门禁），允许与 GPLv3 兼容的协议、拒绝与 GPLv3 不兼容者。其中 **unrar** 的 C 源码以 UnRAR 许可
  随包分发：该许可允许「解压用途」免费分发、禁止用它实现 RAR 创建——hzip 只把该库用于解压
  （D2：不提供 RAR 创建），约束满足；不接受该许可的发行版可构建
  `cargo build --release --no-default-features -p hzip-cli --features default-formats` 剔除 rar（04 §3.2）。
- 各产物还会带 `sha256sums.txt`，便于校验完整性。

---

## 状态

当前版本 **0.1.2**（`v0.x` 期间 minor 递增，`v1.0` 起遵循语义化版本）。已知限制：

- 在线更新检查尚未接入（配置项 `[update].check_url` 已就位，菜单 **帮助 ▸ 检查更新** 目前只弹占位提示）；
- 写侧编码器精简：zip 只写 Deflate（读支持更广）、7z 只写 LZMA2；
- 跨应用（系统）剪贴板：**粘贴外部文件列表**目前仅 Windows；其他平台走应用内剪贴板；
- `rar` / `cpio` 只读；`tar` 不支持就地编辑；
- 分卷**写**目前仅 zip（7z 多卷可读，写侧未接通）；7z 固实块打包未接通（`caps.solid=false`）。
