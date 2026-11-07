# Berthio 安装/更新/卸载脚本的公共函数（Windows / PowerShell 7+）。
# 不要直接执行本文件；由 install.ps1 / update.ps1 / uninstall.ps1 点源加载。

$BerthioRepo   = if ($env:BERTHIO_REPO)   { $env:BERTHIO_REPO }   else { 'yunyu697/berthio' }
$BerthioFeed   = if ($env:BERTHIO_FEED)   { $env:BERTHIO_FEED }   else { "https://github.com/$BerthioRepo/releases/download/updates/latest.json" }
$BerthioPubkeyB64 = 'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDIxRjVBNTFDQTVGQjJFQTQKUldTa0x2dWxIS1gxSVQ0UGl4R3VMZEFmMkhWcEFRdk9nU3lidThGNDV4bEY0VE02Q2tZZ3ovOVkK'

$InstallDir  = if ($env:BERTHIO_INSTALL_DIR) { $env:BERTHIO_INSTALL_DIR } else { Join-Path $env:ProgramFiles 'Berthio' }
$DataDir     = if ($env:BERTHIO_DATA_DIR)    { $env:BERTHIO_DATA_DIR }    else { Join-Path $env:ProgramData 'Berthio' }
$ServiceName = if ($env:BERTHIO_SERVICE_NAME){ $env:BERTHIO_SERVICE_NAME } else { 'berthio' }
$ListenAddr  = if ($env:BERTHIO_LISTEN)      { $env:BERTHIO_LISTEN }      else { '127.0.0.1:18080' }
$HealthUrl   = 'http://127.0.0.1:18080/healthz'

function Get-BinPath { Join-Path $InstallDir 'berthio.exe' }

function Write-Info { param([string]$Message) Write-Host "[berthio] $Message" }
function Write-Warn { param([string]$Message) Write-Host "[berthio] 警告: $Message" -ForegroundColor Yellow }
function Stop-WithError {
    param([string]$Message)
    Write-Host "[berthio] 错误: $Message" -ForegroundColor Red
    exit 1
}

function Test-Admin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Assert-Version {
    param([string]$Version)
    if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(-alpha[0-9]+)?$') {
        Stop-WithError "版本号格式无效：$Version"
    }
}

function Get-VersionKey {
    param([string]$Version)
    if ($Version -match '^([0-9]+)\.([0-9]+)\.([0-9]+)(?:-alpha([0-9]+))?$') {
        $alpha = if ($Matches[4]) { [int]$Matches[4] } else { 999 }
        return ('{0:D4}{1:D4}{2:D4}{3:D3}' -f [int]$Matches[1], [int]$Matches[2], [int]$Matches[3], $alpha)
    }
    Stop-WithError "版本号格式无效：$Version"
}

function Test-VersionGreater {
    param([string]$Left, [string]$Right)
    return ((Get-VersionKey $Left) -gt (Get-VersionKey $Right))
}

function Test-VersionEqual {
    param([string]$Left, [string]$Right)
    return ((Get-VersionKey $Left) -eq (Get-VersionKey $Right))
}

function Get-AssetName {
    param([string]$Version)
    $arch = switch ($env:PROCESSOR_ARCHITECTURE) {
        'AMD64' { 'windows-x86_64' }
        default { Stop-WithError "当前架构 $($env:PROCESSOR_ARCHITECTURE) 暂无发布包（仅提供 windows-x86_64）。" }
    }
    return "berthio-$Version-$arch.exe"
}

function Get-AssetUrl {
    param([string]$Version)
    return "https://github.com/$BerthioRepo/releases/download/$Version/$(Get-AssetName $Version)"
}

function Get-SumsUrl {
    param([string]$Version)
    return "https://github.com/$BerthioRepo/releases/download/$Version/SHA256SUMS"
}

function Invoke-Download {
    param([string]$Url, [string]$OutFile)
    $mirrors = @('', 'https://ghproxy.net/', 'https://gh-proxy.com/', 'https://ghfast.top/')
    foreach ($mirror in $mirrors) {
        try {
            $target = "$mirror$Url"
            if ($mirror) { Write-Info "直连失败，尝试镜像 $mirror" }
            Invoke-WebRequest -Uri $target -OutFile $OutFile -UseBasicParsing -TimeoutSec 600 -MaximumRedirection 10 | Out-Null
            return
        } catch {
            if (-not $mirror) { continue }
        }
    }
    Stop-WithError "下载失败（已尝试官方地址与镜像）：$Url"
}

function Get-Manifest {
    param([string]$Path)
    try {
        return Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
    } catch {
        Stop-WithError "解析 latest.json 失败：$($_.Exception.Message)"
    }
}

