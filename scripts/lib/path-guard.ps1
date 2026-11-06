# 路径护栏（PowerShell）：所有删除 / 移动 / 覆盖操作都必须先经过这里。
#
# 规则：
#   - 目标必须解析为绝对路径，并落在允许根之下（允许根=项目目录或用户显式指定的测试目录）；
#     比较用规范化后的绝对路径，Windows 下不区分大小写。
#   - 拒绝驱动根 / UNC 根、拒绝用户主目录（及其祖先、子孙）、拒绝空值与未定义变量、
#     拒绝含未展开变量的路径（$VAR、%VAR%）。
#   - 默认 dry-run：只打印将要操作的绝对路径与条目数；必须显式 -Apply 才真正执行。
#   - 删除前会重新解析并复核一次，降低检查与执行之间被替换的风险。
#
# 用法：
#   . "$PSScriptRoot\lib\path-guard.ps1"
#   Remove-GuardedPath -Path $dir -AllowedRoot $root -Apply
#   Invoke-GuardedFileOperation -Operation Copy -Path $src -Destination $dst -AllowedRoot $root -Apply
#
# 注意：库文件本身不做任何删除；只有显式 -Apply 且全部校验通过时才执行。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-GuardUserHomes {
  $list = New-Object 'System.Collections.Generic.List[string]'
  $candidates = @(
    [Environment]::GetFolderPath('UserProfile'),
    $HOME,
    $env:USERPROFILE
  )
  if ($env:HOMEDRIVE -and $env:HOMEPATH) { $candidates += ($env:HOMEDRIVE + $env:HOMEPATH) }
  foreach ($c in $candidates) {
    if ([string]::IsNullOrWhiteSpace($c)) { continue }
    try {
      $full = [IO.Path]::GetFullPath($c).TrimEnd('\', '/')
      if (-not [string]::IsNullOrWhiteSpace($full) -and -not $list.Contains($full)) { $list.Add($full) }
    } catch { }
  }
  return ,$list.ToArray()
}

function Resolve-GuardPath {
  param([Parameter(Mandatory)][string]$InputPath)
  $full = [IO.Path]::GetFullPath($InputPath)
  # 逐级解开 symlink / junction，避免“允许根内的链接指向根外”。
  for ($depth = 0; $depth -lt 8; $depth++) {
    $root = [IO.Path]::GetPathRoot($full)
    if ([string]::IsNullOrEmpty($root)) { return $full }
    $tail = $full.Substring($root.Length)
    $parts = @($tail -split '[\\/]' | Where-Object { -not [string]::IsNullOrEmpty($_) })
    $cur = $root
    $replaced = $false
    for ($i = 0; $i -lt $parts.Count; $i++) {
      $cand = Join-Path $cur $parts[$i]
      $item = Get-Item -LiteralPath $cand -Force -ErrorAction SilentlyContinue
      if ($null -ne $item -and $null -ne $item.LinkType) {
        $target = @($item.Target)[0]
        if ([string]::IsNullOrEmpty($target)) {
          throw [System.ArgumentException]::new("链接目标不可解析，拒绝操作: $InputPath")
        }
        if (-not [IO.Path]::IsPathRooted($target)) { $target = Join-Path $cur $target }
        $rest = @()
        if ($i -lt ($parts.Count - 1)) { $rest = @($parts[($i + 1)..($parts.Count - 1)]) }
        $full = [IO.Path]::GetFullPath($target)
        if ($rest.Count -gt 0) { $full = Join-Path $full ($rest -join [IO.Path]::DirectorySeparatorChar) }
        $replaced = $true
        break
      }
      $cur = $cand
    }
    if (-not $replaced) { return $full }
  }
  throw [System.ArgumentException]::new("路径重解析层级过多，拒绝操作: $InputPath")
}

