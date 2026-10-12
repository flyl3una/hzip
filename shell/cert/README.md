# shell/cert · hzip 开发用代码签名证书链

本目录提供**本机开发/验收**用的代码签名证书链与配套脚本。

Windows 11 的右键「新式菜单」要求稀疏包（MSIX）带**可注册的签名**；而 MSIX 注册由
**AppX 部署服务（AppXSvc，以 SYSTEM 运行）** 校验信任链，它**只认 `LocalMachine`
证书存储**。因此：

- 用 `CurrentUser` 存储自签证书 → 必然 `0x800B0109`（根证书不受信任）；
- 用本目录的开发链 + 管理员导入 `LocalMachine` → 可注册，顶层菜单生效。

> **这不是发布用证书。** 发布请用受信任 CA 签发的代码签名证书（`-CertPfx` 指向它即可，
> 无需本目录）。

---

## 文件

| 文件 | 作用 | 是否入库 |
|---|---|---|
| `gen-dev-chain.ps1` | 生成根 CA → 中间 CA → 叶证书，并导出到本目录 | ✅ 入库（交付物） |
| `install-dev-cert.ps1` | 把根/中间 CA 导入 `LocalMachine` 受信任存储（**需管理员**） | ✅ 入库 |
| `uninstall-dev-cert.ps1` | 撤销本机信任（可选 `-PurgeFiles` 连文件一起删） | ✅ 入库 |
| `hzip-dev-root.cer` / `.pfx` | 根 CA（10 年，CA:TRUE pathlen=1） | ❌ gitignore |
| `hzip-dev-intermediate.cer` / `.pfx` | 中间 CA（CA:TRUE pathlen=0，EKU=Code Signing） | ❌ gitignore |
| `hzip-dev-code.cer` / `.pfx` | 叶证书 `CN=hzip-dev`（EKU=Code Signing）——**签名用** | ❌ gitignore |
| `hzip-dev-chain.cer` | 叶 + 中间 + 根 完整链（人工排查/分发） | ❌ gitignore |
| `dev-cert-password.txt` | pfx 口令（随机生成） | ❌ gitignore |
| `*.log` | 导入/撤销的 transcript | ❌ gitignore |

**私钥与口令绝不入库**（`.gitignore` 已挡 `*.pfx`、`*.cer`、口令与日志）。
`.cer` 是公钥、本可入库，但生成物一律排除以免误提交同目录的 `.pfx`。

> 重新生成会覆盖密钥，务必连同旧的 `LocalMachine` 信任一起撤销后再导入新链
> （`gen-dev-chain.ps1 -Force` 会清理 `CurrentUser\My` 里的同名旧证书，
> 但**动不了** `LocalMachine`，需先 `uninstall-dev-cert.ps1`）。

---

## 链结构

```
CN=hzip-dev                              ← 叶证书（代码签名 EKU，2 年）
  └── CN=hzip Development Intermediate CA  ← 中间 CA（5 年）
        └── CN=hzip Development Root CA    ← 根 CA（10 年，自签）
```

签名时由 `signtool /ac hzip-dev-intermediate.cer` 把中间 CA 内嵌进签名，
根 CA 由 `install-dev-cert.ps1` 放进 `LocalMachine\Root` + `LocalMachine\TrustedPeople`。

**包清单的 `Publisher` 必须逐字等于叶证书 Subject**（`CN=hzip-dev`）。
`build/shell-ext/build-shell-ext.ps1` 会从 `.pfx` 自动取该值，不要手抄。

---

## 用法

```powershell
# 1) 生成链（免管理员）
./shell/cert/gen-dev-chain.ps1 -Force

# 2) 导入本机信任（需管理员；UAC 会弹同意框）
./shell/cert/install-dev-cert.ps1

# 3) 打包 + 签名 + 落位 + 注册
./build/shell-ext/build-shell-ext.ps1 `
    -CertPfx      shell/cert/hzip-dev-code.pfx `
    -PasswordFile shell/cert/dev-cert-password.txt `
    -ChainCert    shell/cert/hzip-dev-intermediate.cer `
    -InstallDir   <hzip-gui.exe 所在目录> `
    -Register

# 4) 重启资源管理器让扩展加载
Stop-Process -Name explorer -Force     # 任务栏会闪一下后自动恢复

# 5) 撤下信任（验收结束后）
./shell/cert/uninstall-dev-cert.ps1 -PurgeFiles
```

`install-dev-cert.ps1` / `uninstall-dev-cert.ps1` 会把过程写进同目录的
`*.log`（提权运行时控制台不可见，故落盘便于查看）。

---

## 验收要点（顶层菜单是否真的出现）

1. `Get-AppxPackage hzip.ShellMenu` 能查到包；
2. 部署清单里含 `windows.fileExplorerContextMenus` 与三个 `ItemType`
   （`*` / `Directory` / `Directory\Background`）：
   `Get-Content (Join-Path (Get-AppxPackage hzip.ShellMenu).InstallLocation 'AppxManifest.xml')`
3. 右键 `.zip` / 文件夹 / 文件夹空白处 → **折叠新式菜单**里直接出现「hzip」子菜单；
   若只出现在「显示更多选项」，说明包未生效（回到第 1、2 步排查）。

平台侧状态可经 `hzip-platform` 的 `context_menu::backend().status()` 读取：
`ModernTopLevel` = 顶层生效；`Degraded` = 仅「显示更多选项」。

---

## 安全说明

- 导入 `LocalMachine\Root` 意味着**本机信任由该根签发的任何证书**。这是可复现的
  本地开发信任，不涉及外部 CA；但**验收结束应撤销**（`uninstall-dev-cert.ps1`）。
- 私钥以明文口令保护、落在本机磁盘。这些密钥仅对本机自签信任有意义；
  若怀疑泄漏，撤销信任并重新生成即可。
- 不要把这些证书用于任何对外分发的产物。
