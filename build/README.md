# build/ — hzip 构建与打包脚本

仓库里**所有非源码脚本**都收在这一层，根目录只留源码、文档与配置。
（唯一例外：CLI 端到端测试运行器 `script/cli-test/**` —— 它是测试工具、不属于构建链，按约定原地保留。）

| 目录 | 干什么 | 入口 | 设计依据 |
|---|---|---|---|
| `package/` | **跨平台安装包**：Windows MSI/zip、macOS dmg/tar.gz、Linux deb/rpm/AppImage/tar.gz | `python build/package/package.py --target <triple> --formats <…>`（详见 [`package/README.md`](package/README.md)） | 04 §5、§6.1 |
| `icons/` | 程序图标：ICO（9 档）+ 稀疏包磁贴；lucide 图标资产入库 | `./build/icons/make-icons.ps1`、`./build/icons/fetch-lucide-icons.sh` | 04 §5.5 |
| `shell-ext/` | 资源管理器右键菜单扩展：`hzip-shell.dll` + `hzip-shellmenu.msix` | `./build/shell-ext/build-shell-ext.ps1 -Publisher "CN=hzip"` | 04 §5.1、05 T43 |
| `checks/` | 仓库级质量检查（CI 门禁） | `python build/checks/deps-direction.py`（02 §2.1 五条依赖红线） | 04 §3.1、§6.1 |
| `../script/cli-test/` | CLI 端到端测试运行器（**不在本层**，见上） | `./script/cli-test/run-cli-tests.sh` | 03 |

## 常用命令

```bash
# 依赖方向红线（CI 的 quality job 跑的就是它）
python build/checks/deps-direction.py

# 程序图标（换了 crates/hzip-gui/assets/logo/hzip-<N>.png 后必跑，产物入库）
./build/icons/make-icons.ps1

# 右键菜单扩展（需要 Windows SDK + MSBuild；`-Register` 会顺手注册到当前用户）
./build/shell-ext/build-shell-ext.ps1 -NoSign -Publisher "CN=hzip-dev"   # 不签名冒烟（最快）

# 安装包（本地与 CI 同一条命令；产物落 dist/）
python  build/package/package.py --target x86_64-pc-windows-msvc  --formats msi,zip
python3 build/package/package.py --target x86_64-unknown-linux-gnu --formats tar.gz,deb,rpm,appimage
python3 build/package/package.py --target x86_64-apple-darwin,aarch64-apple-darwin --formats dmg,tar.gz
python  build/package/package.py --target <triple> --dry-run     # 只看计划与工具体检
```

## 编译环境依赖

分三档：**A 构建 Rust 产物**、**B 打包安装包**、**C 可选**（shell 扩展/图标/签名）。
CI 里 A 档由 [`.github/actions/rust-build-deps`](../.github/actions/rust-build-deps/action.yml) **一处安装**
（ci / nightly / package 三个工作流都引用它）——改依赖只改那一个文件。

### A. 构建 Rust 产物（编 `hzip` / `hzip-gui`）

| 依赖 | 说明 | Windows | Linux | macOS |
|---|---|---|---|---|
| Rust | `rust-toolchain.toml` pin **1.95.0**（含 rustfmt/clippy） | rustup | rustup | rustup |
| C/C++ 编译器 | 编 xz2 / bzip2 / zstd / unrar 的 C 源码 | VS 2022 Build Tools：**C++ 生成工具 + Windows SDK** | `build-essential` | Xcode CLT |
| `pkg-config` + freetype 头 | 让 `freetype-sys` 找到 `freetype2` | —（MSVC 下走自带构建） | `pkg-config libfreetype-dev` | 自带 |
| `cmake` + `nasm` | **`aws-lc-sys`**（`gpui-kit → reqwest → rustls` 的 TLS 后端）自带构建脚本要求 | 镜像自带 cmake + `choco install nasm` | `cmake nasm perl` | 自带 cmake（arm64 不需要 nasm） |
| 交叉目标 | 出 arm64 / universal 包时 | `rustup target add aarch64-pc-windows-msvc` | arm64 需 arm64 runner（见 04 §6.1） | `rustup target add x86_64-apple-darwin aarch64-apple-darwin` |

Ubuntu/Debian 一行装齐：

```bash
sudo apt-get install -y build-essential pkg-config cmake nasm perl libfreetype-dev
```

> **为什么有 cmake/nasm**：`hzip-gui` 经 gpui-kit 拉进 `reqwest → rustls → aws-lc-rs`，
> 而 `aws-lc-sys` 用 **cmake** 编译自带 C 源码、x86_64 上还要 **nasm** 汇编（缺了在 configure 阶段就失败）。
> `hzip-cli` 不含这条链。要自己核对依赖，用这两条（不需要装交叉 std）：