function Assert-Sha256 {
    param([string]$File, [string]$Name, [string]$SumsFile)
    if (-not (Test-Path -LiteralPath $SumsFile)) { Stop-WithError "SHA256SUMS 不存在：$SumsFile，已中止。" }
    $entry = $null
    foreach ($line in Get-Content -LiteralPath $SumsFile) {
        $parts = $line -split '\s+'
        if ($parts.Count -ge 2) {
            $fname = $parts[1].TrimStart('*')
            if ($fname -eq $Name) { $entry = $parts[0].ToLower(); break }
        }
    }
    if (-not $entry) { Stop-WithError "SHA256SUMS 中没有 $Name 的条目，已中止。" }
    $got = (Get-FileHash -LiteralPath $File -Algorithm SHA256).Hash.ToLower()
    if ($got -ne $entry) { Stop-WithError "SHA-256 不匹配（期望 $entry，实际 $got），已中止，未安装。" }
    Write-Info "SHA-256 校验通过：$got"
}

function Assert-Minisign {
    param([string]$File, [string]$Name, [string]$Version, [string]$SignatureB64, [string]$MinisignPath)
    $minisign = $MinisignPath
    if (-not $minisign) { $minisign = (Get-Command minisign -ErrorAction SilentlyContinue | Select-Object -First 1).Source }
    if (-not $minisign) {
        Stop-WithError "未找到 minisign。请安装（例如 scoop install minisign）或用 -MinisignPath 指定可执行文件；没有验签工具时拒绝安装。"
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("berthio-verify-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    try {
        $pub = Join-Path $tmp 'pubkey'
        $sig = Join-Path $tmp 'sig'
        [IO.File]::WriteAllBytes($pub, [Convert]::FromBase64String($BerthioPubkeyB64))
        [IO.File]::WriteAllBytes($sig, [Convert]::FromBase64String($SignatureB64))

        & $minisign -Vm $File -p $pub -x $sig -q | Out-Null
        if ($LASTEXITCODE -ne 0) { Stop-WithError "minisign 签名校验失败：安装包与签名不匹配，已中止，未安装。" }

        $comment = (& $minisign -Vm $File -p $pub -x $sig -Q) -join "`n"
        if ($LASTEXITCODE -ne 0 -or -not $comment) { Stop-WithError "无法读取签名的 trusted comment，已中止。" }
        if ($comment -notmatch "version:$([regex]::Escape($Version))") {
            Stop-WithError "签名绑定的版本与清单不一致（$comment），已中止。"
        }
        if ($comment -notmatch "file:$([regex]::Escape($Name))") {
            Stop-WithError "签名绑定的文件名与清单不一致（$comment），已中止。"
        }
        Write-Info "minisign 签名校验通过：$comment"
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Assert-SafePath {
    param([string]$Path)
    if (-not [IO.Path]::IsPathRooted($Path)) { Stop-WithError "拒绝操作：路径不是绝对路径：$Path" }
    if ($Path -match '[?*]') { Stop-WithError "拒绝操作：路径含通配符：$Path" }
    if ($Path -match '\.\.') { Stop-WithError "拒绝操作：路径含 ..：$Path" }
    $full = [IO.Path]::GetFullPath($Path)
    $trimmed = $full.TrimEnd('\')
    if ($trimmed -match '^[A-Za-z]:$' -or $trimmed -match '^[A-Za-z]:\\$' -or $full -eq '\') {
        Stop-WithError "拒绝操作：不能删除盘符根：$full"
    }
    $guarded = @($env:SystemRoot, $env:ProgramFiles, ${env:ProgramFiles(x86)}, $env:ProgramData, $env:USERPROFILE, $env:LOCALAPPDATA) |
        Where-Object { $_ } | ForEach-Object { $_.TrimEnd('\').ToLower() }
    $lower = $trimmed.ToLower()
    if ($guarded -contains $lower) { Stop-WithError "拒绝操作：路径属于系统目录或主目录：$full" }

    $userHome = $env:USERPROFILE.TrimEnd('\')
    if ($lower -eq $userHome.ToLower()) { Stop-WithError "拒绝操作：路径是用户主目录：$full" }
    if (($userHome + '\').ToLower().StartsWith($lower + '\')) {
        Stop-WithError "拒绝操作：路径是主目录的祖先：$full"
    }
    return $full
}

function Get-InstalledVersion {
    $bin = Get-BinPath
    if (-not (Test-Path -LiteralPath $bin)) { return $null }
    try {
        $line = (& $bin --version) -join ' '
        if ($line -match 'berthio\s+([0-9]+\.[0-9]+\.[0-9]+(?:-alpha[0-9]+)?)') { return $Matches[1] }
    } catch { }
    return $null
}

function Get-HealthVersion {
    param([string]$Url)
    try {
        $resp = Invoke-RestMethod -Uri $Url -TimeoutSec 3
        if ($resp.version) { return [string]$resp.version }
    } catch { }
    return $null
}

function Wait-ForVersion {
    param([string]$Url, [string]$Version, [int]$Attempts = 15)
    for ($i = 0; $i -lt $Attempts; $i++) {
        if ((Get-HealthVersion $Url) -eq $Version) { return $true }
        Start-Sleep -Seconds 2
    }
    return $false
}
