# M5 升级路径：alpha4（schema v1）→ 1.0.0（schema v2）。
# 覆盖：state/panel 自动迁移、备份留档、二次启动幂等、
#       高版本拒绝启动、损坏 JSON 不覆盖、迁移校验失败保留原文件。
#
# 用法：
#   .\scripts\test-m5-upgrade.ps1 -Exe .\target\debug\berthio.exe
param(
  [string]$Exe = "",
  [int]$Port = 18190
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
if ($PSVersionTable.PSVersion.Major -lt 6) {
  $pwsh = Get-Command pwsh -ErrorAction SilentlyContinue
  if ($pwsh) {
    & $pwsh.Source -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Exe $Exe -Port $Port
    exit $LASTEXITCODE
  }
  throw "需要 PowerShell 7+（pwsh）"
}
$script:fail = 0
$root = Split-Path -Parent $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $root "target\debug\berthio.exe" }
if (-not (Test-Path $Exe)) { throw "找不到 $Exe，先 cargo build" }

$Base = "http://127.0.0.1:$Port"
$bh = Join-Path (Join-Path $root "tmp") ("berthio-m5-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$Password = "Berthio#2026"
New-Item -ItemType Directory -Force -Path (Join-Path $bh "config"), (Join-Path $bh "state"), (Join-Path $bh "logs/audit") | Out-Null

function Check([bool]$cond, [string]$name, [string]$detail = "") {
  if ($cond) { Write-Host ("PASS  {0}  {1}" -f $name, $detail) }
  else { Write-Host ("FAIL  {0}  {1}" -f $name, $detail); $script:fail++ }
}

function Write-Utf8([string]$path, [string]$text) {
  [System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding($false)))
}

function Read-Json([string]$path) { Get-Content -Raw -LiteralPath $path | ConvertFrom-Json }

function Read-Text([string]$path) {
  $t = Get-Content -Raw -LiteralPath $path -ErrorAction SilentlyContinue
  if ($null -eq $t) { return "" }
  return $t
}

function State-Path { Join-Path $bh "state/state.json" }
function Config-Path { Join-Path $bh "config/panel.json" }

function New-V1State([string]$target = "clash", [string]$source = "", [string]$inboundsJson = "") {
  if (-not $inboundsJson) {
    $inboundsJson = @'
    [
      {
        "id": "inb_legacy1",
        "tag": "legacy-jp",
        "remark": "legacy-jp",
        "protocol": "vless",
        "listen": "0.0.0.0",
        "port": 2443,
        "l4": "tcp",
        "network": "tcp",
        "security": "reality",
        "server_kernel": "xray",
        "server_version": "25.6.8",
        "enabled": true,
        "source": "",
        "created_at": "2026-10-01T00:00:00Z",
        "updated_at": "2026-10-01T00:00:00Z",
        "clients": [
          { "id": "cl_legacy1", "email": "legacy@example.com", "uuid": "11111111-1111-4111-8111-111111111111", "enabled": true }
        ]
      }
    ]
'@
  }
  $body = @"
{
  "schema_version": 1,
  "panel": { "panel_address": "panel.example.com", "created_at": "2026-10-01T00:00:00Z" },
  "inbounds": $inboundsJson,
  "tokens": [
    {
      "id": "tok_legacy1",
      "client_id": "cl_legacy1",
      "token_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "target": "$target",
      "created_at": "2026-10-01T00:00:00Z",
      "revoked_at": null,
      "last_access": null
    }
  ]
}
"@
  Write-Utf8 (State-Path) $body
}

function New-V1Config {
  $body = @"
{
  "schema_version": 1,
  "listen": "127.0.0.1:$Port",
  "panel_address": "panel.example.com",
  "log_keep_days": 0,
  "remote_access": {
    "enabled": false,
    "acknowledged_at": "",
    "acknowledged_by": "",
    "trusted_proxies": [" 10.0.0.1 ", "", "   "]
  }
}
"@
  Write-Utf8 (Config-Path) $body
}

function Start-Raw {
  $out = Join-Path $bh "panel.out.log"
  $err = Join-Path $bh "panel.err.log"
  $env:BERTHIO_HOME = $bh
  Remove-Item Env:BERTHIO_LISTEN -ErrorAction SilentlyContinue
  $sp = Start-Process -FilePath $Exe -PassThru -NoNewWindow -RedirectStandardOutput $out -RedirectStandardError $err
  [pscustomobject]@{ Process = $sp; ErrLog = $err }
}

function Wait-Health([object]$h, [int]$TimeoutMs = 15000) {
  $deadline = (Get-Date).AddMilliseconds($TimeoutMs)
  while ((Get-Date) -lt $deadline) {
    if ($h.Process.HasExited) { return $false }
    try { Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck -TimeoutSec 1 | Out-Null; return $true } catch { Start-Sleep -Milliseconds 200 }
  }
  return $false
}

function Wait-Exit([object]$h, [int]$TimeoutMs = 15000) {
  return $h.Process.WaitForExit($TimeoutMs)
}

