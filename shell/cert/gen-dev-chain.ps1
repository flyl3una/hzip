<#
.SYNOPSIS
  生成 hzip 开发用代码签名证书链（根 CA → 中间 CA → 代码签名叶证书）。

.DESCRIPTION
  仅供**本机开发/验收**使用，不可用于发布（自签根在用户机器上不受信任）。

  生成结构与真实代码签名链一致，用于验证 MSIX 稀疏包从签名到注册的完整链路：

      根 CA（自签，CA:TRUE pathlen=1）
        └── 中间 CA（CA:TRUE pathlen=0，EKU=Code Signing）
              └── 叶证书  CN=hzip-dev（EKU=Code Signing）   ← 包清单 Publisher 必须等于此 Subject

  产出（默认落 `shell/cert/`）：

      hzip-dev-root.cer / .pfx          根 CA 证书 / 含私钥
      hzip-dev-intermediate.cer / .pfx  中间 CA
      hzip-dev-code.cer / .pfx          叶证书（签名用）
      hzip-dev-chain.cer                叶 → 中间 → 根 的完整链（拼接，便于分发/排查）
      dev-cert-password.txt             pfx 口令（随机生成；**已 gitignore**）

  **私钥安全**：`*.pfx` 与口令文件均在本机生成，且已被 `shell/cert/.gitignore`
  排除出版本控制。这些密钥只对本次开发有意义；万一泄漏，影响面仅限本机自签信任，
  仍建议用完后 `uninstall-dev-cert.ps1` 清除信任并删除 pfx。

.PARAMETER OutDir
  产出目录；默认脚本同级的 `shell/cert`。

.PARAMETER Force
  覆盖已存在的产出（同时清理上次留在 CurrentUser\My 里的同名开发证书）。

.PARAMETER ValidityYears
  根 CA 有效期（年，默认 10）；中间 CA 为其一半，叶证书 2 年。

.EXAMPLE
  ./shell/cert/gen-dev-chain.ps1 -Force
#>
[CmdletBinding()]
param(
    [string] $OutDir,

    [switch] $Force,
    [int] $ValidityYears = 10
)

$ErrorActionPreference = 'Stop'

# `$PSScriptRoot` 在 param 默认值求值期不可用（PS 5.1），故在脚本体内解析默认路径
$scriptDir = $PSScriptRoot
if ([string]::IsNullOrEmpty($scriptDir)) {
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
}
if ([string]::IsNullOrEmpty($OutDir)) { $OutDir = $scriptDir }
if ([string]::IsNullOrEmpty($LogFile)) {
    $LogFile = Join-Path $scriptDir ('gen-dev-cert.log' -f '$name')
}

$rootSubject = 'CN=hzip Development Root CA'
$interSubject = 'CN=hzip Development Intermediate CA'
$leafSubject = 'CN=hzip-dev'
$friendlyTag = 'hzip-dev'

$EKU_CODE_SIGNING = '2.5.29.37={text}1.3.6.1.5.5.7.3.3'
$BC_CA_ROOT = '2.5.29.19={critical}{text}ca=1&pathlength=1'
$BC_CA_INTER = '2.5.29.19={critical}{text}ca=1&pathlength=0'
$BC_LEAF = '2.5.29.19={critical}{text}ca=0'

function Assert-AdminOptional { }

# ---------------------------------------------------------------- 前置检查

if (-not (Test-Path $OutDir)) {
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
}
$OutDir = (Resolve-Path $OutDir).Path

$existing = @(
    'hzip-dev-root.cer', 'hzip-dev-root.pfx',
    'hzip-dev-intermediate.cer', 'hzip-dev-intermediate.pfx',
    'hzip-dev-code.cer', 'hzip-dev-code.pfx',
    'hzip-dev-chain.cer', 'dev-cert-password.txt'
) | ForEach-Object { Join-Path $OutDir $_ } | Where-Object { Test-Path $_ }

