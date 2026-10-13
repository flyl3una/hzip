# build/package — hzip 多平台安装包

一条命令 = 一个平台的全部安装包。CI（`.github/workflows/package.yml`）与开发机跑的是
**同一个驱动、同一条命令**，所以本地试跑就是发版预演。

| 平台 | 产物 | 打包工具 | 文档依据 |
|---|---|---|---|
| Windows | `hzip-<ver>-setup-<x86_64\|arm64>.msi`（WiX 安装向导）、`hzip-<ver>-<triple>.zip` | WiX v5 | 04 §5.1 |
| macOS | `hzip-<ver>-universal.dmg`（lipo 双架构 .app）、`hzip-<ver>-universal.tar.gz` | hdiutil / iconutil / codesign | 04 §5.3 |
| Linux | `<triple>.tar.gz`、`hzip_<ver>_<amd64\|arm64>.deb`、`hzip-<ver>-1.<arch>.rpm`、`hzip-<ver>-<arch>.AppImage` | dpkg-deb / rpmbuild / appimagetool | 04 §5.2 |

## 用法

```bash
# 先有二进制（驱动不构建，只打包——构建交给 cargo/CI）
cargo build --release -p hzip-cli -p hzip-gui          # 同平台：自动用 target/release
# 交叉/指定目标：
cargo build --release --target aarch64-pc-windows-msvc -p hzip-cli -p hzip-gui

# Windows（在 Windows 上跑）
python build/package/package.py --target x86_64-pc-windows-msvc --formats msi,zip

# macOS（在 macOS 上跑）：universal = 两个目标并列，驱动自动 lipo
python build/package/package.py \
  --target x86_64-apple-darwin,aarch64-apple-darwin --formats dmg,tar.gz

# Linux（在 Linux 上跑）
python3 build/package/package.py \
  --target x86_64-unknown-linux-gnu --formats tar.gz,deb,rpm,appimage

# 只看计划（含缺什么工具），不落地
python3 build/package/package.py --target <triple> --dry-run

# 列出支持的格式/目标
python3 build/package/package.py --list
```

常用参数：`--out-dir`（默认 `dist/`）、`--bin-dir`、`--bin NAME=PATH`（覆盖单个二进制，
自定义构建或本地冒烟用）、`--check-tag vX.Y.Z`（发版断言 tag == `Cargo.toml` 版本）、
`--no-validate`、`--require-shell-ext`、`--sign-*` / `--macos-*`。

## 前置工具

> **编译环境（编 Rust 产物）的依赖在 [`../README.md`](../README.md)「编译环境依赖」**——
> 本节只列**打包**这一步需要的工具。

| 平台 | 必需 | 装法 |
|---|---|---|
| Windows | WiX **v5**（`wix`）+ .NET 运行时 | `dotnet tool install --global wix --version 5.0.2` |
| Windows | shell 扩展载荷（可选） | `build/shell-ext/build-shell-ext.ps1`（需 Windows SDK / MSBuild） |
| Linux | `dpkg-deb`、`rpmbuild`、`rpm` | Debian/Ubuntu：`apt install dpkg-dev rpm`；Fedora：`dnf install rpm-build` |
| Linux | 打包前元数据校验 | `apt install appstream desktop-file-utils`（缺失只告警，`--no-validate` 可跳过） |
| Linux | appimagetool | **驱动自动下载**（版本+sha256 锁在 `metadata.toml`，缓存 `~/.cache/hzip-package`） |
| macOS | `hdiutil`/`iconutil`/`sips` | 系统自带（Xcode CLT） |
| macOS | 签名与公证（可选） | `codesign` + `xcrun notarytool`（04 §5.3） |

Python ≥ 3.8（标准库即可，不需要 pip 包）。CI 上 `ubuntu-latest` / `macos-latest` /
`windows-latest` 都自带 python3；Windows 上命令名是 `python` 不是 `python3`。

> **打包前先编译**：`cargo build --release`。macOS 首次构建会编译 `vendor/gpui-pre-macos`
> （见仓库根 `Cargo.toml` 的 `[patch.crates-io]`：修触控栏机型关窗崩溃，上游 zed#65186
> 未合并），比纯 registry 依赖慢一次；之后走增量。

> **`Info.plist` 的本地化声明不是装饰**：`templates/macos/Info.plist.in` 里的
> `CFBundleLocalizations`/`CFBundleDevelopmentRegion`（zh-Hans/en）是浏览页「类型」列
> 用**系统类型名**（`UTType.localizedDescription`）的**功能前置**。该 API 的语言由
> 进程 bundle 的本地化集合决定、而非系统语言：缺了这段，同一台 zh_CN 机器上也只能
> 拿到英文 `"Zip archive"`，`hzip-platform` 的门控会如实拒绝采用，该列在 macOS 上
> 恒回退自有文案（详见 02 §5.3.11）。因此**不要**删这两项。

