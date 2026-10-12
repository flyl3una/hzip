<#
.SYNOPSIS
  撤销 hzip 开发证书链在本机的信任（install-dev-cert.ps1 的逆操作）。

.DESCRIPTION
  从 LocalMachine 的 Root / TrustedPeople / CA 存储中移除本开发链的证书，
  并可选删除 CurrentUser\My 中的开发证书与磁盘上的 pfx/口令文件。

  导入 LocalMachine\Root 意味着本机信任该根签发的任何证书。开发/验收结束后
  应当撤销——这是本脚本存在的主要理由。

.PARAMETER PurgeFiles
  同时删除 shell/cert 下的 pfx、cer 与口令文件（默认只撤销信任，保留文件）。

.EXAMPLE
  ./shell/cert/uninstall-dev-cert.ps1            # 只撤销信任
  ./shell/cert/uninstall-dev-cert.ps1 -PurgeFiles # 连带删除本地文件
#>
[CmdletBinding()]
param(
    [string] $OutDir,

    [switch] $PurgeFiles,
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
    $LogFile = Join-Path $scriptDir ('uninstall-dev-cert.log' -f '$name')
}
Start-Transcript -Path $LogFile -Force | Out-Null

try {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $isAdmin = (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)

    # 先按 Subject 记录指纹（文件可能已被删，故 Subject 匹配为主、文件匹配为辅）
    $rootSubject = 'CN=hzip Development Root CA'
    $interSubject = 'CN=hzip Development Intermediate CA'

    if ($isAdmin) {
        foreach ($pair in @(@('Root', $rootSubject), @('TrustedPeople', $rootSubject),
                            @('CA', $interSubject))) {
            $store = $pair[0]; $subject = $pair[1]
            Get-ChildItem "Cert:\LocalMachine\$store" |
                Where-Object { $_.Subject -eq $subject } |
                ForEach-Object {
                    Write-Host "[cert] 移除 LocalMachine\$store $($_.Subject)"
                    Remove-Item ("Cert:\LocalMachine\$store\" + $_.Thumbprint) -Force
                }
        }
        Write-Host '[cert] LocalMachine 信任已撤销'
    } else {
        Write-Warning '未以管理员运行：LocalMachine 存储无法修改（需管理员）。'
    }

    Get-ChildItem Cert:\CurrentUser\My |
        Where-Object { $_.FriendlyName -like 'hzip-dev*' } |
        ForEach-Object {
            Write-Host "[cert] 移除 CurrentUser\My $($_.FriendlyName)"
            Remove-Item ("Cert:\CurrentUser\My\" + $_.Thumbprint) -Force
        }

    if ($PurgeFiles) {
        Get-ChildItem $OutDir -Include 'hzip-dev-*.cer', 'hzip-dev-*.pfx', 'dev-cert-password.txt' -File |
            ForEach-Object {
                Write-Host "[cert] 删除 $($_.Name)"
                Remove-Item $_.FullName -Force
            }
    } else {
        Write-Host '[cert] 保留磁盘文件（加 -PurgeFiles 可一并删除）'
    }
} finally {
    Stop-Transcript | Out-Null
}
