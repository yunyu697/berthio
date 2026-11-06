# M5：3x-ui 入站 JSON 导入（粘贴 / 文件上传 / 数组 / 备份 / 字符串化 settings）。
# 样本来自 3x-ui v3.11.0 官方镜像的 /panel/api/inbounds/list 真实导出（8 条：
# Reality、TLS+WS、trojan、坏 Reality、WS 自定义路径、无客户端、停用 Reality、gRPC）。
# 样本里的 Reality 私钥是测试专用一次性密钥，与 scripts/make-3xui-fixture.py 一致。
#
# 用法：
#   .\scripts\test-m5-json.ps1 -Exe .\target\debug\berthio.exe
param(
  [string]$Exe = "",
  [int]$Port = 18192,
  [string]$Fixture = ""
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
if ($PSVersionTable.PSVersion.Major -lt 6) {
  $pwsh = Get-Command pwsh -ErrorAction SilentlyContinue
  if ($pwsh) {
    & $pwsh.Source -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Exe $Exe -Port $Port -Fixture $Fixture
    exit $LASTEXITCODE
  }
  throw "需要 PowerShell 7+（pwsh）"
}
$script:fail = 0
$root = Split-Path -Parent $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $root "target\debug\berthio.exe" }
if (-not (Test-Path $Exe)) { throw "找不到 $Exe，先 cargo build" }
if (-not $Fixture) { $Fixture = Join-Path $PSScriptRoot "fixtures\3xui-v3-export.json" }
if (-not (Test-Path $Fixture)) { throw "找不到样本 $Fixture" }

