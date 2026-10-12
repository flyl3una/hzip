<#
.SYNOPSIS
  把 hzip 开发证书链导入**本机**受信任存储，使 MSIX 稀疏包可注册（Windows 11 顶层右键菜单的前提）。

.DESCRIPTION
  # 为什么必须导入 LocalMachine（而不是 CurrentUser）

  MSIX 注册由 **AppX 部署服务（AppXSvc，以 SYSTEM 运行）** 校验签名信任链。
  SYSTEM 看不到 `CurrentUser\*` 证书存储——实测只导入 CurrentUser 会得到
  `0x800B0109`（根证书不受信任）。故必须写 `LocalMachine\*`，**需要管理员权限**。

  # 导入内容

      根 CA     → LocalMachine\Root（让它成为受信任链根）
                → LocalMachine\TrustedPeople（MSIX 显式信任锚，双保险）
      中间 CA   → LocalMachine\CA（受信任的中间证书颁发机构）
      叶证书    → 不导入（签名时由 signtool /ac 内嵌进签名）

  # 安全性

  这些是**本机自建**的开发根，只对本次开发有意义。导入 `LocalMachine\Root` 意味着
  本机将信任由该根签发的任何证书——这是可复现的本地开发信任，不是把生产系统
  暴露给外部 CA。用完请跑 `uninstall-dev-cert.ps1` 撤销。

.PARAMETER OutDir
  证书目录（默认脚本同级）。

.PARAMETER LogFile
  日志落盘路径（提权运行时控制台不可见，故写文件便于外部读取）。

.EXAMPLE
  # 需在**管理员** PowerShell 中执行
  ./shell/cert/install-dev-cert.ps1
#>
[CmdletBinding()]
param(
    [string] $OutDir,

    [string] $LogFile
)

$ErrorActionPreference = 'Stop'

# `$PSScriptRoot` 在 param 默认值求值期不可用（PS 5.1），故在脚本体内解析默认路径
$scriptDir = $PSScriptRoot
if ([string]::IsNullOrEmpty($scriptDir)) {
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
}
if ([string]::IsNullOrEmpty($OutDir)) { $OutDir = $scriptDir }
if ([string]::IsNullOrEmpty($LogFile)) {
    $LogFile = Join-Path $scriptDir ('install-dev-cert.log' -f '$name')
}
Start-Transcript -Path $LogFile -Force | Out-Null

try {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($id)
    $isAdmin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    Write-Host "[cert] 当前身份 = $($id.Name)，IsAdmin = $isAdmin"
    if (-not $isAdmin) {
        throw '需要管理员权限（LocalMachine 证书存储）。请以管理员身份重新运行。'
    }

    $rootCer = Join-Path $OutDir 'hzip-dev-root.cer'
    $interCer = Join-Path $OutDir 'hzip-dev-intermediate.cer'
    foreach ($f in @($rootCer, $interCer)) {
        if (-not (Test-Path $f)) {
            throw "缺少 $f —— 请先运行 gen-dev-chain.ps1"
        }
    }

    # 幂等：先移除同 Subject 的旧副本，避免重复堆积
    $root = Get-PfxCertificate -FilePath $rootCer
    foreach ($store in @('Root', 'TrustedPeople')) {
        Get-ChildItem "Cert:\LocalMachine\$store" |
            Where-Object { $_.Thumbprint -eq $root.Thumbprint } |
            ForEach-Object {
                Write-Host "[cert] 移除 LocalMachine\$store 旧副本 $($_.Thumbprint)"
                Remove-Item ("Cert:\LocalMachine\$store\" + $_.Thumbprint) -Force
            }
    }
    Get-ChildItem 'Cert:\LocalMachine\CA' |
        Where-Object { $_.Subject -eq 'CN=hzip Development Intermediate CA' } |
        ForEach-Object {
            Write-Host "[cert] 移除 LocalMachine\CA 旧副本 $($_.Thumbprint)"
            Remove-Item ("Cert:\LocalMachine\CA\" + $_.Thumbprint) -Force
        }

    Write-Host '[cert] 导入根 CA → LocalMachine\Root'
    Import-Certificate -FilePath $rootCer -CertStoreLocation 'Cert:\LocalMachine\Root' | Out-Null
    Write-Host '[cert] 导入根 CA → LocalMachine\TrustedPeople'
    Import-Certificate -FilePath $rootCer -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople' | Out-Null
    Write-Host '[cert] 导入中间 CA → LocalMachine\CA'
    Import-Certificate -FilePath $interCer -CertStoreLocation 'Cert:\LocalMachine\CA' | Out-Null

    Write-Host ''
    Write-Host '--- 校验 ---'
    Write-Host ("根 Thumbprint   = " + $root.Thumbprint)
    $inter = Get-PfxCertificate -FilePath $interCer
    Write-Host ("中间 Thumbprint = " + $inter.Thumbprint)

    # 链构建自检：叶证书应能一路验到受信任根
    $codeCer = Join-Path $OutDir 'hzip-dev-code.cer'
    if (Test-Path $codeCer) {
        $code = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($codeCer)
        $chain = New-Object System.Security.Cryptography.X509Certificates.X509Chain
        $chain.ChainPolicy.RevocationMode = 'NoCheck'
        $ok = $chain.Build($code)
        Write-Host "[cert] 叶证书链构建 = $ok"
        foreach ($el in $chain.ChainElements) {
            Write-Host ("        - " + $el.Certificate.Subject)
        }
        foreach ($st in $chain.ChainStatus) {
            Write-Host ("        status: " + $st.Status + " " + $st.StatusInformation)
        }
        if (-not $ok) {
            Write-Warning '链未构建成功——MSIX 注册可能仍报 0x800B0109'
        }
    }

    Write-Host ''
    Write-Host '完成。现在可用此证书签名并注册稀疏包：'
    Write-Host '  ./build/shell-ext/build-shell-ext.ps1 -CertPfx shell/cert/hzip-dev-code.pfx ``'
    Write-Host '      -ChainCert shell/cert/hzip-dev-intermediate.cer -InstallDir <目标目录> -Register'
} catch {
    Write-Host "[cert] 失败：$($_.Exception.Message)"
    throw
} finally {
    Stop-Transcript | Out-Null
}
