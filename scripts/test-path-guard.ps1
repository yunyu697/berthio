# 路径护栏自测：只在 D: 盘的项目 tmp 目录里创建/删除，不触碰 C: 盘。
# 必须全部拒绝：用户主目录、驱动根、$env:USERPROFILE、空字符串、../ 逃逸、未展开变量。
# 必须放行：允许根下的临时目录；且默认 dry-run 不得删除任何东西。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib\path-guard.ps1')

$script:fail = 0
function Check([bool]$cond, [string]$name, [string]$detail = '') {
  if ($cond) { Write-Host ("PASS  {0}  {1}  {2}" -f $name, $detail, '') }
  else { Write-Host ("FAIL  {0}  {1}" -f $name, $detail); $script:fail++ }
}

$repo = Split-Path -Parent $PSScriptRoot
$Allowed = Join-Path $repo 'tmp'
New-Item -ItemType Directory -Force -Path $Allowed | Out-Null
if (-not ([IO.Path]::GetPathRoot($Allowed).StartsWith('D:', [StringComparison]::OrdinalIgnoreCase))) {
  throw "自测只允许在 D: 盘运行，当前允许根: $Allowed"
}

function Expect-Reject([AllowNull()][AllowEmptyString()][string]$Path, [string]$name) {
  try {
    $v = Test-GuardPath -Path $Path -AllowedRoot $Allowed
    if ($v.Ok) { Check $false $name ("意外放行: " + $v.FullPath) }
    else { Check $true $name ("拒绝原因=" + $v.Reason) }
  } catch {
    Check $false $name ("抛异常而非返回拒绝: " + $_.Exception.Message)
  }
}

Write-Host ("允许根: {0}" -f $Allowed)

# ---------------------------------------------------------------- 必须拒绝
$userProfile = [string]$env:USERPROFILE
Expect-Reject $userProfile '拒绝 $env:USERPROFILE'
Expect-Reject 'C:\Users\Tyler Zhang' '拒绝 C:\Users\Tyler Zhang'
Expect-Reject 'C:\' '拒绝驱动根 C:\'
Expect-Reject '' '拒绝空字符串'
Expect-Reject $null '拒绝未定义（null）'
Expect-Reject '..\..\Users' '拒绝 ..\..\Users 逃逸'
Expect-Reject '..\..\Users\Tyler Zhang' '拒绝 ..\..\Users\Tyler Zhang 逃逸'
Expect-Reject '$env:USERPROFILE' '拒绝未展开的 $env:USERPROFILE 字面量'
Expect-Reject '%USERPROFILE%' '拒绝未展开的 %USERPROFILE%'
Expect-Reject '%TEMP%' '拒绝未展开的 %TEMP%'
if (-not [string]::IsNullOrWhiteSpace($userProfile)) {
  Expect-Reject (Join-Path $userProfile 'Downloads') '拒绝用户主目录之内的路径'
}

# ---------------------------------------------------------------- 放行 + 默认 dry-run
$Target = Join-Path $Allowed ('path-guard-selftest-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path (Join-Path $Target 'sub') | Out-Null
Set-Content -LiteralPath (Join-Path $Target 'sub\a.txt') -Value 'guard' -Encoding UTF8
Set-Content -LiteralPath (Join-Path $Target 'b.txt') -Value 'guard' -Encoding UTF8

$v = Test-GuardPath -Path $Target -AllowedRoot $Allowed
Check ($v.Ok -and $v.Kind -eq 'Directory' -and $v.Entries -ge 3) '允许根下的临时目录被放行' ("entries=" + $v.Entries)

$manifest = Join-Path $Allowed 'guard-audit-selftest.jsonl'
$dry = Remove-GuardedPath -Path $Target -AllowedRoot $Allowed -Manifest $manifest
Check ((-not $dry.Applied) -and (Test-Path -LiteralPath $Target)) '默认 dry-run 不删除' ("applied=" + $dry.Applied)
Check (Test-Path -LiteralPath $manifest) '确认清单在删除前落盘'

$apply = Remove-GuardedPath -Path $Target -AllowedRoot $Allowed -Apply -Manifest $manifest
Check ($apply.Applied -and -not (Test-Path -LiteralPath $Target)) '显式 -Apply 才删除'
Check ((@(Get-Content -LiteralPath $manifest).Count) -ge 2) '确认清单记录了 dry-run 与 apply 两次操作'

# ---------------------------------------------------------------- Copy / Move 也走同一护栏
$CopyTarget = Join-Path $Allowed ('path-guard-copy-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $CopyTarget | Out-Null
$srcFile = Join-Path $CopyTarget 'src.txt'
$dstFile = Join-Path $CopyTarget 'dst.txt'
Set-Content -LiteralPath $srcFile -Value 'copy' -Encoding UTF8
$cp = Copy-GuardedItem -Path $srcFile -Destination $dstFile -AllowedRoot $Allowed -Apply
Check ($cp.Applied -and (Test-Path -LiteralPath $dstFile)) '允许根内的复制被执行'

$moveFile = Join-Path $CopyTarget 'moved.txt'
$mv = Move-GuardedItem -Path $dstFile -Destination $moveFile -AllowedRoot $Allowed -Apply
Check ($mv.Applied -and (Test-Path -LiteralPath $moveFile) -and -not (Test-Path -LiteralPath $dstFile)) '允许根内的移动被执行'

Remove-GuardedPath -Path $CopyTarget -AllowedRoot $Allowed -Apply | Out-Null
Check (-not (Test-Path -LiteralPath $CopyTarget)) '复制/移动测试目录已清理'

Write-Host ''
if ($script:fail -eq 0) { Write-Host '路径护栏自测：全部通过' } else { Write-Host ("路径护栏自测：{0} 项失败" -f $script:fail) }
exit $(if ($script:fail -eq 0) { 0 } else { 1 })