```bash
cargo tree -p hzip-gui --target x86_64-unknown-linux-gnu -e normal   # Linux 依赖树
cargo tree -p hzip-gui --target x86_64-unknown-linux-gnu -e normal -i aws-lc-sys   # 谁把它拉进来的
```

### B. 打包安装包（按目标平台装；`--dry-run` 会体检缺什么）

| 产物 | 需要 | 装法 |
|---|---|---|
| Windows MSI | **WiX v5**（`wix` 命令）+ **.NET SDK**（`dotnet tool install` 需要 SDK，不是光有运行时） | 见下面「WiX 两步装法」 |
| Windows 签名（可选） | `signtool` | VS 安装器勾「Windows SDK」 |
| Linux deb | `dpkg-deb` | `apt install dpkg-dev`（Debian 系）/ `dnf install dpkg` |
| Linux rpm | `rpmbuild` + `rpm` | `dnf install rpm-build` / `apt install rpm` |
| Linux 元数据校验（建议装） | `desktop-file-validate`、`appstreamcli` | `apt install desktop-file-utils appstream` |
| Linux AppImage | `mksquashfs`（`squashfs-tools`）+ appimagetool/runtime | `apt install squashfs-tools`；**后两者由驱动按版本+sha256 自动下载**到 `~/.cache/hzip-package` |
| macOS dmg | `hdiutil` / `iconutil` / `sips`（Xcode CLT 自带） | `xcode-select --install` |
| macOS 签名+公证（可选） | `codesign` + `xcrun notarytool` + 凭据 | 见 04 §5.3 |
#### WiX 两步装法（用户级，免管理员、免 `sudo`）

```powershell
# ① 用户级 .NET SDK（~200MB，装到 %LOCALAPPDATA%\Microsoft\dotnet；不改机器级）
Invoke-WebRequest https://dot.net/v1/dotnet-install.ps1 -OutFile "$env:TEMP\dotnet-install.ps1"
& "$env:TEMP\dotnet-install.ps1" -Channel 8.0 -InstallDir "$env:LOCALAPPDATA\Microsoft\dotnet" -NoPath

# ② 与 metadata.toml 里 pin 一致的 WiX（装到 %USERPROFILE%\.dotnet\tools\wix.exe）
& "$env:LOCALAPPDATA\Microsoft\dotnet\dotnet.exe" tool install --global wix --version 5.0.2
```

> **驱动怎么挑 wix（不用你手动 `--wix`）**：候选顺序 `--wix` → 环境变量 `HZIP_WIX`
> → `%USERPROFILE%\.dotnet\tools\wix.exe` → PATH，**每个候选都会先跑 `--version` 验证**，
> 跑不起来的直接跳过并记进报错清单。这样避开两个本机踩过的坑：
> ① PATH 上先出现的 scoop `wixtoolset`（framework-dependent，缺 .NET 就起不来、且是 v4 不是 pin 的 v5）；
> ② 刚装完 SDK 的**老终端**里 `DOTNET_ROOT` 还没生效 → 驱动会自动按用户级位置注入
> （新增终端后就不需要这层兜底）。
> 卸载：`Remove-Item -Recurse "$env:LOCALAPPDATA\Microsoft\dotnet"` + 删 `%USERPROFILE%\.dotnet\tools\wix*`
> + 用 `[Environment]::SetEnvironmentVariable` 移除刚加的用户 PATH 项。

| 全部平台 | **Python ≥ 3.8**（只用标准库，无 pip 依赖；Windows 上命令名是 `python`） | 系统自带 / `apt install python3` |

### C. 可选：shell 扩展与图标

| 脚本 | 需要 |
|---|---|
| `build/shell-ext/build-shell-ext.ps1` | MSBuild（VS 2022）+ Windows SDK **≥ 10.0.17763**（MSIX 自该版本起才支持；脚本按**数值版本**挑最新 SDK，也可 `-MakeAppx`/`-SignTool` 显式指定）+（可选）自签证书 |

| `build/icons/make-icons.ps1` | PowerShell 5.1 + System.Drawing（Windows 自带；**不需要** ImageMagick 等） |
| `build/icons/fetch-lucide-icons.sh` | bash + curl（一次性拉 lucide 图标，产物入库） |

## shell 扩展：两种构建模式与自签名证书（Windows）

`build/shell-ext/build-shell-ext.ps1` 产出 `hzip-shell.dll` + `hzip-shellmenu.msix`，分**两种模式**：

