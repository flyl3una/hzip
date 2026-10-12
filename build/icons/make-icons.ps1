<#
.SYNOPSIS
  生成 hzip 的程序图标产物（多尺寸 ICO + 稀疏包磁贴）。

.DESCRIPTION
  源：`crates/hzip-gui/assets/logo/hzip-<N>.png` —— 逐尺寸手工图
  （N = 16/20/24/32/40/48/64/128/256）。程序图标属于 GUI crate（与
  `crates/hzip-gui/assets/icons/*.svg` 同层），故源与产物都在该目录下。
  产物：

    crates/hzip-gui/assets/logo/hzip.ico   → 可执行文件图标资源
                                             （`crates/hzip-gui/hzip.rc` /
                                              `crates/hzip-cli/hzip.rc` 内嵌；
                                              资源 ID 必须为 1——见 hzip-gui/build.rs）
    shell/hzip-shell/appx/Assets/*.png     → 右键菜单稀疏包磁贴（AppxManifest 引用）

  # 每档尺寸的解析规则（逐档独立判定）

  1. 有 `hzip-<N>.png` → **原样取用**（不缩放、不加边距、不裁切）；
  2. 否则若给了 `-Source`（基准大图）→ 由它缩放；
  3. 否则 → 由**最近的更大档**逐尺寸图缩放（宁缩不放）；
     所有档位都小于目标 → 取最大档放大，并告警。

  为什么要逐尺寸图：小尺寸直接缩小大图会糊——字形变细、像素对不齐。设计者按尺寸
  调过的小图（加粗、去细节、对齐像素网格）清晰得多，所以第 1 条优先。可以只给
  一部分尺寸，缺的档按第 2/3 条补齐（脚本会逐档打印来源，便于核对）。

  # ICO 条目格式

  尺寸档 16/20/24/32/40/48/64/128/256，按尺寸分两种条目：

  - ≤64：**DIB（BMP）条目**。资源管理器小图标、任务栏、Alt-Tab 实际取用的就是
    这几档，DIB 兼容性最广（老 shell 组件对 PNG 条目支持不全）。
  - ≥128：**PNG 条目**。Vista 起 ICO 允许 PNG，省掉 256² 未压缩条目（约 256KB）。

  AND 掩码由 alpha 推导（位=1 表示透明）：不认 alpha 的旧渲染路径不会把透明区
  画成黑块。

.PARAMETER PerSizeDir
  逐尺寸图目录（默认 `crates/hzip-gui/assets/logo`；按 `hzip-<N>.png` 命名查找）。
  尺寸不符（非 N×N）即报错——静默缩放会掩盖出图错误。

.PARAMETER IcoPath
  ICO 输出路径（默认 `crates/hzip-gui/assets/logo/hzip.ico`）。

.PARAMETER Source
  可选基准大图（正方形、边长 ≥ 256）。仅用于**没有逐尺寸图**的档位。

.PARAMETER AppxAssetDir
  稀疏包磁贴目录（默认 `shell/hzip-shell/appx/Assets`）。

.EXAMPLE
  ./build/icons/make-icons.ps1
#>
[CmdletBinding()]
param(
    [string]$PerSizeDir,
    [string]$IcoPath,
    [string]$Source,
    [string]$AppxAssetDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

# 默认路径在**正文**里解析：PS 5.1 的 param() 默认值求值期 $PSScriptRoot 为空
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)   # build/<子目录>/ → 仓库根
if (-not $PerSizeDir) { $PerSizeDir = Join-Path $root 'crates\hzip-gui\assets\logo' }
if (-not $IcoPath) { $IcoPath = Join-Path $PerSizeDir 'hzip.ico' }
if (-not $AppxAssetDir) { $AppxAssetDir = Join-Path $root 'shell\hzip-shell\appx\Assets' }

# ICO 条目尺寸档；≥ $PngThreshold 的档位用 PNG 条目（其余 DIB）
$Sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$PngThreshold = 128

# 稀疏包磁贴（文件名 → 边长；与 AppxManifest 的 Square150x150Logo/Square44x44Logo 对应）
$Tiles = [ordered]@{
    'Square44x44Logo.png'   = 44
    'Square150x150Logo.png' = 150
    'StoreLogo.png'         = 50
}

function New-ScaledBitmap {
    param([System.Drawing.Image]$Src, [int]$Size)
    $bmp = New-Object System.Drawing.Bitmap $Size, $Size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    try {
        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
        $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $g.Clear([System.Drawing.Color]::Transparent)
        $dst = New-Object System.Drawing.Rectangle 0, 0, $Size, $Size
        $g.DrawImage($Src, $dst, 0, 0, $Src.Width, $Src.Height, [System.Drawing.GraphicsUnit]::Pixel)
    }
    finally { $g.Dispose() }
    return $bmp
}

function New-ExactBitmap {
    param([string]$Path, [int]$Size)
    $im = [System.Drawing.Image]::FromFile($Path)
    try {
        # Clone 到 32bppArgb：格式归一（DIB 条目路径要求），像素**不重采样**
        $rect = New-Object System.Drawing.Rectangle 0, 0, $Size, $Size
        return $im.Clone($rect, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    }
    finally { $im.Dispose() }
}

function Get-DibEntryBytes {
    param([System.Drawing.Bitmap]$Bmp)
    # ICO 的 DIB 条目 = BITMAPINFOHEADER（高度写 2×）+ XOR 位图（BGRA 自下而上）+ AND 掩码
    $size = $Bmp.Width
    $rect = New-Object System.Drawing.Rectangle 0, 0, $size, $size
    $data = $Bmp.LockBits($rect,
        [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $stride = $data.Stride
        $pixels = New-Object byte[] ($stride * $size)
        [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $pixels, 0, $pixels.Length)
    }
    finally { $Bmp.UnlockBits($data) }

    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter $ms
    try {
        $bw.Write([uint32]40)                 # biSize
        $bw.Write([int32]$size)               # biWidth
        $bw.Write([int32]($size * 2))         # biHeight（ICO 约定：含掩码，写 2 倍）
        $bw.Write([uint16]1)                  # biPlanes
        $bw.Write([uint16]32)                 # biBitCount
        $bw.Write([uint32]0)                  # biCompression = BI_RGB
        $bw.Write([uint32]($size * $size * 4))# biSizeImage
        $bw.Write([int32]0); $bw.Write([int32]0)   # 分辨率（像素/米，0 = 未指定）
        $bw.Write([uint32]0); $bw.Write([uint32]0) # 调色板
        for ($y = $size - 1; $y -ge 0; $y--) { $bw.Write($pixels, $y * $stride, $size * 4) }
        # AND 掩码：1bpp，行按 4 字节对齐，自下而上（与 XOR 位图同序）。
        # 位 = 1 表示**透明**（老式 `dest AND mask` 路径用；认 alpha 的渲染器忽略它）。
        $maskRow = [int]([math]::Floor(($size + 31) / 32) * 4)
        $mask = New-Object byte[] ($maskRow * $size)
        for ($y = 0; $y -lt $size; $y++) {
            $srcRow = $y * $stride
            $dstRow = ($size - 1 - $y) * $maskRow
            for ($x = 0; $x -lt $size; $x++) {
                if ($pixels[$srcRow + $x * 4 + 3] -lt 128) {
                    $mask[$dstRow + [int][math]::Floor($x / 8)] = $mask[$dstRow + [int][math]::Floor($x / 8)] -bor (0x80 -shr ($x % 8))
                }
            }
        }
        $bw.Write($mask)
        $bw.Flush()
        # 逗号前缀：PS 输出会把数组**展开**成 Object[]（逐元素装箱），
        # 后续 BinaryWriter.Write 会因此选中 Write(byte) 重载只写 1 字节
        return , $ms.ToArray()
    }
    finally { $bw.Dispose(); $ms.Dispose() }
}

function Get-PngEntryBytes {
    param([System.Drawing.Bitmap]$Bmp)
    $ms = New-Object System.IO.MemoryStream
    try {
        $Bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        return , $ms.ToArray()
    }
    finally { $ms.Dispose() }
}

# ---- 源收集与校验 ----------------------------------------------------------
$available = @{}      # 边长 → 逐尺寸图绝对路径
foreach ($s in $Sizes) {
    $p = Join-Path $PerSizeDir ("hzip-{0}.png" -f $s)
    if (Test-Path -LiteralPath $p) { $available[$s] = (Resolve-Path -LiteralPath $p).Path }
}

$base = $null
if ($Source) {
    $srcPath = (Resolve-Path -LiteralPath $Source).Path
    $base = [System.Drawing.Image]::FromFile($srcPath)
    if ($base.Width -ne $base.Height) {
        throw "基准图必须是正方形（当前 $($base.Width)×$($base.Height)）：$srcPath"
    }
    if ($base.Width -lt 256) {
        throw "基准图边长须 ≥ 256（当前 $($base.Width)）：$srcPath"
    }
    Write-Host "[icon] 基准图 $srcPath ($($base.Width)×$($base.Height))"
}

try {
    if ($available.Count -eq 0 -and -not $base) {
        throw "没有任何图标源：$PerSizeDir 下需要 hzip-<N>.png（N ∈ $($Sizes -join '/')），或用 -Source 指定基准大图"
    }
    foreach ($s in ($available.Keys | Sort-Object)) {
        $im = [System.Drawing.Image]::FromFile($available[$s])
        try {
            if ($im.Width -ne $s -or $im.Height -ne $s) {
                throw "逐尺寸图标尺寸不符：$($available[$s]) 是 $($im.Width)×$($im.Height)，应为 $s×$s"
            }
        }
        finally { $im.Dispose() }
    }
    Write-Host "[icon] 逐尺寸图 $($available.Count) 档：$(($available.Keys | Sort-Object) -join '/')"

    # 逐档解析：返回位图 + 来源说明
    $resolve = {
        param([int]$Size)
        if ($available.ContainsKey($Size)) {
            return @{ Bmp = (New-ExactBitmap -Path $available[$Size] -Size $Size); Origin = "hzip-$Size.png" }
        }
        if ($base) {
            return @{ Bmp = (New-ScaledBitmap -Src $base -Size $Size); Origin = "基准图缩放" }
        }
        # @() 包裹：单个匹配时 Where/Sort 返回标量，StrictMode 下取 .Count 会抛
        $bigger = @($available.Keys | Where-Object { $_ -gt $Size } | Sort-Object)
        if ($bigger.Count -gt 0) {
            $from = $bigger[0]
            $note = ''
        }
        else {
            $from = ($available.Keys | Sort-Object)[-1]
            $note = '（放大，建议补一张该档手工图）'
        }
        $im = [System.Drawing.Image]::FromFile($available[$from])
        try { return @{ Bmp = (New-ScaledBitmap -Src $im -Size $Size); Origin = "hzip-$from.png 缩放$note" } }
        finally { $im.Dispose() }
    }

    # ---- ICO ---------------------------------------------------------------
    $entries = @()
    foreach ($s in $Sizes) {
        $r = & $resolve $s
        try {
            $bytes = if ($s -ge $PngThreshold) { Get-PngEntryBytes -Bmp $r.Bmp } else { Get-DibEntryBytes -Bmp $r.Bmp }
        }
        finally { $r.Bmp.Dispose() }
        $entries += [pscustomobject]@{ Size = $s; Bytes = [byte[]] $bytes; Origin = $r.Origin }
    }

    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter $ms
    try {
        $bw.Write([uint16]0)                 # reserved
        $bw.Write([uint16]1)                 # type = 1（图标）
        $bw.Write([uint16]$entries.Count)    # 图像数
        $offset = 6 + 16 * $entries.Count
        foreach ($e in $entries) {
            # 目录项：256 档宽高记 0（ICO 规范的「256 及以上」编码）
            $dim = if ($e.Size -ge 256) { 0 } else { $e.Size }
            $bw.Write([byte]$dim); $bw.Write([byte]$dim)
            $bw.Write([byte]0); $bw.Write([byte]0)          # 调色板数 / reserved
            $bw.Write([uint16]1); $bw.Write([uint16]32)     # planes / bitCount
            $bw.Write([uint32]$e.Bytes.Length)
            $bw.Write([uint32]$offset)
            $offset += $e.Bytes.Length
        }
        foreach ($e in $entries) { $bw.Write([byte[]] $e.Bytes, 0, [int] $e.Bytes.Length) }
        $bw.Flush()
        [System.IO.File]::WriteAllBytes($IcoPath, $ms.ToArray())
    }
    finally { $bw.Dispose(); $ms.Dispose() }

    $icoSize = (Get-Item -LiteralPath $IcoPath).Length
    Write-Host "[icon] $IcoPath  ($($entries.Count) 档；$([math]::Round($icoSize / 1KB, 1)) KB)"
    foreach ($e in $entries) {
        Write-Host ("         {0,3}×{0,-3} ← {1}" -f $e.Size, $e.Origin)
    }

    # ---- 稀疏包磁贴 --------------------------------------------------------
    if (-not (Test-Path -LiteralPath $AppxAssetDir)) {
        New-Item -ItemType Directory -Path $AppxAssetDir -Force | Out-Null
    }
    foreach ($name in $Tiles.Keys) {
        $t = $Tiles[$name]
        $r = & $resolve $t
        $out = Join-Path $AppxAssetDir $name
        try { $r.Bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png) }
        finally { $r.Bmp.Dispose() }
        Write-Host "[icon] $out  ($t×$t) ← $($r.Origin)"
    }
}
finally { if ($base) { $base.Dispose() } }