function Stop-Panel([object]$h) {
  if ($h -and -not $h.Process.HasExited) { Stop-Process -Id $h.Process.Id -Force; $h.Process.WaitForExit(5000) | Out-Null }
  Start-Sleep -Milliseconds 300
}

function Call([string]$method, [string]$path, $body = $null, $sess = $null, [hashtable]$headers = @{}) {
  $p = @{ Uri = ($Base + $path); Method = $method; SkipHttpErrorCheck = $true }
  if ($sess) { $p.WebSession = $sess }
  if ($null -ne $body) {
    $p.ContentType = 'application/json; charset=utf-8'
    $p.Body = [System.Text.Encoding]::UTF8.GetBytes(($body | ConvertTo-Json -Compress -Depth 12))
  }
  if ($headers.Count -gt 0) { $p.Headers = $headers }
  $r = Invoke-WebRequest @p
  $j = $null
  try { $j = $r.Content | ConvertFrom-Json } catch {}
  [pscustomobject]@{ Status = [int]$r.StatusCode; Body = $j; Raw = $r.Content }
}

function Backup-Count([string]$pattern) {
  @(Get-ChildItem (Join-Path $bh "state/backups") -Filter $pattern -ErrorAction SilentlyContinue).Count
}

Write-Host "== M5 升级路径 @ $Base（HOME=$bh） =="
$panel = $null
try {
  # ---------- A. v1 → v2 自动迁移 ----------
  New-V1Config
  New-V1State
  $stateV1Hash = (Get-FileHash (State-Path) -Algorithm SHA256).Hash
  $cfgV1Hash = (Get-FileHash (Config-Path) -Algorithm SHA256).Hash

  $panel = Start-Raw
  Check (Wait-Health $panel) "alpha4 状态启动成功（自动迁移后）"
  $errText = Read-Text $panel.ErrLog
  Check ($errText -match "schema 升级") "启动日志报告了 schema 升级" $errText.Trim()
  Check ($errText -match "state schema v1 → v2") "state 升级到 v2 有日志"
  Check ($errText -match "panel schema v1 → v2") "panel 升级到 v2 有日志"

  $state = Read-Json (State-Path)
  Check ($state.schema_version -eq 2) "state.json schema_version=2" ("actual=" + $state.schema_version)
  Check ($state.tokens[0].target -eq "mihomo") "token target clash→mihomo" ("actual=" + $state.tokens[0].target)
  Check ($state.inbounds[0].source -eq "manual") "入站空 source→manual" ("actual=" + $state.inbounds[0].source)
  Check ($state.inbounds[0].clients.Count -eq 1) "入站客户端保留"

  $cfg = Read-Json (Config-Path)
  Check ($cfg.schema_version -eq 2) "panel.json schema_version=2" ("actual=" + $cfg.schema_version)
  Check ($cfg.log_keep_days -eq 14) "log_keep_days 0→14" ("actual=" + $cfg.log_keep_days)
  Check ($cfg.remote_access.trusted_proxies.Count -eq 1 -and $cfg.remote_access.trusted_proxies[0] -eq "10.0.0.1") "trusted_proxies 清洗" ($cfg.remote_access.trusted_proxies -join ",")

  $sb = @(Get-ChildItem (Join-Path $bh "state/backups") -Filter "state.v1.*.json" -ErrorAction SilentlyContinue)
  $pb = @(Get-ChildItem (Join-Path $bh "state/backups") -Filter "panel.v1.*.json" -ErrorAction SilentlyContinue)
  Check ($sb.Count -eq 1) "state v1 备份已留档" ($sb.Name -join ",")
  Check ($pb.Count -eq 1) "panel v1 备份已留档" ($pb.Name -join ",")
  if ($sb.Count -eq 1) {
    Check ((Get-FileHash $sb[0].FullName -Algorithm SHA256).Hash -eq $stateV1Hash) "state 备份是原始字节（哈希一致）"
    $bakState = Read-Json $sb[0].FullName
    Check ($bakState.schema_version -eq 1 -and $bakState.tokens[0].target -eq "clash") "备份保留 v1 原值"
  }
  if ($pb.Count -eq 1) {
    Check ((Get-FileHash $pb[0].FullName -Algorithm SHA256).Hash -eq $cfgV1Hash) "panel 备份是原始字节（哈希一致）"
  }

  $auditFiles = @(Get-ChildItem (Join-Path $bh "logs/audit") -Filter "*.jsonl")
  $auditText = ($auditFiles | ForEach-Object { Get-Content -Raw -LiteralPath $_.FullName }) -join "`n"
  Check (([regex]::Matches($auditText, '"event":"schema\.upgrade"')).Count -eq 2) "审计记录两条 schema.upgrade"

  $sess = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $setup = Call "POST" "/api/auth/setup" @{ password = $Password; confirm = $Password } -sess $sess
  $login = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess
  Check ($login.Status -eq 200) "迁移后登录成功" ("status=" + $login.Status)
  $inb = Call "GET" "/api/inbound/inbounds" -sess $sess
  Check ($inb.Status -eq 200 -and $inb.Body.Count -ge 1) "迁移后入站列表可读" ("status=" + $inb.Status)

  Stop-Panel $panel
  $stateV2Hash = (Get-FileHash (State-Path) -Algorithm SHA256).Hash

  # ---------- A2. 二次启动幂等：不再迁移、不再新增备份 ----------
  $sbBefore = Backup-Count "*.json"
  $panel = Start-Raw
  Check (Wait-Health $panel) "v2 状态二次启动成功"
  $errText2 = Read-Text $panel.ErrLog
  Check ($errText2 -notmatch "schema 升级") "二次启动不再触发迁移" $errText2.Trim()
  Check ((Backup-Count "*.json") -eq $sbBefore) "备份数量未增加" ("before=$sbBefore")
  Check ((Get-FileHash (State-Path) -Algorithm SHA256).Hash -eq $stateV2Hash) "二次启动 state 未变"
  Stop-Panel $panel

  # ---------- B. 高版本拒绝启动，原文件不动 ----------
  $future = @"
{ "schema_version": 99, "panel": { "created_at": "2026-11-01T00:00:00Z" }, "inbounds": [], "tokens": [] }
"@
  Write-Utf8 (State-Path) $future
  $futureHash = (Get-FileHash (State-Path) -Algorithm SHA256).Hash
  $panel = Start-Raw
  Check (Wait-Exit $panel 15000) "v99 状态被拒绝启动" ("exit=" + $panel.Process.ExitCode)
  Check ($panel.Process.ExitCode -ne 0) "拒绝启动返回非零退出码" ("exit=" + $panel.Process.ExitCode)
  $errText = Read-Text $panel.ErrLog
  Check ($errText -match "高于本程序支持") "错误说明版本过高" $errText.Trim()
  Check ((Get-FileHash (State-Path) -Algorithm SHA256).Hash -eq $futureHash) "v99 文件未被改写"
  Stop-Panel $panel

  # ---------- C. 损坏 JSON 不覆盖 ----------
  $broken = "{ not-json ,,,"
  Write-Utf8 (State-Path) $broken
  $brokenHash = (Get-FileHash (State-Path) -Algorithm SHA256).Hash
  $panel = Start-Raw
  Check (Wait-Exit $panel 15000) "损坏 state 被拒绝启动"
  $errText = Read-Text $panel.ErrLog
  Check ($errText -match "状态升级失败") "错误说明解析失败" $errText.Trim()
  Check ((Get-FileHash (State-Path) -Algorithm SHA256).Hash -eq $brokenHash) "损坏文件原样保留（未覆盖）"
  Stop-Panel $panel

  # ---------- D. 迁移后校验失败：保留原文件 + 留备份 ----------
  $badState = @"
{
  "schema_version": 1,
  "panel": { "created_at": "2026-10-01T00:00:00Z" },
  "inbounds": [ { "id": "inb_bad", "tag": "bad", "clients": "not-an-array" } ],
  "tokens": []
}
"@
  Write-Utf8 (State-Path) $badState
  $badHash = (Get-FileHash (State-Path) -Algorithm SHA256).Hash
  $sbBefore = Backup-Count "state.v1.*.json"
  $panel = Start-Raw
  Check (Wait-Exit $panel 15000) "迁移校验失败时拒绝启动"
  $errText = Read-Text $panel.ErrLog
  Check ($errText -match "校验失败") "错误说明校验失败" $errText.Trim()
  Check ((Get-FileHash (State-Path) -Algorithm SHA256).Hash -eq $badHash) "校验失败时原文件未改写"
  Check ((Backup-Count "state.v1.*.json") -eq ($sbBefore + 1)) "校验失败仍保留 v1 备份"
  Stop-Panel $panel

  # ---------- E. 配置高版本拒绝启动 ----------
  New-V1State
  $cfgFuture = @"
{ "schema_version": 99, "listen": "127.0.0.1:$Port", "panel_address": "", "log_keep_days": 14, "remote_access": { "enabled": false } }
"@
  Write-Utf8 (Config-Path) $cfgFuture
  $cfgFutureHash = (Get-FileHash (Config-Path) -Algorithm SHA256).Hash
  $panel = Start-Raw
  Check (Wait-Exit $panel 15000) "高版本 panel.json 被拒绝启动"
  $errText = Read-Text $panel.ErrLog
  Check ($errText -match "配置文件升级失败") "配置错误说明清楚" $errText.Trim()
  Check ((Get-FileHash (Config-Path) -Algorithm SHA256).Hash -eq $cfgFutureHash) "高版本配置文件未被改写"
}
finally {
  if ($panel) { Stop-Panel $panel }
  Write-Host "临时目录：$bh"
}
if ($script:fail -gt 0) {
  Write-Host ("M5 升级路径：{0} 项失败" -f $script:fail)
  exit 1
}
Write-Host "M5 升级路径：全部通过"
exit 0