| 模式 | 命令 | 产出 | 能干什么 |
|---|---|---|---|
| **不签名**（冒烟） | `… -NoSign -Publisher "CN=hzip-dev"` | **未签名** `.msix` | 只验 DLL/清单/打包链路（免证书、免管理员）；**不能注册、不能分发** |
| **签名** | 见下面两条证书路线 | 已签名 `.msix`（带时间戳） | 可注册稀疏包 → Windows 11 顶层右键菜单（注册需管理员） |

> `-NoSign` 与 `-Register` **互斥**：稀疏包必须由受信任证书签名才能部署，未签名的包 `Add-AppxPackage`
> 必然失败，脚本会直接报错而不是让你撞到一个看不懂的 HRESULT。
> 产物总结里会写清当前状态（`（已签名，证书 …）` / `（**未签名** —— 仅调试用）`）。

### 路线一：单张自签证书（`-CreateTestCert`，最快，仅冒烟）

```powershell
./build/shell-ext/build-shell-ext.ps1 -CreateTestCert -Publisher "CN=hzip-dev"
```

* 脚本自建（或复用）一张 `CN=hzip-dev` 的自签证书放在 `CurrentUser\My`，免管理员；
* **签名会成功** ✓，但**注册仍会 `0x800B0109`** —— AppX 部署服务以 SYSTEM 运行、只看
  `LocalMachine` 证书存储，看不见 `CurrentUser` 里的信任；
* 适用：本地验证打包与签名链路、CI 上没有证书时的冒烟。

### 路线二：开发证书链（本机真机可注册，推荐验收用）

完整说明见 [`shell/cert/README.md`](../shell/cert/README.md)，四步概要：

```powershell
# ① 生成 根 CA → 中间 CA → 叶证书 CN=hzip-dev（免管理员）
./shell/cert/gen-dev-chain.ps1 -Force

# ② 以管理员运行（UAC 同意）：把根/中间 CA 导入 LocalMachine 受信任存储
./shell/cert/install-dev-cert.ps1

# ③ 签名构建 + 落位 + 注册（仍是管理员窗口）
./build/shell-ext/build-shell-ext.ps1 `
    -CertPfx shell/cert/hzip-dev-code.pfx `
    -PasswordFile shell/cert/dev-cert-password.txt `
    -ChainCert shell/cert/hzip-dev-intermediate.cer `
    -InstallDir "<hzip-gui.exe 所在目录>" -Register

# ④ 收工后撤销本机信任（可选）
./shell/cert/uninstall-dev-cert.ps1
```

要点：

* `-Publisher` 省略即可 —— 给了 `-CertPfx` 就自动取证书 Subject；它写进包清单 `Identity/@Publisher`，
  **必须与证书 Subject 逐字一致**，否则注册失败；
* `-ChainCert` 把中间 CA 经 signtool `/ac` 内嵌进签名，链不完整同样 `0x800B0109`；
* **这不是发布证书**：发给用户的包要用受信任 CA 签发的代码签名证书，参数与路线二完全相同
  （`-CertPfx` 指向那个 pfx 即可，无需 `shell/cert`）。

### 注册为什么必须管理员

稀疏包注册由 AppX 部署服务（AppXSvc，**以 SYSTEM 运行**）校验信任链，它**只认 `LocalMachine`
证书存储**；非管理员把证书导入 `CurrentUser\TrustedPeople` 它看不见 → `0x800B0109`。
脚本对这种情况**不再甩原始异常**：给出上面两条处理方法并以**退出码 1** 结束，同时说明
不影响的部分（DLL 与 .msix 已产出、经典注册入口仍可用，菜单出现在「显示更多选项」里）。

### 工具前置

* Windows SDK **≥ 10.0.17763**（MSIX 自该版本起才支持）—— 脚本按**数值版本**挑最新 SDK，
  也可 `-MakeAppx` / `-SignTool` 显式指定；
* `signtool` 与 `makeappx` 必须来自同一套较新 SDK（旧工具签 MSIX 会报
  `SignerSign() failed … 0x80080209`）。

### 故障：`cargo clean` 删不掉 `target\<profile>\hzip-shell.dll`（拒绝访问）