function Test-GuardSamePath {
  param([string]$Left, [string]$Right)
  $l = $Left.TrimEnd('\', '/')
  $r = $Right.TrimEnd('\', '/')
  return [string]::Equals($l, $r, [StringComparison]::OrdinalIgnoreCase)
}

function Test-GuardUnderRoot {
  param([string]$Candidate, [string]$Root)
  $c = $Candidate.TrimEnd('\', '/')
  $r = $Root.TrimEnd('\', '/')
  if ($c.Length -le $r.Length) { return $false }
  if (-not $c.StartsWith($r, [StringComparison]::OrdinalIgnoreCase)) { return $false }
  $boundary = $c[$r.Length]
  return ($boundary -eq '\' -or $boundary -eq '/')
}

# 只做校验，不执行任何操作。返回 Ok/Reason/FullPath/Kind/Entries。
function Test-GuardPath {
  [CmdletBinding()]
  param(
    [AllowNull()][AllowEmptyString()][string]$Path,
    [string[]]$AllowedRoot = @(),
    [switch]$AllowMissing
  )
  $v = [pscustomobject]@{
    Ok        = $false
    Reason    = ''
    FullPath  = ''
    Kind      = 'Missing'
    Entries   = 0
  }

  if ([string]::IsNullOrWhiteSpace($Path)) { $v.Reason = '空值或未定义路径'; return $v }
  if ($Path -match '\$') { $v.Reason = '路径含未展开的 $ 变量'; return $v }
  if ($Path -match '%[A-Za-z_][A-Za-z0-9_]*%') { $v.Reason = '路径含未展开的 %VAR% 变量'; return $v }

  try { $full = Resolve-GuardPath $Path } catch { $v.Reason = "无法解析路径: $($_.Exception.Message)"; return $v }
  $v.FullPath = $full

  $pathRoot = [IO.Path]::GetPathRoot($full)
  if ([string]::IsNullOrEmpty($pathRoot)) { $v.Reason = '无法识别路径根'; return $v }
  if (Test-GuardSamePath $full $pathRoot) { $v.Reason = '拒绝驱动根 / UNC 根'; return $v }

  foreach ($h in (Get-GuardUserHomes)) {
    if (Test-GuardSamePath $full $h) { $v.Reason = '拒绝用户主目录本身'; return $v }
    if (Test-GuardUnderRoot -Candidate $h -Root $full) { $v.Reason = '拒绝用户主目录的祖先路径'; return $v }
    if (Test-GuardUnderRoot -Candidate $full -Root $h) { $v.Reason = '拒绝用户主目录之内的路径'; return $v }
  }

  $matched = $false
  foreach ($r in $AllowedRoot) {
    if ([string]::IsNullOrWhiteSpace($r)) { continue }
    if ($r -match '\$' -or $r -match '%[A-Za-z_][A-Za-z0-9_]*%') { continue }
    try { $rf = Resolve-GuardPath $r } catch { continue }
    if (Test-GuardSamePath $full $rf) {
      $v.Reason = '拒绝操作允许根本身（请指向根下的子项）'
      return $v
    }
    if (Test-GuardUnderRoot -Candidate $full -Root $rf) { $matched = $true; break }
  }
  if (-not $matched) { $v.Reason = '目标不在允许根之下'; return $v }

  $item = Get-Item -LiteralPath $full -Force -ErrorAction SilentlyContinue
  if ($null -eq $item) {
    if ($AllowMissing) { $v.Ok = $true; $v.Kind = 'Missing'; return $v }
    $v.Reason = '目标不存在'
    return $v
  }
  if ($item.PSIsContainer) {
    $v.Kind = 'Directory'
    $v.Entries = @(Get-ChildItem -LiteralPath $full -Recurse -Force -ErrorAction SilentlyContinue).Count
  } else {
    $v.Kind = 'File'
    $v.Entries = 1
  }
  $v.Ok = $true
  return $v
}

# 唯一的执行入口。默认 dry-run；-Apply 才真正执行。
function Invoke-GuardedFileOperation {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][ValidateSet('Remove', 'Copy', 'Move')][string]$Operation,
    [Parameter(Mandatory)][AllowEmptyString()][string]$Path,
    [AllowEmptyString()][string]$Destination = '',
    [string[]]$AllowedRoot = @(),
    [switch]$Apply,
    [switch]$AllowMissing
  )

  # 复制只写目标：源只需通过“绝对路径 + 非根/非主目录”校验（允许从只读夹具读取）；
  # 移动会删除源，因此源也必须落在允许根之下。
  $srcRoots = $AllowedRoot
  if ($Operation -eq 'Copy') {
    $srcParent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($Path))
    if (-not [string]::IsNullOrEmpty($srcParent)) { $srcRoots = @($srcParent) }
  }
  $src = Test-GuardPath -Path $Path -AllowedRoot $srcRoots -AllowMissing:$AllowMissing
  if (-not $src.Ok) {
    throw [System.ArgumentException]::new("路径护栏拒绝（$Operation）：$($src.Reason)；输入=$Path")
  }

  $dst = $null
  if ($Operation -ne 'Remove') {
    if ([string]::IsNullOrWhiteSpace($Destination)) {
      throw [System.ArgumentException]::new("路径护栏拒绝（$Operation）：缺少目标路径")
    }
    $dst = Test-GuardPath -Path $Destination -AllowedRoot $AllowedRoot -AllowMissing
    if (-not $dst.Ok) {
      throw [System.ArgumentException]::new("路径护栏拒绝（$Operation）：目标路径不合规：$($dst.Reason)；输入=$Destination")
    }
  }

  $mode = if ($Apply) { 'APPLY' } else { 'DRY-RUN' }
  Write-Host ("[guard] {0} {1}" -f $mode, $Operation.ToUpperInvariant())
  Write-Host ("[guard]   源: {0}" -f $src.FullPath)
  Write-Host ("[guard]   条目: {0}（{1}）" -f $src.Entries, $src.Kind)
  if ($Operation -ne 'Remove') { Write-Host ("[guard]   目标: {0}" -f $dst.FullPath) }

  $result = [pscustomobject]@{
    Applied     = [bool]$Apply
    Operation   = $Operation
    FullPath    = $src.FullPath
    Entries     = $src.Entries
    Destination = $(if ($dst) { $dst.FullPath } else { '' })
  }

  if (-not $Apply) { return $result }
  if ($src.Kind -eq 'Missing') { return $result }

  # 执行前复核，避免检查后被替换成根外路径。
  $recheck = Test-GuardPath -Path $src.FullPath -AllowedRoot $AllowedRoot
  if (-not $recheck.Ok) {
    throw [System.ArgumentException]::new("路径护栏拒绝（$Operation）：执行前复核失败：$($recheck.Reason)")
  }

  switch ($Operation) {
    'Remove' {
      if ($src.Kind -eq 'Directory') {
        Remove-Item -LiteralPath $src.FullPath -Recurse -Force
      } else {
        Remove-Item -LiteralPath $src.FullPath -Force
      }
    }
    'Copy' { Copy-Item -LiteralPath $src.FullPath -Destination $dst.FullPath -Force -Recurse }
    'Move' { Move-Item -LiteralPath $src.FullPath -Destination $dst.FullPath -Force }
  }
  return $result
}

function Remove-GuardedPath {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][AllowEmptyString()][string]$Path,
    [string[]]$AllowedRoot = @(),
    [switch]$Apply,
    [switch]$AllowMissing
  )
  return Invoke-GuardedFileOperation -Operation Remove -Path $Path -AllowedRoot $AllowedRoot -Apply:$Apply -AllowMissing:$AllowMissing
}

function Copy-GuardedItem {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][AllowEmptyString()][string]$Path,
    [Parameter(Mandatory)][AllowEmptyString()][string]$Destination,
    [string[]]$AllowedRoot = @(),
    [switch]$Apply,
    [switch]$AllowMissing
  )
  return Invoke-GuardedFileOperation -Operation Copy -Path $Path -Destination $Destination -AllowedRoot $AllowedRoot -Apply:$Apply -AllowMissing:$AllowMissing
}

function Move-GuardedItem {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][AllowEmptyString()][string]$Path,
    [Parameter(Mandatory)][AllowEmptyString()][string]$Destination,
    [string[]]$AllowedRoot = @(),
    [switch]$Apply,
    [switch]$AllowMissing
  )
  return Invoke-GuardedFileOperation -Operation Move -Path $Path -Destination $Destination -AllowedRoot $AllowedRoot -Apply:$Apply -AllowMissing:$AllowMissing
}
