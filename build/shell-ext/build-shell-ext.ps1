<#
.SYNOPSIS
  构建 hzip 资源管理器右键菜单扩展（Shell DLL + 稀疏包 MSIX）。

.DESCRIPTION
  产出两个文件，放进 hzip-gui.exe 同目录即可被 hzip-platform 的
  context_menu::win 后端使用：

    hzip-shell.dll          IExplorerCommand 实现（经典注册走 InProcServer32；
                            稀疏包走 com:SurrogateServer）
    hzip-shellmenu.msix     稀疏包（Windows 11 / Server 2025 现代菜单顶层入口）

  Windows 11 的顶层菜单**必须有已签名的稀疏包**；没有可注册的签名时只能走经典
  注册（仅出现在「显示更多选项」）。

  包清单 `Identity/@Publisher` 必须与**签名证书的 Subject 逐字一致**。为避免手抄
  出错，`-Publisher` 已改为可选——给了 `-CertPfx` 就从证书自动取 Subject。

  # 证书信任（本机验证的关键）

  MSIX 注册由 AppX 部署服务（SYSTEM）校验信任链，它**只认 LocalMachine 存储**，
  而写 LocalMachine 需要管理员。故本机验证请用 `shell/cert` 下的开发链：

      ./shell/cert/gen-dev-chain.ps1 -Force        # 生成（免管理员）
      # 以管理员运行（UAC 同意）
      ./shell/cert/install-dev-cert.ps1
      ./build/shell-ext/build-shell-ext.ps1 -CertPfx shell/cert/hzip-dev-code.pfx `
          -PasswordFile shell/cert/dev-cert-password.txt `
          -ChainCert shell/cert/hzip-dev-intermediate.cer -InstallDir <目录> -Register

.PARAMETER Publisher
  证书 Subject，写入包清单 `Identity/@Publisher`。**省略时从 `-CertPfx` 自动取**。

.PARAMETER CertPfx
  签名用 .pfx 路径。与 -CreateTestCert 二选一。

.PARAMETER CertPassword
  .pfx 口令（无口令省略）。

.PARAMETER PasswordFile
  从文件读 pfx 口令（`gen-dev-chain.ps1` 产出的 `dev-cert-password.txt`）。
  与 `-CertPassword` 二选一；同时给出时以 `-CertPassword` 为准。

.PARAMETER ChainCert
  中间 CA 证书（`.cer`），经 signtool `/ac` 内嵌进签名。链不完整会导致
  注册报 `0x800B0109`，故用中间 CA 时必须提供。

.PARAMETER NoSign
  **不签名**构建：跳过证书与 signtool，产出【未签名】hzip-shellmenu.msix。
  仅用于调试 DLL/清单、或签名交由 CI 后补的场景 —— 未签名的稀疏包**无法注册**，
  更不能分发（AppX 部署要求签名受信任）。与 -Register 互斥（会直接报错）。

.PARAMETER CreateTestCert
  自建**单张自签**证书（免管理员，只进 CurrentUser 存储）。
  仅作快速冒烟；**它无法让 MSIX 注册成功**（AppXSvc 看不到 CurrentUser 存储），
  本机真机验证请改用上面的 `shell/cert` 开发链。

.PARAMETER InstallDir
  产物落位目录（hzip-gui.exe 所在目录）。省略则只产出到 target/shell-ext。

.PARAMETER Register
  打包后直接 Add-AppxPackage（验证用；等效于设置页点「应用」）。

.EXAMPLE
  # 本地冒烟：不签名（最快；产未签名 .msix，只验 DLL/清单能否产出）
  ./build/shell-ext/build-shell-ext.ps1 -NoSign -Publisher "CN=hzip-dev"

