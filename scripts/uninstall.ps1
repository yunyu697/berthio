# Berthio 一键卸载（Windows / PowerShell 7+）。默认保留数据目录，只有 -Purge 才删数据。
[CmdletBinding()]
param(
    [string]$Prefix,
    [string]$DataDirParam,
    [string]$ServiceNameParam,
    [switch]$Purge,
    [string]$ConfirmPurge,
    [switch]$NoService,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib/berthio-common.ps1')

if ($Prefix) { $InstallDir = $Prefix }
if ($DataDirParam) { $DataDir = $DataDirParam }
if ($ServiceNameParam) { $ServiceName = $ServiceNameParam }
$BinPath = Get-BinPath

if (-not $DryRun -and -not $NoService -and -not (Test-Admin)) {
    Stop-WithError "卸载需要管理员权限（停止/删除服务与程序目录）：请以管理员身份运行 PowerShell，或先用 -DryRun 查看计划；仅删文件可加 -NoService。"
}

$targets = @($InstallDir)
if ($Purge) { $targets += $DataDir }

foreach ($t in $targets) {
    $full = Assert-SafePath $t
    Write-Verbose "白名单校验通过：$full"
}

if ($Purge) {
    Write-Info "将要删除的绝对路径（逐条白名单校验通过）："
    foreach ($t in $targets) {
        $full = [IO.Path]::GetFullPath($t)
        Write-Host "  - $full"
    }
    Write-Host "  - Windows 服务 $ServiceName（若存在）"

    if ($DryRun) {
        Write-Info "[dry-run] 以上路径不会真的被删除"
    } elseif ($ConfirmPurge) {
        if ($ConfirmPurge -ne 'DELETE') { Stop-WithError "-ConfirmPurge 的值必须是 DELETE。未删除任何文件。" }
        Write-Info "已收到显式确认 -ConfirmPurge DELETE，继续。"
    } elseif (-not [Console]::IsInputRedirected -and [Environment]::UserInteractive) {
        $answer = Read-Host "`n这会永久删除上面的数据目录，且不可恢复。输入 yes 继续"
        if ($answer -ne 'yes') { Stop-WithError "未得到 yes，已取消，未删除任何文件。" }
    } else {
        Stop-WithError "非交互模式执行 -Purge 需要额外显式参数：-ConfirmPurge DELETE。未删除任何文件。"
    }
}

if ($DryRun) {
    Write-Info "[dry-run] 将停止并删除服务：$ServiceName"
    Write-Info "[dry-run] 将删除程序目录：$InstallDir"
    if ($Purge) {
        Write-Info "[dry-run] 将删除数据目录：$DataDir"
    } else {
        Write-Info "[dry-run] 保留数据目录：$DataDir（含 config\panel.json）"
    }
    exit 0
}

$svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($NoService) {
    Write-Info "已跳过服务处理（-NoService）"
} elseif ($svc) {
    if ($svc.Status -eq 'Running') { Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue }
    & sc.exe delete $ServiceName | Out-Null
    Write-Info "服务已停止并删除：$ServiceName"
} else {
    Write-Info "服务不存在，跳过：$ServiceName"
}

if (-not $NoService) {
    [Environment]::SetEnvironmentVariable('BERTHIO_HOME', $null, 'Machine')
    [Environment]::SetEnvironmentVariable('BERTHIO_LISTEN', $null, 'Machine')
}

if (Test-Path -LiteralPath $InstallDir) {
    Remove-Item -LiteralPath $InstallDir -Recurse -Force
    Write-Info "已删除程序目录：$InstallDir"
} else {
    Write-Info "程序目录不存在，跳过：$InstallDir"
}

if ($Purge) {
    if (Test-Path -LiteralPath $DataDir) {
        Remove-Item -LiteralPath $DataDir -Recurse -Force
        Write-Info "已删除数据目录：$DataDir"
    } else {
        Write-Info "数据目录不存在，跳过：$DataDir"
    }
    Write-Info "卸载完成（-Purge：数据已删除）"
} else {
    Write-Info "卸载完成；数据目录已保留：$DataDir"
    Write-Info "如需彻底清除数据，重新运行：pwsh -File scripts/uninstall.ps1 -Purge -ConfirmPurge DELETE"
}