## 配置与模板

* `metadata.toml` —— **打包元数据单一来源**：维护者、主页、SPDX、描述、deb/rpm 依赖、
  AppImage 工具与 runtime 的 URL+sha256、WiX 版本与 UpgradeCode。版本号**不在这里**
  （唯一来源是根 `Cargo.toml` 的 `[workspace.package].version`，02 §2.2）。
  文件里的仓库地址与 maintainer 已换成真实值（`https://github.com/flyl3una/hzip` /
  `flyl3una <l3una@outlook.com>`）；驱动仍会扫描 `<org>`/`example.com`，用于拦截将来
  新引入的占位符。
  许可同为**单一事实源**：根 `LICENSE`（GPL-3.0 全文）+ 本文件 `[package] spdx`；
  驱动据此把许可全文自动装进各产物，不在这里另抄正文。
* `templates/` —— 各格式的清单/脚本模板：
  `windows/hzip.wxs.in`（MSI 定义，WiX `-d` 变量传参）、
  `linux/{hzip.desktop.in,hzip.metainfo.xml.in,hzip.spec.in,deb-control.in,deb-postinst.in,deb-postrm.in,AppRun.in}`、
  `macos/{Info.plist.in,dmg-readme.txt}`、`portable/README.txt.in`。
  占位符是 `{{UPPER_SNAKE}}`，漏填会**报错**（不会静默产出空字段）。

### 依赖推导（deb/rpm 的 Depends/Requires）

二进制静态链接了 C 库（xz/bzip2/zstd/unrar），动态依赖只剩基础运行库 + GUI 的
X11/Wayland 栈。改动依赖树后重新推导：

```bash
# ① 动态库 soname（真实 Linux 产物；交叉架构的产物在 CI 里跑）
ldd target/x86_64-unknown-linux-gnu/release/hzip-gui | awk '{print $1}' | sort -u
# ② 依赖树里谁需要系统库（-sys 包 / pkg-config / dlopen），不用装交叉 std 也能查：
cargo tree -p hzip-gui --target x86_64-unknown-linux-gnu -e normal
cargo tree -p hzip-gui --target x86_64-unknown-linux-gnu -e normal -i aws-lc-sys
# 再把 soname 映射成包名（dpkg -S / apt-file search），更新 metadata.toml 的 [deb]/[rpm]
```

2026-09-22 的推导结论（已写进 `metadata.toml` 注释）：C 库（xz/bzip2/zstd/unrar）静态自编，
动态只剩 glibc/libgcc + GUI 图形栈（`libvulkan1` 由 wgpu/ash dlopen、`libfreetype6` 由
`freetype-sys` 动态链、`libxkbcommon0`/`libwayland-*` 保守声明）。

**2026-10-12 补**：`libfontconfig1` 也在这条链上——`yeslogic-fontconfig-sys`（gpui 的字体栈，
经 `zed-font-kit`）**无条件** pkg-config 探测 `fontconfig`，只有设 `RUST_FONTCONFIG_DLOPEN`
才改走 dlopen（CI 未设）⇒ 产物 DT_NEEDED 里有 `libfontconfig.so.1`，`[deb]/[rpm]` 必须声明它。
暴露路径：CI 的 Linux 腿因缺 `fontconfig.pc` 在 build script 里 panic（exit 101）。CI 现在会把
Linux 腿的 `ldd` 打进日志（`package.yml`），换依赖树后照着它核清单，别靠记忆。

### 弱依赖（Recommends）

`[deb] recommends` / `[rpm] recommends` **不是**从 `ldd` 推出来的，是产品语义声明：

- `libegl1, libgles2, mesa-vulkan-drivers`：wgpu 的 GLES/Vulkan 回落路径；
- 中文字体（deb `fonts-noto-cjk` / rpm `google-noto-sans-cjk-fonts`）：产品文案默认跟随系统
  语言，而包内**不**内置字体——机器上一款中文字体都没有时，GUI 会把界面回退英文（02 §12.2）。
  弱依赖让 apt/dnf 默认装上它，`--no-install-recommends` 的最小安装仍可用（英文界面）。

rpm 的 `Recommends:` 行由 `package.py` **整行**成形（占位符留空会产出一个空的字段行 = 非法
spec），模板里只留 `{{RPM_RECOMMENDS}}`。