.EXAMPLE
  # 本机真机验证（需管理员，用 shell/cert 开发链，见 build/README.md「自签名证书与签名构建」）
  ./build/shell-ext/build-shell-ext.ps1 -CertPfx shell/cert/hzip-dev-code.pfx `
      -PasswordFile shell/cert/dev-cert-password.txt -InstallDir <目录> -Register

.EXAMPLE
  # 发布：用正式证书签名并落位
  ./build/shell-ext/build-shell-ext.ps1 -Publisher "CN=hzip" -CertPfx .\hzip.pfx -CertPassword *** -InstallDir "C:\Program Files\hzip"
#>
[CmdletBinding()]
param(
    [string] $Publisher,

    [string] $CertPfx,
    [string] $CertPassword,
    [string] $PasswordFile,
    [string] $ChainCert,
    [switch] $CreateTestCert,
    [switch] $NoSign,
    [string] $InstallDir,
    [switch] $Register,
    [string] $Configuration = "release",
    [string] $MakeAppx,
    [string] $SignTool,
    [string] $IdentityName = "hzip.ShellMenu",
    [string] $Version = "0.1.0.0"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)   # build/<子目录>/ → 仓库根
$shellDir = Join-Path $root "shell/hzip-shell"
$appxSrc = Join-Path $shellDir "appx"
$outDir = Join-Path $shellDir "target/shell-ext"

# 稀疏包 CLSID：**必须与 hzip-platform::context_menu::win::CLSID_CLASSIC 区分**，
# 且与 shell/hzip-shell/src/lib.rs::CLSID_SHELL_MENU 一致。
$clsid = "A7C4D1E8-5B93-4F60-9E27-8D3A1B5C7F04"

# 工具所在 SDK 版本：优先从路径里的版本目录取，退而取文件版本号
function Get-SdkVersion([string] $toolPath) {
    if ($toolPath -match '\\10\\bin\\(\d+\.\d+\.\d+\.\d+)\\') { return [version]$Matches[1] }
    $fv = (Get-Item -LiteralPath $toolPath).VersionInfo.FileVersion
    if ($fv -match '^(\d+\.\d+\.\d+)') { return [version]$Matches[1] }
    return [version]'0.0.0'
}

function Get-ToolPath([string] $name, [string] $override, [string] $minSdk) {
    # 选取顺序：显式 -MakeAppx/-SignTool → Windows Kits 里**版本号最大**的 SDK → PATH。
    #
    # [坑 1] 不能 `Sort-Object FullName -Descending`（字符串排序）：`bin\x64\signtool.exe`
    #   （历史遗留副本，本机是 2016 年 th2 版）会排在 `bin\10.0.26100.0\x64\signtool.exe`
    #   前面（'x' > '1'）→ 用旧工具签 MSIX，报 SignerSign() failed (-2146958839/0x80080209)。
    # [坑 2] 不能先查 PATH：本机 PATH 里可能有更老的 SignTool（如 Windows 7 SDK 6.1.7600）。
    # 因此：先按**数值版本**在 SDK 目录里挑最新，PATH 只作兜底，再用 $minSdk 卡下限。
    if ($override) {
        if (-not (Test-Path $override)) { throw "指定的 $name 不存在：$override" }
        return (Resolve-Path $override).Path
    }
    $archDir = switch ($env:PROCESSOR_ARCHITECTURE) { "AMD64" { "x64" } "ARM64" { "arm64" } default { "x64" } }
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits/10/bin"
    $picked = $null
    if (Test-Path $kits) {
        $picked = Get-ChildItem $kits -Directory |
            Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
            Sort-Object { [version]$_.Name } -Descending |
            ForEach-Object { Join-Path $_.FullName "$archDir/$name" } |
            Where-Object { Test-Path $_ } |
            Select-Object -First 1
    }
    if (-not $picked) {
        $cmd = Get-Command $name -ErrorAction SilentlyContinue
        if ($cmd) { $picked = $cmd.Source }
    }
    if (-not $picked) { throw "找不到 $name（需要 Windows SDK，见 build/README.md）" }
    if ($minSdk) {
        $ver = Get-SdkVersion $picked
        if ($ver -lt [version]$minSdk) {
            throw "$name 来自 Windows SDK $ver（$picked），低于 MSIX 所需的 $minSdk —— 请用 Visual Studio 安装器勾选较新的「Windows 10/11 SDK」，或显式传 -MakeAppx/-SignTool 指定工具。"
        }
    }
    return $picked
}

# ---------------------------------------------------------------- 1. 证书

$signThumb = $null
if ($NoSign) {
    if ($Register) {
        throw "-NoSign 与 -Register 互斥：稀疏包必须由**受信任**证书签名才能部署，未签名的包 Add-AppxPackage 必然失败。只做调试/冒烟请去掉 -Register。"
    }
    if (-not $Publisher) {
        $Publisher = 'CN=hzip-dev'
        Write-Warning "未给 -Publisher，占位为 $Publisher（仅本机冒烟；将来签名时 Publisher 必须与证书 Subject 逐字一致）"
    }
    Write-Host '[cert] -NoSign：跳过证书，产出【未签名】hzip-shellmenu.msix（仅调试/冒烟，不可注册、不可分发）'
} elseif ($CreateTestCert) {
    $subject = $Publisher
    $existing = Get-ChildItem Cert:\CurrentUser\My |
        Where-Object { $_.Subject -eq $subject -and $_.NotAfter -gt (Get-Date) } |
        Select-Object -First 1
    if ($existing) {
        $cert = $existing
        Write-Host "[cert] 复用已有自签证书 $($cert.Thumbprint)"
    } else {
        $cert = New-SelfSignedCertificate -Type Custom -Subject $subject `
            -KeyUsage DigitalSignature -FriendlyName "hzip shell ext (dev)" `
            -CertStoreLocation "Cert:\CurrentUser\My" `
            -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
        Write-Host "[cert] 已创建自签证书 $($cert.Thumbprint)"
    }
    # MSIX 注册由 **AppX 部署服务（以 SYSTEM 运行）** 校验签名信任链，
    # 它只认 **LocalMachine** 存储——CurrentUser\TrustedPeople 对它不可见。
    # 实测：只导入 CurrentUser 会得到 0x800B0109（根证书不受信任）。
    $cer = Join-Path $env:TEMP "$IdentityName.cer"
    Export-Certificate -Cert $cert -FilePath $cer -Force | Out-Null
    $isAdmin = ([Security.Principal.WindowsPrincipal] `
        [Security.Principal.WindowsIdentity]::GetCurrent()
    ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if ($isAdmin) {
        Import-Certificate -FilePath $cer -CertStoreLocation "Cert:\LocalMachine\TrustedPeople" | Out-Null
        Write-Host "[cert] 公钥已导入 LocalMachine\TrustedPeople（MSIX 注册需要，需管理员）"
    } else {
        Import-Certificate -FilePath $cer -CertStoreLocation "Cert:\CurrentUser\TrustedPeople" | Out-Null
        Write-Warning @"
未以管理员运行：公钥只导入了 CurrentUser\TrustedPeople，而 AppX 部署服务（SYSTEM）
看不到它 —— 稀疏包注册会以 0x800B0109（根证书不受信任）失败，Windows 11 的新式
右键菜单因此无法生效（只会回落经典注册，仅出现在「显示更多选项」）。

要让 Windows 11 顶层菜单可用，二选一：
  1. 以管理员重跑本脚本（导入 LocalMachine\TrustedPeople）；
  2. 使用受信任 CA 签发的代码签名证书（-CertPfx），其根证书系统已信任。
"@
    }
    Remove-Item $cer -Force
    $signThumb = $cert.Thumbprint
} elseif ($CertPfx) {
    if (-not $CertPassword -and $PasswordFile) {
        if (-not (Test-Path $PasswordFile)) { throw "口令文件不存在：$PasswordFile" }
        $CertPassword = (Get-Content $PasswordFile -Raw).Trim()
    }
    # 注意：Windows PowerShell 5.1 的 `Get-PfxCertificate` **没有 -Password 参数**
    # （实测该 build 只支持 -FilePath/-LiteralPath）。带口令的 pfx 必须经 .NET 加载，
    # 否则会报「找不到与参数名称 Password 匹配的参数」。
    if (-not (Test-Path $CertPfx)) { throw "证书不存在：$CertPfx" }
    $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2(
        (Resolve-Path $CertPfx).Path, $CertPassword)
    if (-not $cert.HasPrivateKey) {
        throw "$CertPfx 不含私钥（无法签名）——请用 gen-dev-chain.ps1 产出的 *.pfx"
    }
    # Publisher 必须与证书 Subject 逐字一致：能给就自动取，避免手抄出错
    if ([string]::IsNullOrWhiteSpace($Publisher)) {
        $Publisher = $cert.Subject
        Write-Host "[cert] Publisher 取自证书 Subject：$Publisher"
    } elseif ($cert.Subject -ne $Publisher) {
        throw "证书 Subject '$($cert.Subject)' 与 -Publisher '$Publisher' 不一致（稀疏包注册会失败）"
    }
    if ($ChainCert -and -not (Test-Path $ChainCert)) {
        throw "中间 CA 证书不存在：$ChainCert"
    }
    $signThumb = $cert.Thumbprint
} else {
    throw '需提供签名证书：-CertPfx（推荐，见 shell/cert/）、-CreateTestCert（仅冒烟）或 -NoSign（完全不签名）。'
}
if (-not $Publisher) {
    throw '-Publisher 无法确定（请给 -CertPfx 或显式指定）'
}

# ---------------------------------------------------------------- 2. 编译 DLL

Write-Host "[build] cargo build（$Configuration；shell/hzip-shell）"
Push-Location $shellDir
try {
    # cargo 无 `--debug` 标志（debug 是默认）；只在 release 时显式传
    if ($Configuration -eq "release") {
        & cargo build --release
    } else {
        & cargo build
    }
    if ($LASTEXITCODE -ne 0) { throw "cargo build 失败（退出码 $LASTEXITCODE）" }
    $dllSrc = Join-Path $shellDir "target/$Configuration/hzip_shell.dll"
    if (-not (Test-Path $dllSrc)) { throw "未产出 $dllSrc" }
    $dll = Join-Path $outDir "hzip-shell.dll"
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
    Copy-Item $dllSrc $dll -Force
} finally {
    Pop-Location
}
Write-Host "[build] $dll"

# ---------------------------------------------------------------- 3. 展开清单

$stage = Join-Path $outDir "appx"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item (Join-Path $appxSrc "Assets") $stage -Recurse
Copy-Item $dll $stage -Force

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { "x64" }
    "ARM64" { "arm64" }
    default { throw "不支持的架构 $env:PROCESSOR_ARCHITECTURE" }
}

$manifest = Get-Content (Join-Path $appxSrc "AppxManifest.xml") -Raw -Encoding UTF8
$manifest = $manifest.Replace("{{IDENTITY_NAME}}", $IdentityName)
$manifest = $manifest.Replace("{{PUBLISHER}}", $Publisher)
$manifest = $manifest.Replace("{{VERSION}}", $Version)
$manifest = $manifest.Replace("{{ARCH}}", $arch)
$manifest = $manifest.Replace("{{CLSID}}", $clsid)
$manifest = $manifest.Replace("{{DLL}}", "hzip-shell.dll")
$manifestPath = Join-Path $stage "AppxManifest.xml"
# UTF-8 无 BOM：MakeAppx 对 BOM 敏感
[System.IO.File]::WriteAllText($manifestPath, $manifest, (New-Object System.Text.UTF8Encoding($false)))

# MSIX 自 Windows SDK 10.0.17763 起才支持（更早的 makeappx/signtool 会打包/签名失败）
$msixMinSdk = '10.0.17763'
$makeappx = Get-ToolPath "makeappx.exe" $MakeAppx $msixMinSdk
$msix = Join-Path $outDir "hzip-shellmenu.msix"
if (Test-Path $msix) { Remove-Item $msix -Force }
Write-Host "[pack] makeappx $((Get-SdkVersion $makeappx)) → $msix"
& $makeappx pack /o /d $stage /nv /p $msix
if ($LASTEXITCODE -ne 0) { throw "makeappx 失败（退出码 $LASTEXITCODE）" }

# ---------------------------------------------------------------- 4. 签名

if ($signThumb) {
    $signtool = Get-ToolPath "signtool.exe" $SignTool $msixMinSdk
    # /ac：把中间 CA 内嵌进签名，否则链不完整 → 注册报 0x800B0109
    $acArgs = if ($ChainCert) { @('/ac', (Resolve-Path $ChainCert).Path) } else { @() }
    Write-Host "[sign] signtool（证书 $signThumb$(if ($ChainCert) { '，链：' + (Split-Path -Leaf $ChainCert) }))"
    & $signtool sign /fd SHA256 /sha1 $signThumb @acArgs /tr http://timestamp.digicert.com /td SHA256 $msix
    if ($LASTEXITCODE -ne 0) {
        Write-Warning '带时间戳签名失败，改用无时间戳签名（离线环境常见）'
        & $signtool sign /fd SHA256 /sha1 $signThumb @acArgs $msix
        if ($LASTEXITCODE -ne 0) { throw "signtool 失败（退出码 $LASTEXITCODE）" }
    }
}

# ---------------------------------------------------------------- 5. 落位

if ($InstallDir) {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item $dll (Join-Path $InstallDir "hzip-shell.dll") -Force
    Copy-Item $msix (Join-Path $InstallDir "hzip-shellmenu.msix") -Force
    Write-Host "[place] 已落位到 $InstallDir"
}

# ---------------------------------------------------------------- 6. 注册（可选）

$registerFailed = $false
if ($Register) {
    $target = if ($InstallDir) { $InstallDir } else { $outDir }
    Write-Host "[register] Add-AppxPackage -ExternalLocation $target"
    try {
        Add-AppxPackage -Path $msix -ExternalLocation $target -ErrorAction Stop
        $ok = Get-AppxPackage $IdentityName
        if (-not $ok) { throw "注册后仍未在仓库中找到 $IdentityName" }
        Write-Host "[register] 成功：$($ok.PackageFullName)"
        Write-Host "[register] 重启资源管理器后即可在右键菜单看到 hzip："
        Write-Host "           Stop-Process -Name explorer -Force"
    } catch {
        # 非管理员 + 自签证书是最常见的组合：公钥只在 CurrentUser\TrustedPeople，
        # 而 AppX 部署服务（SYSTEM）只认 LocalMachine 存储 → 0x800B0109 / 0x80073D02。
        # 与其甩一个原始异常，不如把「怎么才能成」直接讲清楚。
        $registerFailed = $true
        Write-Warning "稀疏包注册失败：$($_.Exception.Message)"
        Write-Warning @"
Windows 11 新式右键菜单需要稀疏包注册成功，失败几乎都是「证书信任」：
  1. 以管理员重跑本脚本（导入 LocalMachine\TrustedPeople 并完成注册）：
       .\build\shell-ext\build-shell-ext.ps1 -Publisher "$Publisher" -Register
  2. 或改用受信任 CA 签发的代码签名证书：-CertPfx .\hzip.pfx
不影响的部分：hzip-shell.dll 与 .msix 已经产出并可分发；经典注册（hzip-platform
回落路径）不受影响，菜单仍会出现在「显示更多选项」里。
"@
    }
}

Write-Host ""
Write-Host "产物：$outDir"
$signState = if ($signThumb) { "（已签名，证书 $signThumb）" } else { '（**未签名** —— 仅调试用；要注册/分发请去掉 -NoSign 改用证书）' }
Write-Host "  hzip-shell.dll / hzip-shellmenu.msix$signState"
if (-not $InstallDir) {
    Write-Host "提示：把这两个文件复制到 hzip-gui.exe 同目录后，在设置页点「应用」。"
}
if ($registerFailed) {
    Write-Host "完成（稀疏包**未注册**：见上面的处理方法；经典菜单入口不受影响）。"
    exit 1
}
Write-Host "完成。"