$Base = "http://127.0.0.1:$Port"
$bh = Join-Path (Join-Path $root "tmp") ("berthio-m5-json-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$Password = "Berthio#2026"
$SecretKey = "wCmzHKJR4pef2-B7uIv60Myqv1q-1RF74KZBwDYZaFU"
New-Item -ItemType Directory -Force -Path $bh | Out-Null

function Check([bool]$cond, [string]$name, [string]$detail = "") {
  if ($cond) { Write-Host ("PASS  {0}  {1}" -f $name, $detail) }
  else { Write-Host ("FAIL  {0}  {1}" -f $name, $detail); $script:fail++ }
}

function Read-Text([string]$path) {
  $t = Get-Content -Raw -LiteralPath $path -ErrorAction SilentlyContinue
  if ($null -eq $t) { return "" }
  return $t
}

function Call([string]$method, [string]$path, $body = $null, $sess = $null, [hashtable]$headers = @{}) {
  $p = @{ Uri = ($Base + $path); Method = $method; SkipHttpErrorCheck = $true }
  if ($sess) { $p.WebSession = $sess }
  if ($null -ne $body) {
    $p.ContentType = 'application/json; charset=utf-8'
    $p.Body = [System.Text.Encoding]::UTF8.GetBytes(($body | ConvertTo-Json -Compress -Depth 30))
  }
  if ($headers.Count -gt 0) { $p.Headers = $headers }
  $r = Invoke-WebRequest @p
  $j = $null
  try { $j = $r.Content | ConvertFrom-Json } catch {}
  [pscustomobject]@{ Status = [int]$r.StatusCode; Body = $j; Raw = $r.Content }
}

function Counts($j) { "{0}/{1}/{2} (already={3})" -f $j.counts.import, $j.counts.skip, $j.counts.conflict, $j.counts.already_imported }

function Find-Item($j, [string]$remark) {
  return ($j.items | Where-Object { $_.remark -eq $remark } | Select-Object -First 1)
}

$panel = $null
try {
  $exportText = Read-Text $Fixture
  $export = $exportText | ConvertFrom-Json -AsHashtable
  $arr = @($export.obj)
  Check ($arr.Count -eq 8) "真实 v3.11.0 导出样本含 8 条入站" ("actual=" + $arr.Count)

  $env:BERTHIO_HOME = $bh
  $env:BERTHIO_LISTEN = "127.0.0.1:$Port"
  $env:BERTHIO_KERNEL_DIR = Join-Path $root ".tools\kernel"
  $env:BERTHIO_TRUST_PROXY = ""
  $panel = Start-Process -FilePath $Exe -PassThru -NoNewWindow `
    -RedirectStandardOutput (Join-Path $bh "panel.out.log") `
    -RedirectStandardError (Join-Path $bh "panel.err.log")
  for ($i = 0; $i -lt 80; $i++) {
    try { Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck -TimeoutSec 1 | Out-Null; break } catch { Start-Sleep -Milliseconds 250 }
  }
  $sess = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $null = Call "POST" "/api/auth/setup" @{ password = $Password; confirm = $Password } -sess $sess
  $login = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess
  Check ($login.Status -eq 200) "登录成功"
  $H = @{ "X-Berthio-CSRF" = $login.Body.csrf_token }

  # ---------- 1. 粘贴入口：整份真实 API 响应 ----------
  $p1 = Call "POST" "/api/migrate/preview" @{ json_text = $exportText; label = "pasted" } -sess $sess -headers $H
  Check ($p1.Status -eq 200) "粘贴入口预览成功" ("status=" + $p1.Status)
  Check ($p1.Body.source_kind -eq "json") "识别为 JSON 来源"
  Check ($p1.Body.schema.shape -eq "obj.inbounds 数组") "自动识别 API 响应形态" ("shape=" + $p1.Body.schema.shape)
  Check ($p1.Body.counts.import -eq 3 -and $p1.Body.counts.skip -eq 3 -and $p1.Body.counts.conflict -eq 2) `
    "预览计数 导入3/跳过3/冲突2" (Counts $p1.Body)
  $exportBytes = [System.Text.Encoding]::UTF8.GetByteCount($exportText)
  Check ($p1.Body.source.bytes -eq $exportBytes) "字节数与原文一致" ("bytes=" + $p1.Body.source.bytes)
  Check ($p1.Body.source.sha256.Length -eq 64) "返回 sha256（不返回原文）"
  Check (($p1.Raw -notmatch [regex]::Escape($SecretKey)) -and ($p1.Raw -notmatch "trojan-pw")) "预览响应不含私钥/密码明文"
  Check ($p1.Body.items[0].position -eq 1 -and $p1.Body.items[7].position -eq 8) "每条带 position（第几条）"

  $paused = Find-Item $p1.Body "v3-已停用-REALITY"
  Check ($paused.kind -eq "import" -and $paused.mapped.enabled -eq $false) "停用项导入并继承 enabled=false"
  $grpc = Find-Item $p1.Body "v3-gRPC-REALITY"
  Check ($grpc.kind -eq "skip" -and ($grpc.reasons -join " ") -match "grpc") "gRPC 明确跳过并说明原因" ($grpc.reasons -join " | ")
  $bad = Find-Item $p1.Body "v3-坏-REALITY-只认publicKey"
  Check ($bad.kind -eq "conflict" -and ($bad.reasons -join " ") -match "privateKey") "坏 Reality 冲突指到 privateKey"
  $ws = Find-Item $p1.Body "v3-WS-自定义路径"
  Check ($ws.kind -eq "conflict" -and ($ws.reasons -join " ") -match "path=/custom") "WS 自定义路径冲突并给出实际值"
  Check (@($p1.Body.items | Where-Object { $_.kind -eq "conflict" -and @($_.reasons).Count -eq 0 }).Count -eq 0) "所有冲突都有原因"

  # ---------- 2. 文件上传入口（前端 File.text() 后走同一接口） ----------
  $p2 = Call "POST" "/api/migrate/preview" @{ payload = $exportText; file_name = "3xui-export.json" } -sess $sess -headers $H
  Check ($p2.Status -eq 200) "上传入口预览成功" ("status=" + $p2.Status)
  Check ($p2.Body.source.label -eq "3xui-export.json") "上传入口记录文件名" ("label=" + $p2.Body.source.label)
  Check ($p2.Body.counts.import -eq 3 -and $p2.Body.counts.skip -eq 3 -and $p2.Body.counts.conflict -eq 2) `
    "上传入口计数与粘贴一致" (Counts $p2.Body)

  # ---------- 3. settings/streamSettings 是“字符串化 JSON”（库里最常见） ----------
  $strArr = @()
  foreach ($ib in $arr) {
    $copy = $ib.Clone()
    $copy["settings"] = ($ib["settings"] | ConvertTo-Json -Depth 30 -Compress)
    $copy["streamSettings"] = ($ib["streamSettings"] | ConvertTo-Json -Depth 30 -Compress)
    $strArr += $copy
  }
  $strText = @{ inbounds = $strArr } | ConvertTo-Json -Depth 30 -Compress
  $p3 = Call "POST" "/api/migrate/preview" @{ json_text = $strText; label = "settings-string" } -sess $sess -headers $H
  Check ($p3.Status -eq 200) "字符串化 settings 预览成功" ("status=" + $p3.Status)
  Check ($p3.Body.schema.shape -eq "inbounds 数组") "识别 inbounds 数组" ("shape=" + $p3.Body.schema.shape)
  Check ($p3.Body.counts.import -eq 3 -and $p3.Body.counts.skip -eq 3 -and $p3.Body.counts.conflict -eq 2) `
    "字符串化 settings 计数与对象形态一致" (Counts $p3.Body)
  $refsObj = ($p1.Body.items | Where-Object { $_.kind -eq "import" } | ForEach-Object { $_.source_ref }) -join ","
  $refsStr = ($p3.Body.items | Where-Object { $_.kind -eq "import" } | ForEach-Object { $_.source_ref }) -join ","
  Check ($refsObj -eq $refsStr) "两种存储形态的幂等键一致"
  $jp = Find-Item $p3.Body "v3-东京-REALITY"
  Check ($jp.mapped.clients[0].uuid -eq "11111111-1111-4111-8111-111111111111") "字符串化后客户端 UUID 仍正确"

  # ---------- 4. 整份备份：xray 里再套一层字符串 ----------
  $backupInner = @{ inbounds = $strArr } | ConvertTo-Json -Depth 30 -Compress
  $backupText = @{ xray = $backupInner } | ConvertTo-Json -Depth 30 -Compress
  $p4 = Call "POST" "/api/migrate/preview" @{ json_text = $backupText; label = "backup" } -sess $sess -headers $H
  Check ($p4.Status -eq 200) "嵌套备份 JSON 预览成功" ("status=" + $p4.Status)
  Check ($p4.Body.schema.shape -eq "xray.inbounds 数组") "自动识别 xray.inbounds 嵌套" ("shape=" + $p4.Body.schema.shape)
  Check ($p4.Body.counts.import -eq 3 -and $p4.Body.counts.skip -eq 3 -and $p4.Body.counts.conflict -eq 2) `
    "嵌套备份计数一致" (Counts $p4.Body)

  # ---------- 5. 单条 inbound 对象 ----------
  $one = $arr[1] | ConvertTo-Json -Depth 30
  $p5 = Call "POST" "/api/migrate/preview" @{ json_text = $one; label = "single" } -sess $sess -headers $H
  Check ($p5.Status -eq 200) "单条 inbound 对象预览成功"
  Check ($p5.Body.schema.shape -eq "单条 inbound 对象") "识别单条对象" ("shape=" + $p5.Body.schema.shape)
  Check ($p5.Body.counts.import -eq 1 -and $p5.Body.counts.skip -eq 0 -and $p5.Body.counts.conflict -eq 0) `
    "单条计数 1/0/0" (Counts $p5.Body)

  # ---------- 6. 错误定位 ----------
  $broken = '{"inbounds":[{"protocol":"vless","settings":{"privateKey":"TOPSECRET-123"}'
  $e1 = Call "POST" "/api/migrate/preview" @{ json_text = $broken } -sess $sess -headers $H
  Check ($e1.Status -eq 400) "非法 JSON 返回 400" ("status=" + $e1.Status)
  Check ($e1.Body.error.message -match "第 \d+ 行第 \d+ 列") "非法 JSON 给出行列" $e1.Body.error.message
  Check (($e1.Raw -notmatch "TOPSECRET-123") -and ($e1.Raw -match "\*\*\*")) "错误片段脱敏（不落私钥明文）" $e1.Body.error.message

  $notObj = '{"inbounds":[{"protocol":"vless","port":24990,"settings":{"clients":[{"id":"99999999-9999-4999-8999-999999999999","email":"a@demo"}]},"streamSettings":{"network":"tcp","security":"tls"}}, 42]}'
  $e2 = Call "POST" "/api/migrate/preview" @{ json_text = $notObj } -sess $sess -headers $H
  Check ($e2.Status -eq 400 -and $e2.Body.error.message -match "第 2 条不是 JSON 对象") "非对象元素指出是第几条" $e2.Body.error.message

  $missing = '{"inbounds":[{"remark":"ok","tag":"ok","port":24991,"protocol":"vless","settings":{"clients":[{"id":"99999999-9999-4999-8999-999999999999","email":"a@demo"}]},"streamSettings":{"network":"tcp","security":"tls"}},{"remark":"no-protocol","port":24992,"settings":{"clients":[{"id":"99999999-9999-4999-8999-999999999998","email":"b@demo"}]},"streamSettings":{"network":"tcp","security":"tls"}}]}'
  $e3 = Call "POST" "/api/migrate/preview" @{ json_text = $missing } -sess $sess -headers $H
  Check ($e3.Status -eq 200) "字段缺失按逐条冲突报告" ("status=" + $e3.Status)
  $missItem = $e3.Body.items[1]
  Check ($missItem.position -eq 2 -and $missItem.kind -eq "conflict" -and ($missItem.reasons -join " ") -match "protocol") `
    "字段缺失指到第 2 条 + 字段名" ($missItem.reasons -join " | ")

  $badSettings = '{"inbounds":[{"remark":"bad-settings","port":24993,"protocol":"vless","settings":"{bad","streamSettings":{"network":"tcp","security":"reality"}}]}'
  $e4 = Call "POST" "/api/migrate/preview" @{ json_text = $badSettings } -sess $sess -headers $H
  $bsItem = $e4.Body.items[0]
  Check ($e4.Status -eq 200 -and ($bsItem.reasons -join " ") -match "settings 解析失败") "字符串化 settings 非法时指到字段名" ($bsItem.reasons -join " | ")
  Check (($bsItem.reasons -join " ") -match "第 1 行第 \d+ 列") "字符串内 JSON 非法也带行列"

  # ---------- 7. 应用 + 幂等 ----------
  $pre = Call "POST" "/api/migrate/preview" @{ json_text = $exportText } -sess $sess -headers $H
  $ap = Call "POST" "/api/migrate/apply" @{ json_text = $exportText; preview_id = $pre.Body.preview_id; plan_hash = $pre.Body.plan_hash; confirm = $true } -sess $sess -headers $H
  Check ($ap.Status -eq 200 -and $ap.Body.imported -eq 3) "apply 写入 3 条" ("status=" + $ap.Status + " imported=" + $ap.Body.imported)
  Check ($ap.Body.counts.skip -eq 3 -and $ap.Body.counts.conflict -eq 2) "apply 复用同一份计数" (Counts $ap.Body)

  $list = Call "GET" "/api/inbound/inbounds" -sess $sess
  Check ($list.Body.items.Count -eq 3) "入站总数 3" ("actual=" + $list.Body.items.Count)
  $pausedInb = $list.Body.items | Where-Object { $_.remark -eq "v3-已停用-REALITY" } | Select-Object -First 1
  Check ($pausedInb.enabled -eq $false) "停用状态已落地"
  Check ($list.Raw -notmatch [regex]::Escape($SecretKey)) "入站列表不返回私钥明文"

  $pre2 = Call "POST" "/api/migrate/preview" @{ json_text = $exportText } -sess $sess -headers $H
  Check ($pre2.Body.counts.import -eq 0 -and $pre2.Body.counts.already_imported -eq 3) `
    "重复粘贴全部命中幂等键" (Counts $pre2.Body)
  $ap2 = Call "POST" "/api/migrate/apply" @{ json_text = $exportText; preview_id = $pre2.Body.preview_id; plan_hash = $pre2.Body.plan_hash; confirm = $true } -sess $sess -headers $H
  Check ($ap2.Status -eq 200 -and $ap2.Body.imported -eq 0) "第二次 apply 不新增" ("imported=" + $ap2.Body.imported)
  $list2 = Call "GET" "/api/inbound/inbounds" -sess $sess
  Check ($list2.Body.items.Count -eq 3) "第二次 apply 后总数仍为 3"

  $stale = Call "POST" "/api/migrate/apply" @{ json_text = $exportText; preview_id = $pre2.Body.preview_id; plan_hash = ("0" * 64); confirm = $true } -sess $sess -headers $H
  Check ($stale.Status -eq 409 -and $stale.Body.error.code -eq "plan_changed") "陈旧 plan_hash 被 409 拒绝"
  $noConfirm = Call "POST" "/api/migrate/apply" @{ json_text = $exportText; preview_id = $pre2.Body.preview_id; plan_hash = $pre2.Body.plan_hash; confirm = $false } -sess $sess -headers $H
  Check ($noConfirm.Status -eq 400) "未确认不写入"

  # ---------- 8. 不落盘、不泄密 ----------
  Check (-not (Test-Path (Join-Path $bh "tmp/migrate"))) "JSON 路径不生成 tmp/migrate"
  $tmpFiles = @(Get-ChildItem (Join-Path $bh "tmp") -Recurse -File -ErrorAction SilentlyContinue)
  Check ($tmpFiles.Count -eq 0) "tmp 目录没有残留文件" ("files=" + $tmpFiles.Count)
  $auditFiles = @(Get-ChildItem (Join-Path $bh "logs/audit") -Filter "*.jsonl")
  $auditText = ($auditFiles | ForEach-Object { Read-Text $_.FullName }) -join "`n"
  Check ($auditText -match '"event":"migrate\.apply"') "审计记录了 apply"
  Check (($auditText -notmatch [regex]::Escape($SecretKey)) -and ($auditText -notmatch "trojan-pw") -and ($auditText -notmatch "TOPSECRET")) "审计与日志无明文密钥"
  Check ($auditText -notmatch "streamSettings") "审计不记录原文"
}
finally {
  if ($panel -and -not $panel.HasExited) { Stop-Process -Id $panel.Id -Force -ErrorAction SilentlyContinue }
  Write-Host "临时目录：$bh"
}
if ($script:fail -gt 0) {
  Write-Host ("M5 JSON 导入：{0} 项失败" -f $script:fail)
  exit 1
}
Write-Host "M5 JSON 导入：全部通过"
exit 0