if ($existing.Count -gt 0 -and -not $Force) {
    throw "产出已存在（加 -Force 覆盖）：`n  $($existing -join "`n  ")"
}

# 清掉上次生成留在用户证书库里的同名开发证书，避免越堆越多
Get-ChildItem Cert:\CurrentUser\My |
    Where-Object { $_.FriendlyName -like "$friendlyTag*" } |
    ForEach-Object {
        Write-Host "[gen] 移除旧开发证书 $($_.Thumbprint) [$($_.FriendlyName)]"
        Remove-Item ("Cert:\CurrentUser\My\" + $_.Thumbprint) -Force
    }

# ---------------------------------------------------------------- 口令

$bytes = New-Object byte[] 24
[System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
$passwordPlain = [Convert]::ToBase64String($bytes)
$password = ConvertTo-SecureString $passwordPlain -AsPlainText -Force
$passwordFile = Join-Path $OutDir 'dev-cert-password.txt'
[System.IO.File]::WriteAllText($passwordFile, $passwordPlain, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "[gen] pfx 口令已写入 $passwordFile（已 gitignore）"

# ---------------------------------------------------------------- 1. 根 CA

Write-Host "[gen] 根 CA：$rootSubject"
$root = New-SelfSignedCertificate `
    -Type Custom -Subject $rootSubject `
    -KeyAlgorithm RSA -KeyLength 4096 -HashAlgorithm SHA256 `
    -KeyUsage CertSign, CRLSign, DigitalSignature `
    -KeyExportPolicy Exportable `
    -NotAfter (Get-Date).AddYears($ValidityYears) `
    -CertStoreLocation 'Cert:\CurrentUser\My' `
    -TextExtension @($BC_CA_ROOT) `
    -FriendlyName "$friendlyTag-root"

# ---------------------------------------------------------------- 2. 中间 CA

Write-Host "[gen] 中间 CA：$interSubject"
$inter = New-SelfSignedCertificate `
    -Type Custom -Subject $interSubject `
    -Signer $root `
    -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 `
    -KeyUsage CertSign, CRLSign `
    -KeyExportPolicy Exportable `
    -NotAfter (Get-Date).AddYears([Math]::Max(1, [int]($ValidityYears / 2))) `
    -CertStoreLocation 'Cert:\CurrentUser\My' `
    -TextExtension @($BC_CA_INTER, $EKU_CODE_SIGNING) `
    -FriendlyName "$friendlyTag-intermediate"

# ---------------------------------------------------------------- 3. 叶证书

Write-Host "[gen] 叶证书：$leafSubject"
$leaf = New-SelfSignedCertificate `
    -Type Custom -Subject $leafSubject `
    -Signer $inter `
    -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 `
    -KeyUsage DigitalSignature `
    -KeyExportPolicy Exportable `
    -NotAfter (Get-Date).AddYears(2) `
    -CertStoreLocation 'Cert:\CurrentUser\My' `
    -TextExtension @($BC_LEAF, $EKU_CODE_SIGNING) `
    -FriendlyName "$friendlyTag-code"

# ---------------------------------------------------------------- 4. 导出

function Export-Pair([System.Security.Cryptography.X509Certificates.X509Certificate2] $cert, [string] $base) {
    $cer = Join-Path $OutDir "$base.cer"
    $pfx = Join-Path $OutDir "$base.pfx"
    Export-Certificate -Cert $cert -FilePath $cer -Type CERT -Force | Out-Null
    Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $password -Force | Out-Null
    Write-Host "[gen] $base.cer / .pfx"
}

Export-Pair $root 'hzip-dev-root'
Export-Pair $inter 'hzip-dev-intermediate'
Export-Pair $leaf 'hzip-dev-code'

# 完整链（叶 → 中间 → 根）：Base64 拼接，供分发与人工排查
function ConvertTo-Base64Cert([System.Security.Cryptography.X509Certificates.X509Certificate2] $c) {
    $b64 = [Convert]::ToBase64String($c.RawData, [Base64FormattingOptions]::InsertLineBreaks)
    "# $($c.Subject)`n-----BEGIN CERTIFICATE-----`n$b64`n-----END CERTIFICATE-----`n"
}
$chain = (ConvertTo-Base64Cert $leaf) + (ConvertTo-Base64Cert $inter) + (ConvertTo-Base64Cert $root)
$chainFile = Join-Path $OutDir 'hzip-dev-chain.cer'
[System.IO.File]::WriteAllText($chainFile, $chain, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "[gen] hzip-dev-chain.cer（叶 + 中间 + 根）"

# ---------------------------------------------------------------- 5. 自检

Write-Host ''
Write-Host '--- 生成结果 ---'
Write-Host ("根   Subject = " + $root.Subject + "  Thumbprint = " + $root.Thumbprint)
Write-Host ("中间 Subject = " + $inter.Subject + "  Thumbprint = " + $inter.Thumbprint)
Write-Host ("叶   Subject = " + $leaf.Subject + "  Thumbprint = " + $leaf.Thumbprint)
Write-Host ''
Write-Host "包清单 Publisher 必须逐字等于：$($leaf.Subject)"
Write-Host "（build-shell-ext.ps1 会从 pfx 自动取该值，无需手抄）"
Write-Host ''
Write-Host '下一步（需管理员，UAC 会弹同意框）：'
Write-Host "  powershell -ExecutionPolicy Bypass -File `"$OutDir\install-dev-cert.ps1`""
