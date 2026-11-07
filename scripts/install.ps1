# Berthio 一键安装（Windows / PowerShell 7+）。
[CmdletBinding()]
param(
    [string]$Package,
    [string]$Checksums,
    [string]$Manifest,
    [string]$Version,
    [string]$Prefix,
    [string]$DataDirParam,
    [string]$ServiceNameParam,
    [string]$MinisignPath,
    [Alias('h')][switch]$Help,
    [switch]$NoService,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib/berthio-common.ps1')

if ($Prefix)      { $InstallDir = $Prefix }
if ($DataDirParam){ $DataDir = $DataDirParam }
if ($ServiceNameParam) { $ServiceName = $ServiceNameParam }
$BinPath = Get-BinPath

function Show-Usage {
    @"
用法: pwsh -File scripts/install.ps1 [选项]

来源：
  （默认）                在线安装最新正式版
  -Package <文件>         使用本地离线包（同目录需有 SHA256SUMS 与 latest.json）
  -Checksums <文件>       离线模式的 SHA256SUMS
  -Manifest <文件>        离线模式的 latest.json
  -Version <X.Y.Z>        在线安装指定版本

安装位置：
  -Prefix <目录>          程序目录（默认 $InstallDir）
  -DataDirParam <目录>    数据目录（默认 $DataDir）
  -ServiceNameParam <名>  服务名（默认 $ServiceName）
  -MinisignPath <文件>    指定 minisign 可执行文件

其它：
  -NoService              仅安装文件，不注册 Windows 服务（测试或自管进程时用）
  -DryRun                 只打印计划，不下载、不写文件、不改服务
"@
}

if ($Help) { Show-Usage; exit 0 }

$TmpRun = Join-Path ([IO.Path]::GetTempPath()) ("berthio-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $TmpRun -Force | Out-Null

$TargetVersion = $null
$VerifiedPkg = $null

try {
    if (-not $DryRun -and -not $NoService -and -not (Test-Admin)) {
        Stop-WithError "注册 Windows 服务需要管理员权限：请用「以管理员身份运行」打开 PowerShell，或加 -NoService 仅安装文件。"
    }

    if ($Package) {
        Write-Info "来源：本地离线包 $Package"
        if (-not (Test-Path -LiteralPath $Package)) { Stop-WithError "离线包不存在：$Package" }
        $sumsFile = if ($Checksums) { $Checksums } else { Join-Path (Split-Path $Package) 'SHA256SUMS' }
        $manifestFile = if ($Manifest) { $Manifest } else { Join-Path (Split-Path $Package) 'latest.json' }
        if (-not (Test-Path -LiteralPath $sumsFile)) { Stop-WithError "离线模式缺少 SHA256SUMS：$sumsFile" }
        if (-not (Test-Path -LiteralPath $manifestFile)) { Stop-WithError "离线模式缺少 latest.json：$manifestFile" }

        $m = Get-Manifest $manifestFile
        $TargetVersion = [string]$m.version
        Assert-Version $TargetVersion
        $name = Split-Path $Package -Leaf
        $sig = [string]$m.platforms.'windows-x86_64'.signature
        if (-not $sig) { Stop-WithError "离线清单缺少 windows-x86_64.signature，已中止。" }
        $expectedSize = $m.platforms.'windows-x86_64'.size
        if ($expectedSize -and (Get-Item -LiteralPath $Package).Length -ne [int64]$expectedSize) {
            Stop-WithError "安装包大小与清单不一致（期望 $expectedSize 字节），已中止。"
        }
        if ($DryRun) {
            Write-Info "[dry-run] 将校验 SHA-256（$sumsFile）与 minisign 签名（$manifestFile）"
        } else {
            Assert-Sha256 -File $Package -Name $name -SumsFile $sumsFile
            Assert-Minisign -File $Package -Name $name -Version $TargetVersion -SignatureB64 $sig -MinisignPath $MinisignPath
        }
        $VerifiedPkg = $Package
    } else {
        $manifestFile = Join-Path $TmpRun 'latest.json'
        if ($DryRun) {
            try {
                Invoke-Download -Url $BerthioFeed -OutFile $manifestFile
                $TargetVersion = [string](Get-Manifest $manifestFile).version
            } catch {
                $TargetVersion = $null
            }
            if (-not $TargetVersion) { $TargetVersion = '最新正式版（运行时读取 latest.json）' }
            Write-Info "[dry-run] 将读取 latest.json 并下载 windows 安装包"
        } else {
            Invoke-Download -Url $BerthioFeed -OutFile $manifestFile
            $m = Get-Manifest $manifestFile
            $TargetVersion = if ($Version) { $Version } else { [string]$m.version }
            Assert-Version $TargetVersion
            $name = Get-AssetName $TargetVersion
            $sig = [string]$m.platforms.'windows-x86_64'.signature
            if (-not $sig) { Stop-WithError "清单缺少 windows-x86_64.signature，已中止。" }
            Write-Info "下载 $name"
            $pkgPath = Join-Path $TmpRun $name
            Invoke-Download -Url (Get-AssetUrl $TargetVersion) -OutFile $pkgPath
            Invoke-Download -Url (Get-SumsUrl $TargetVersion) -OutFile (Join-Path $TmpRun 'SHA256SUMS')
            $expectedSize = $m.platforms.'windows-x86_64'.size
            if ($expectedSize -and (Get-Item -LiteralPath $pkgPath).Length -ne [int64]$expectedSize) {
                Stop-WithError "安装包大小与清单不一致（期望 $expectedSize 字节），已中止。"
            }
            Assert-Sha256 -File $pkgPath -Name $name -SumsFile (Join-Path $TmpRun 'SHA256SUMS')
            Assert-Minisign -File $pkgPath -Name $name -Version $TargetVersion -SignatureB64 $sig -MinisignPath $MinisignPath
            $VerifiedPkg = $pkgPath
        }
    }

    Write-Info "安装版本：$TargetVersion"
    Write-Info "程序目录：$InstallDir"
    Write-Info "数据目录：$DataDir"

    $current = Get-InstalledVersion
    if ($current -and (Test-VersionEqual $current $TargetVersion)) {
        Write-Info "已安装同版本 $current，将只校正文件权限与服务状态（幂等）"
    }

    if ($DryRun) {
        Write-Info "[dry-run] 将创建目录：$InstallDir、$DataDir"
        Write-Info "[dry-run] 将写入二进制：$BinPath（临时文件 + 原子替换）"
        if (-not $NoService) { Write-Info "[dry-run] 将注册/重启 Windows 服务：$ServiceName" }
        Write-Info "[dry-run] 完成后访问：http://$ListenAddr/"
        exit 0
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    New-Item -ItemType Directory -Path $DataDir -Force | Out-Null

    if ($current -and -not (Test-VersionEqual $current $TargetVersion) -and (Test-Path -LiteralPath $BinPath)) {
        $prev = Join-Path $InstallDir "berthio.prev-$current.exe"
        Copy-Item -LiteralPath $BinPath -Destination $prev -Force
        Write-Info "旧二进制已保留：$prev"
    }

    $newTmp = Join-Path $InstallDir ("berthio.new-" + [Guid]::NewGuid().ToString('N') + ".exe")
    Copy-Item -LiteralPath $VerifiedPkg -Destination $newTmp -Force
    Move-Item -LiteralPath $newTmp -Destination $BinPath -Force
    Write-Info "二进制已就位：$BinPath"

    if ($NoService) {
        Write-Info "已跳过服务注册（-NoService）"
    } else {
        [Environment]::SetEnvironmentVariable('BERTHIO_HOME', $DataDir, 'Machine')
        [Environment]::SetEnvironmentVariable('BERTHIO_LISTEN', $ListenAddr, 'Machine')
        $existing = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
        if ($existing) {
            Restart-Service -Name $ServiceName -Force
            Write-Info "服务已重启：$ServiceName"
        } else {
            New-Service -Name $ServiceName -BinaryPathName "`"$BinPath`"" -DisplayName 'Berthio 控制面板' -StartupType Automatic | Out-Null
            Start-Service -Name $ServiceName
            Write-Info "服务已创建并启动：$ServiceName"
        }
        $svc = Get-Service -Name $ServiceName
        if ($svc.Status -ne 'Running') { Stop-WithError "服务 $ServiceName 未处于 Running 状态，安装未完成。" }
    }

    Write-Info ""
    Write-Info "访问地址：http://$ListenAddr/"
    Write-Info "数据目录：$DataDir"
    Write-Info "配置文件：$(Join-Path $DataDir 'config/panel.json')"
    Write-Info "下一步：浏览器打开上面的地址，按页面提示设置面板口令；对外暴露前请先配置 HTTPS。"
} finally {
    Remove-Item -LiteralPath $TmpRun -Recurse -Force -ErrorAction SilentlyContinue
}