### AppImage 工具升级

`metadata.toml` 的 `appimagetool_*` / `runtime_*` 是 **URL + sha256 双锁**（滚动 tag 的
发布物也会变，校验失败即中止，不会静默用上被替换的文件）。升级步骤：改 URL 里的版本 →
跑一次打包拿到实际 sha256（报错信息里就有）→ 回填 metadata。

## 设计要点

* **许可（GPL-3.0-or-later）**：根 `LICENSE` 是唯一正文来源，`metadata.toml` 的 `spdx`
  是唯一 SPDX 标识来源。驱动在打包时把根 `LICENSE*/COPYING*` 装进**每一个**产物：
  deb/rpm/AppImage → `usr/share/doc/hzip/`、portable `tar.gz`/`zip` → 包根、
  macOS `.app` → `Contents/Resources/`、Windows MSI → 安装目录（`INSTALLFOLDER`）。
  即「随二进制附许可全文」的 GPL 义务；**GPL 还要求提供对应源码**——源码即本仓库
  （`homepage`/`issues_url`），请保证发布时源码可获取。缺根 `LICENSE` 时驱动只**警告**，
  会产出不含许可文本的包，**不要**这样发版。
* **产物命名**跟随 04 §5；`msi` 的别名 `setup` 指同一个东西（文档里那个「setup 安装包」就是 MSI）。
* **Windows MSI**：装完在系统里**注册为 `hzip`**——`RegisteredApplications` + `Capabilities`
  （「默认应用」候选，逐扩展名指向 `hzip.Archive`）、开始菜单/桌面快捷方式与 ARP 条目都叫 `hzip`；
  `hzip.exe` 进系统 PATH + `App Paths`，命令行直接可用。一机一份装到 Program Files；
  `MajorUpgrade` 升级保配置（配置在
  `%APPDATA%\hzip`，MSI 从不触碰）；卸载精确回收自己写的文件/快捷方式/PATH/注册表值；
  用户数据默认保留，`msiexec /x <msi> REMOVEUSERDATA=1` 才清（04 §8）。
  右键菜单**不由 MSI 注册**——经典注册与稀疏包都由应用运行时按 HKCU 写（04 §5.1），
  MSI 只落 `hzip-shell.dll` / `hzip-shellmenu.msix`。
  组件 GUID 是固定字面量（扩展名关联的 GUID 由 `uuid5(固定命名空间, "hzip.installer.ext.<ext>")`
  派生），**发布后不得改动**。
* **macOS**：`.app` 的 `CFBundleDocumentTypes` 只登记 Viewer + `LSHandlerRank=Alternate`
  （不抢系统默认程序）；dmg 里放 `Applications` 软链 + 安装说明；签名/公证按 04 §5.3
  （不给凭据就产 unsigned，**不会**假装签过）。
* **Linux**：`.desktop` / AppStream 元数据在打包时**先校验再进包**；`.desktop` 名、
  AppStream `<id>`、图标名三者都是 `metadata.toml` 的 `app_id`（GNOME 按 app-id 找图标）。
* **失败即停**：工具缺失、模板占位符漏填、校验失败、产物缺文件都是硬错误。

## 验收锚点

```text
# 1) Windows：MSI 结构（不装也能查）——Registry/Shortcut/Environment/File/Feature 表
#    msiexec /a <msi> TARGETDIR=<tmp> /qn   → hzip.exe / hzip-gui.exe 落到 <tmp>\hzip
#    （真正的安装/卸载校验跑在 CI：.github/workflows/release.yml 的 verify-msi job）
# 2) Linux：dpkg-deb --info/--contents、rpm -qp --qf '%{NAME} %{VERSION} %{ARCH} %{SIZE}'、
#    AppImage --appimage-extract 后 squashfs-root/usr/bin/{hzip,hzip-gui} 存在（驱动内建自证）
# 2b) 许可：每个产物都含根 LICENSE 全文——deb/rpm/AppImage: usr/share/doc/hzip/LICENSE；
#     tar.gz/zip: 包根 LICENSE；dmg: hzip.app/Contents/Resources/LICENSE；msi: 安装目录 LICENSE
# 2c) macOS：universal 双架构由驱动内建自证（对 lipo 后的 hzip / hzip-gui 跑
#     `lipo -archs` 核对架构集合——**不要**用 `-verify_arch <arch…> <file>`，那是
#     Xcode 26 会拒绝的参数顺序）；dmg 内为 hzip.app + Applications 软链 + README.txt
# 3) 全平台：dist/sha256sums.txt 覆盖每个产物（04 §5.4）
```
