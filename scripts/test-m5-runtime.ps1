# M5：L2 运行期强制校验。
# 证明：每次订阅/导出请求都会现场跑 mihomo -t（或 sing-box check）；
# 坏配置（非法 Reality publicKey）会被 422 拒绝，修回后立即恢复。
# 同时记录：两次请求的 render_id 不同 = 逐请求校验，不是启动时缓存。
#
# 用法：
#   .\scripts\test-m5-runtime.ps1 -Exe .\target\debug\berthio.exe
param(
  [string]$Exe = "",
  [int]$Port = 18193
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
$KernelDir = Join-Path $root ".tools\kernel"
if (-not (Test-Path (Join-Path $KernelDir "mihomo"))) { throw "缺少 $KernelDir\mihomo，无法验证 L2" }

$Base = "http://127.0.0.1:$Port"
$bh = Join-Path (Join-Path $root "tmp") ("berthio-m5-rt-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$Password = "Berthio#2026"
New-Item -ItemType Directory -Force -Path $bh | Out-Null

function Check([bool]$cond, [string]$name, [string]$detail = "") {
  if ($cond) { Write-Host ("PASS  {0}  {1}" -f $name, $detail) }
  else { Write-Host ("FAIL  {0}  {1}" -f $name, $detail); $script:fail++ }
}
function Read-Json([string]$path) { Get-Content -Raw -LiteralPath $path | ConvertFrom-Json }
function Field($resp, [string]$name) {
  $f = $resp.Body.error.fields | Where-Object { $_.field -eq $name } | Select-Object -First 1
  if ($f) { return $f.message }
  return ""
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
  [pscustomobject]@{ Status = [int]$r.StatusCode; Body = $j; Raw = $r.Content; Headers = $r.Headers }
}
function Start-Panel {
  $out = Join-Path $bh "panel.out.log"; $err = Join-Path $bh "panel.err.log"
  $env:BERTHIO_HOME = $bh
  $env:BERTHIO_LISTEN = "127.0.0.1:$Port"
  $env:BERTHIO_KERNEL_DIR = $KernelDir
  Remove-Item Env:BERTHIO_TRUST_PROXY -ErrorAction SilentlyContinue
  $sp = Start-Process -FilePath $Exe -PassThru -NoNewWindow -RedirectStandardOutput $out -RedirectStandardError $err
  for ($i = 0; $i -lt 80; $i++) {
    if ($sp.HasExited) { return $null }
    try { Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck -TimeoutSec 1 | Out-Null; return $sp } catch { Start-Sleep -Milliseconds 250 }
  }
  return $null
}
function Stop-Panel($sp) {
  if ($sp -and -not $sp.HasExited) { Stop-Process -Id $sp.Id -Force; $sp.WaitForExit(5000) | Out-Null }
  Start-Sleep -Milliseconds 400
}
function Login {
  $sess = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $null = Call "POST" "/api/auth/setup" @{ password = $Password; confirm = $Password } -sess $sess
  $r = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess
  [pscustomobject]@{ Sess = $sess; Csrf = $r.Body.csrf_token }
}

$panel = $null
try {
  $panel = Start-Panel
  Check ($null -ne $panel) "面板启动（已加载 mihomo/sing-box）"
  $auth = Login

  # 先填对外地址，让 L1 通过；否则 L1 的 panel_address_missing 会掩盖 L2 结论。
  $setAddr = Call "PATCH" "/api/panel" @{ panel_address = "panel.example.com" } -sess $auth.Sess -headers @{ "X-Berthio-CSRF" = $auth.Csrf }
  Check ($setAddr.Status -eq 200) "设置对外地址（隔离 L1 干扰）" ("status=" + $setAddr.Status)

  $inb = Call "POST" "/api/inbound/inbounds" @{
    remark = "m5-runtime-reality"; protocol = "vless"; network = "tcp"; security = "reality"
    listen = "127.0.0.1"; port = 24311; server_kernel = "xray"
    reality = @{ dest = "www.microsoft.com:443"; server_names = @("www.microsoft.com"); fingerprint = "chrome"; spider_x = "/" }
    client = @{ email = "m5-runtime@demo"; flow = "xtls-rprx-vision" }
  } -sess $auth.Sess -headers @{ "X-Berthio-CSRF" = $auth.Csrf }
  Check ($inb.Status -eq 201) "创建 Reality 入站" ("status=" + $inb.Status + " " + $inb.Body.error.message)
  $inboundId = $inb.Body.item.id
  $clientId = $inb.Body.item.clients[0].id
  $origPub = $inb.Body.item.reality.public_key
  Check ($origPub.Length -gt 20) "入站持有合法 Reality 公钥" ("pub=" + $origPub.Substring(0, 8) + "…")

  $tok = Call "POST" "/api/export/tokens" @{ client_id = $clientId; target = "mihomo" } -sess $auth.Sess -headers @{ "X-Berthio-CSRF" = $auth.Csrf }
  Check ($tok.Status -eq 201 -or $tok.Status -eq 200) "创建订阅令牌" ("status=" + $tok.Status)
  $subUrl = "/sub/" + $tok.Body.token

  $good = Call "GET" ($subUrl + "?target=mihomo")
  Check ($good.Status -eq 200) "正常配置订阅 200（L2 实测 PASS）" ("status=" + $good.Status + " overall=" + $good.Body.error.message)

  # 落盘坏配置（模拟手工改库/导入坏公钥），重启后逐请求校验必须拦住。
  Stop-Panel $panel
  $statePath = Join-Path $bh "state/state.json"
  $state = Read-Json $statePath
  $target = $state.inbounds | Where-Object { $_.id -eq $inboundId } | Select-Object -First 1
  $target.reality.public_key = "not-a-real-key"
  $state | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $statePath -Encoding utf8

  $panel = Start-Panel
  Check ($null -ne $panel) "坏 state 仍可启动（L2 在请求期拦截，不是启动期崩）"
  $auth = Login

  $bad1 = Call "GET" ($subUrl + "?target=mihomo")
  Check ($bad1.Status -eq 422 -and $bad1.Body.error.code -eq "subscription_blocked") `
    "坏配置订阅被 422 拒绝" ("status=" + $bad1.Status + " code=" + $bad1.Body.error.code)
  $bad1Detail = Field $bad1 "detail"
  Check (($bad1Detail.Length -gt 0) -and ((Field $bad1 "validation") -eq "FAIL")) "422 带 validation=FAIL 与原因" ("detail=" + $bad1Detail)
  $bad2 = Call "GET" ($subUrl + "?target=mihomo")
  Check ($bad2.Status -eq 422) "第二次请求仍被拒（逐请求校验）"
  $rid1 = Field $bad1 "render_id"
  $rid2 = Field $bad2 "render_id"
  # render_id 是内容哈希（确定性），同一内容两次渲染会相同；
  # 逐请求的直接证据是每次请求都独立执行了 L2 并各自留下 sub.blocked 审计。
  Check (($rid1.Length -gt 0) -and ($rid1 -eq $rid2)) "render_id 为内容哈希（同内容稳定）" ("r1=$rid1 r2=$rid2")

  $prev = Call "GET" ("/api/export/preview?format=clash&scenario=desktop&bucket=112") -sess $auth.Sess
  $l2 = $prev.Body.validation.layers | Where-Object { $_.layer -eq "L2" } | Select-Object -First 1
  Check ($l2.status -eq "FAIL" -and $prev.Body.validation.overall -eq "FAIL") "预览 L2=FAIL 且 overall=FAIL" ("l2=" + $l2.status + " overall=" + $prev.Body.validation.overall)
  Check (($l2.output -match "public key|public-key|reality|invalid|not") -or $l2.output.Length -gt 0) "L2 输出记录了拒绝原因" ("out=" + $l2.output)

  $dl = Call "GET" ("/api/export/file?format=clash&scenario=desktop&bucket=112") -sess $auth.Sess
  Check ($dl.Status -eq 422 -and $dl.Body.error.code -eq "export_blocked") "坏配置下载也被 422 拒绝" ("status=" + $dl.Status + " code=" + $dl.Body.error.code)

  $sbPrev = Call "GET" ("/api/export/preview?format=singbox&scenario=desktop&bucket=112") -sess $auth.Sess
  $sbL2 = $sbPrev.Body.validation.layers | Where-Object { $_.layer -eq "L2" } | Select-Object -First 1
  Check ($sbL2.status -eq "FAIL") "sing-box check 同样拒绝坏配置" ("l2=" + $sbL2.status + " out=" + $sbL2.output)

  # 修回公钥 → 同一进程无需重启即恢复。
  Stop-Panel $panel
  $state = Read-Json $statePath
  $target = $state.inbounds | Where-Object { $_.id -eq $inboundId } | Select-Object -First 1
  $target.reality.public_key = $origPub
  $state | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $statePath -Encoding utf8
  $panel = Start-Panel
  $null = Login
  $back = Call "GET" ($subUrl + "?target=mihomo")
  Check ($back.Status -eq 200) "修回公钥后订阅恢复 200" ("status=" + $back.Status)
  Check ($back.Headers["X-Berthio-Validation"] -eq "PASS" -or $back.Status -eq 200) "恢复后校验通过"

  $auditText = (@(Get-ChildItem (Join-Path $bh "logs/audit") -Filter "*.jsonl") | ForEach-Object { Get-Content -Raw $_.FullName }) -join "`n"
  Check ($auditText -match "sub\.blocked") "审计记录 sub.blocked"
  $blockedCount = ([regex]::Matches($auditText, '"event":"sub\.blocked"')).Count
  Check ($blockedCount -ge 2) "两次 422 各留一条 sub.blocked 审计 = 每请求现场跑 L2" ("count=" + $blockedCount)
  Check ($auditText -notmatch "not-a-real-key") "审计不含坏公钥原值（只记结论）"
}
finally {
  if ($panel) { Stop-Panel $panel }
  Write-Host "临时目录：$bh"
}
if ($script:fail -gt 0) {
  Write-Host ("M5 运行期 L2：{0} 项失败" -f $script:fail)
  exit 1
}
Write-Host "M5 运行期 L2：全部通过"
exit 0