不是安装包的问题：**MSI 把 `hzip-shell.dll` 装进安装目录**（`filShellDll` 落 `INSTALLFOLDER`），
安装版的 `hzip-gui.exe` 注册的也是那份（`validate_spec` 默认取 exe 同目录，`[ui].shell_ext_dll`
可覆盖）。报错删不掉的那份在 `target\<profile>\` 下——它是**开发运行**的落位副本，由 `hzip-gui`
的 `build.rs::stage_shell_ext` 从 `shell/hzip-shell/target/shell-ext/` 拷到 exe 旁边，好让
`cargo run` 出来的 GUI 也能用右键菜单。开发机上点过「应用到系统」后，注册表里的 COM 服务器
路径就指向**这份开发副本**；资源管理器用过一次就把它的映像锁住 → 删除/覆盖都失败。

解锁（任选，按「谁该生效」挑）：

```powershell
# A. 想让**安装版**生效：从容器的 hzip-gui.exe 设置页再点一次「应用到系统」（注册指回安装目录），
#    然后重启资源管理器让旧映像卸载；之后 cargo clean 不再失败
Stop-Process -Name explorer -Force      # 桌面会闪一下，随后自动重启（或 taskkill /f /im explorer.exe & start explorer）

# B. 只想立刻删掉开发副本：先解除注册（GUI 设置页「解除关联/卸载右键菜单」）再重启资源管理器
```

下次 `cargo build` 落位失败时不再静默：`build.rs` 会打 `cargo:warning=`（含上面两条解法）。

### 本机实测状态（2026-09-22，供对照）

* **Windows 本机**：cargo 1.95 / VS Build Tools / cmake+nasm / Python 3.14 齐；
  WiX v5.0.2 已按上面两步装到**用户级**（`%LOCALAPPDATA%\Microsoft\dotnet` + `%USERPROFILE%\.dotnet\tools\wix.exe`）
  → `python build/package/package.py --target x86_64-pc-windows-msvc --formats msi,zip` 可直接产出，无需 `--wix`/`DOTNET_ROOT`。
* **WSL Ubuntu 22.04**：`dpkg-deb`/`rpmbuild`/`appstreamcli`/`desktop-file-validate`/`mksquashfs` 齐，deb/rpm/AppImage
  本地可产出并真装真卸；但**该环境 cargo 拉不到 crates.io 索引**（curl/Python 秒级可达而 cargo 超时，
  与本仓库无关）→ Linux 二进制在本机构建不出，交 CI。

## CI（`.github/workflows`）

| 工作流 | 触发 | 跑什么 |
|---|---|---|
| [`ci.yml`](../.github/workflows/ci.yml) | push / PR / 手动 | `quality`（fmt、`clippy -D warnings`、cargo-deny、`build/checks/*`、工具链↔MSRV 一致性）、`test`（三平台全工作区，**串行**执行）、`package-smoke`（打包相关文件一动就真打一遍包，不发布） |
| [`package.yml`](../.github/workflows/package.yml) | 被 release / ci 调用 | 可复用的「构建 + 打包」作业；矩阵 5 格（见下） |
| [`release.yml`](../.github/workflows/release.yml) | tag `v*` / 手动演练 | 全矩阵打包 → MSI 真装真卸校验 → 建 GitHub Release（汇总各格产物 + 重算全量 `sha256sums.txt`） |
| [`actions/rust-build-deps`](../.github/actions/rust-build-deps/action.yml) | 被引用 | 上面「编译环境依赖」A 档的**唯一实现**（改依赖只改它，ci/package 都引它） |

发布矩阵：Windows x86_64 / arm64（`msi,zip`）· macOS universal（`dmg,tar.gz`）· Linux x86_64 /
arm64（`tar.gz,deb,rpm,appimage`）。**关掉某一格**用仓库变量（`HZIP_ARM64_RUNNER=off` 不做
Linux arm64、`HZIP_WIN_ARM=off` 不做 Windows arm64）；被 `--skip` 的用例、以及各条与早期设计
草稿的差异，逐条写在 workflow 注释与 [04 §6.1](../docs/design/04-编译与部署.md)。

本地复现发版（CI 跑的同一对命令）：

```bash
cargo build --release --target <triple> -p hzip-cli -p hzip-gui
python build/package/package.py --target <triple> --formats msi,zip --check-tag vX.Y.Z
```

## 约定

* 脚本**不写死仓库根**：PowerShell 用 `Split-Path -Parent (Split-Path -Parent $PSScriptRoot)`、
  Python 用「向上找带 `[workspace]` 的 `Cargo.toml`」——再挪目录也不会找错根。
* 脚本只做「打包 / 生成」，**不构建 Rust 产物**（构建交给 `cargo` 与 CI，见 04 §3/§6）。
* 工具链版本一律 pin 在配置里（`package/metadata.toml` 的 WiX / appimagetool / runtime
  版本 + sha256），不依赖「当时最新」。
