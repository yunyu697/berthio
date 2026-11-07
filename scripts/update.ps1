# Berthio 一键更新（Windows / PowerShell 7+），复用 updates/latest.json 通道。
[CmdletBinding()]
param(
    [string]$HealthUrlParam,
    [string]$Prefix,
    [string]$ServiceNameParam,
    [string]$MinisignPath,
    [switch]$NoService,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib/berthio-common.ps1')

if ($Prefix) { $InstallDir = $Prefix }
if ($ServiceNameParam) { $ServiceName = $ServiceNameParam }
if ($HealthUrlParam) { $HealthUrl = $HealthUrlParam }
$BinPath = Get-BinPath

if (-not $DryRun -and -not $NoService -and -not (Test-Admin)) {
    Stop-WithError "重启 Windows 服务需要管理员权限：请以管理员身份运行，或加 -NoService（只替换文件，不重启进程）。"
}
if (-not (Test-Path -LiteralPath $BinPath)) { Stop-WithError "未找到已安装的 $BinPath，请先运行 install.ps1。" }

$current = Get-InstalledVersion
if (-not $current) { Stop-WithError "无法读取当前安装版本（$BinPath --version 失败）。" }
Write-Info "当前版本：$current"

$TmpRun = Join-Path ([IO.Path]::GetTempPath()) ("berthio-update-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $TmpRun -Force | Out-Null
try {
    $manifestFile = Join-Path $TmpRun 'latest.json'
    Invoke-Download -Url $BerthioFeed -OutFile $manifestFile
    $m = Get-Manifest $manifestFile
    $target = [string]$m.version
    Assert-Version $target
    Write-Info "清单版本：$target"

    if (-not (Test-VersionGreater $target $current)) {
        if (Test-VersionEqual $target $current) {
            Write-Info "已是最新版本 $current，无需更新。"
        } else {
            Write-Info "当前版本 $current 高于清单版本 $target，不执行更新。"
        }
        exit 0
    }

    if ($DryRun) {
        Write-Info "[dry-run] 将下载并校验 $(Get-AssetName $target)"
        Write-Info "[dry-run] 将把旧二进制保留为 $(Join-Path $InstallDir "berthio.prev-$current.exe")"
        Write-Info "[dry-run] 将原子替换 $BinPath"
        if ($NoService) {
            Write-Info "[dry-run] -NoService：不重启服务、不做健康回读"
        } else {
            Write-Info "[dry-run] 将重启服务 $ServiceName，并在 $HealthUrl 回读版本；失败则回滚"
        }
        exit 0
    }

    $name = Get-AssetName $target
    $sig = [string]$m.platforms.'windows-x86_64'.signature
    if (-not $sig) { Stop-WithError "清单缺少 windows-x86_64.signature，已中止。" }
    $pkgPath = Join-Path $TmpRun $name
    Write-Info "下载 $name"
    Invoke-Download -Url (Get-AssetUrl $target) -OutFile $pkgPath
    Invoke-Download -Url (Get-SumsUrl $target) -OutFile (Join-Path $TmpRun 'SHA256SUMS')
    $expectedSize = $m.platforms.'windows-x86_64'.size
    if ($expectedSize -and (Get-Item -LiteralPath $pkgPath).Length -ne [int64]$expectedSize) {
        Stop-WithError "安装包大小与清单不一致（期望 $expectedSize 字节），已中止。"
    }
    Assert-Sha256 -File $pkgPath -Name $name -SumsFile (Join-Path $TmpRun 'SHA256SUMS')
    Assert-Minisign -File $pkgPath -Name $name -Version $target -SignatureB64 $sig -MinisignPath $MinisignPath

    $prev = Join-Path $InstallDir "berthio.prev-$current.exe"
    Copy-Item -LiteralPath $BinPath -Destination $prev -Force
    Write-Info "旧二进制已保留：$prev"

    $newTmp = Join-Path $InstallDir ("berthio.new-" + [Guid]::NewGuid().ToString('N') + ".exe")
    Copy-Item -LiteralPath $pkgPath -Destination $newTmp -Force
    Move-Item -LiteralPath $newTmp -Destination $BinPath -Force
    Write-Info "二进制已替换：$BinPath"

    if ($NoService) {
        Write-Info "已替换文件（-NoService：未重启服务、未做健康回读）。请自行重启进程后生效。"
        Write-Info "更新完成：$current -> $target"
        exit 0
    }

    try {
        Restart-Service -Name $ServiceName -Force -ErrorAction Stop
        if (Wait-ForVersion -Url $HealthUrl -Version $target) {
            Write-Info "更新成功：$current -> $target（$HealthUrl 回读一致）"
            Write-Info "回滚命令（如需）：Copy-Item -Force '$prev' '$BinPath' ; Restart-Service $ServiceName"
            exit 0
        }
        throw "健康回读未在限定时间内返回 $target"
    } catch {
        Write-Warn "更新后校验失败（$($_.Exception.Message)），开始回滚到 $current"
        Copy-Item -LiteralPath $prev -Destination $BinPath -Force
        Restart-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue
        if (Wait-ForVersion -Url $HealthUrl -Version $current) {
            Stop-WithError "更新失败，已回滚到 $current 并恢复服务。请查看 Windows 事件日志或手动运行 $BinPath 排查。"
        }
        Stop-WithError "更新失败，已回滚二进制并重启，但健康回读未返回 $current，请手动检查服务。"
    }
} finally {
    Remove-Item -LiteralPath $TmpRun -Recurse -Force -ErrorAction SilentlyContinue
}
