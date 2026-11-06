# M4 回归：3x-ui 只读迁移 + 订阅链路（真实二进制校验）。
# 脚本自建临时 HOME、临时端口、样本 SQLite 库，不触碰任何真实面板数据。
#
# 用法：
#   .\scripts\test-m4-migrate.ps1 -Exe .\target\debug\berthio.exe
param(
  [string]$Exe = "",
  [int]$Port = 18184,
  [string]$KernelDir = ""
)
$ErrorActionPreference = "Stop"
if ($PSVersionTable.PSVersion.Major -lt 6) {
  $pwsh = Get-Command pwsh -ErrorAction SilentlyContinue
  if ($pwsh) {
    & $pwsh.Source -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Exe $Exe -Port $Port -KernelDir $KernelDir
    exit $LASTEXITCODE
  }
  throw "需要 PowerShell 7+（pwsh）"
}
$script:fail = 0
$root = Split-Path -Parent $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $root "target\debug\berthio.exe" }
if (-not (Test-Path $Exe)) { throw "找不到 $Exe，先 cargo build" }
if (-not $KernelDir) { $KernelDir = Join-Path $root ".tools\kernel" }
$Mihomo = Join-Path $KernelDir "mihomo\1.19.33\mihomo.exe"
$SingBox = Join-Path $KernelDir "sing-box\1.14.4\sing-box.exe"
$Xray = Join-Path $KernelDir "xray\25.6.8\xray.exe"

$Base = "http://127.0.0.1:$Port"
$bh = Join-Path $env:TEMP ("berthio-m4-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Force -Path $bh | Out-Null
$fixtures = Join-Path $bh "fixtures"
$Password = "Berthio#2026"
$PRIV = "wCmzHKJR4pef2-B7uIv60Myqv1q-1RF74KZBwDYZaFU"

function Check([bool]$cond, [string]$name, [string]$detail = "") {
  if ($cond) { Write-Host ("PASS  {0}  {1}" -f $name, $detail) }
  else { Write-Host ("FAIL  {0}  {1}" -f $name, $detail); $script:fail++ }
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

function CallSub([string]$token, [string]$query = "", [string]$ua = "", [hashtable]$headers = @{}) {
  $p = @{ Uri = ($Base + "/sub/" + $token + $query); SkipHttpErrorCheck = $true }
  if ($ua) { $p.UserAgent = $ua }
  if ($headers.Count -gt 0) { $p.Headers = $headers }
  $r = Invoke-WebRequest @p
  $j = $null
  try { $j = $r.Content | ConvertFrom-Json } catch {}
  [pscustomobject]@{ Status = [int]$r.StatusCode; Body = $j; Raw = $r.Content; Headers = $r.Headers }
}

function Write-Utf8NoBom([string]$path, [string]$text) {
  [System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding($false)))
}

function DeepCopy($obj) {
  return ($obj | ConvertTo-Json -Depth 12 | ConvertFrom-Json -AsHashtable)
}

function Start-Panel {
  $probe = $null
  try { $probe = Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck -TimeoutSec 1 } catch {}
  if ($probe) { throw "端口 $Port 已被占用，换 -Port" }
  $env:BERTHIO_HOME = $bh
  $env:BERTHIO_LISTEN = "127.0.0.1:$Port"
  $env:BERTHIO_KERNEL_DIR = $KernelDir
  $env:BERTHIO_TRUST_PROXY = ""
  $sp = Start-Process -FilePath $Exe -PassThru -NoNewWindow `
    -RedirectStandardOutput (Join-Path $bh "panel.out.log") `
    -RedirectStandardError (Join-Path $bh "panel.err.log")
  $script:panel = $sp
  for ($i = 0; $i -lt 80; $i++) {
    try { Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck -TimeoutSec 1 | Out-Null; return $sp } catch { Start-Sleep -Milliseconds 250 }
  }
  throw "面板未在预期时间内启动（$Base），见 $bh\panel.err.log"
}

Write-Host "== M4 迁移 / 订阅回归 @ $Base（HOME=$bh） =="
$panel = $null
try {
  python (Join-Path $PSScriptRoot "make-3xui-fixture.py") $fixtures | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "样本库生成失败" }
  Check (Test-Path (Join-Path $fixtures "v2.db")) "样本库已生成"
  Check (Test-Path (Join-Path $fixtures "v2-wal.db-wal")) "WAL 样本保留了 -wal 文件"

  $panel = Start-Panel
  $sess = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $null = Call "POST" "/api/auth/setup" @{ password = $Password; confirm = $Password } -sess $sess
  $login = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess
  Check ($login.Status -eq 200) "登录成功"
  $csrf = $login.Body.csrf_token
  $H = @{ "X-Berthio-CSRF" = $csrf }

  $v2 = Join-Path $fixtures "v2.db"
  $old = Join-Path $fixtures "old.db"
  $wal = Join-Path $fixtures "v2-wal.db"
  $collision = Join-Path $fixtures "v2-collision.db"

  $shaV2Before = (Get-FileHash $v2 -Algorithm SHA256).Hash
  $shaOldBefore = (Get-FileHash $old -Algorithm SHA256).Hash
  $shaWalBefore = (Get-FileHash $wal -Algorithm SHA256).Hash
  $shaWalLogBefore = (Get-FileHash ($wal + "-wal") -Algorithm SHA256).Hash

  # ---------- 旧版 x-ui：schema 探测 + 只读预览 ----------
  $pOld = Call "POST" "/api/migrate/preview" @{ db_path = $old } -sess $sess -headers $H
  Check ($pOld.Status -eq 200) "旧版 x-ui 预览成功" ("status=" + $pOld.Status)
  Check ($pOld.Body.schema.flavor -match "x-ui") "旧版 flavor 识别正确" ("flavor=" + $pOld.Body.schema.flavor)
  Check ($pOld.Body.schema.has_client_traffics -eq $false) "旧版无 client_traffics"
  Check ($pOld.Body.counts.import -eq 1 -and $pOld.Body.counts.skip -eq 1) "旧版分类：1 导入 / 1 跳过" (($pOld.Body.counts | ConvertTo-Json -Compress))
  Check ($pOld.Body.source.snapshot_strategy -match "vacuum_into_readonly") "干净库走只读 VACUUM INTO" $pOld.Body.source.snapshot_strategy
  Check ($pOld.Body.source.sha256 -eq $shaOldBefore.ToLower()) "旧版源库哈希未变（预览前后一致）" ("sha=" + $pOld.Body.source.sha256)

  # ---------- WAL 库：绝不打开源库，文件拷贝快照 ----------
  $pWal = Call "POST" "/api/migrate/preview" @{ db_path = $wal } -sess $sess -headers $H
  Check ($pWal.Status -eq 200) "WAL 库预览成功" ("status=" + $pWal.Status)
  Check ($pWal.Body.source.wal_present -eq $true -and $pWal.Body.source.wal_bytes -gt 0) "识别到 -wal" ("wal=" + $pWal.Body.source.wal_bytes)
  Check ($pWal.Body.source.snapshot_strategy -match "file_copy") "WAL 库使用文件拷贝策略（不打开源库）" $pWal.Body.source.snapshot_strategy
  Check ($pWal.Body.counts.import -eq 1) "WAL 样本可导入 1 条" (($pWal.Body.counts | ConvertTo-Json -Compress))
  Check ((Get-FileHash $wal -Algorithm SHA256).Hash -eq $shaWalBefore) "WAL 源库未变"
  Check ((Get-FileHash ($wal + "-wal") -Algorithm SHA256).Hash -eq $shaWalLogBefore) "WAL 源库 -wal 未变"

  # ---------- 3x-ui v2：完整 diff ----------
  $pV2 = Call "POST" "/api/migrate/preview" @{ db_path = $v2 } -sess $sess -headers $H
  Check ($pV2.Status -eq 200) "v2 预览成功" ("status=" + $pV2.Status)
  Check ($pV2.Body.schema.flavor -eq "3x-ui v2.x") "v2 flavor 识别正确" $pV2.Body.schema.flavor
  Check ($pV2.Body.schema.has_client_traffics -eq $true) "v2 有 client_traffics"
  Check ($pV2.Body.counts.import -eq 3 -and $pV2.Body.counts.skip -eq 5 -and $pV2.Body.counts.conflict -eq 4) "v2 分类：导入 3 / 跳过 5 / 冲突 4" (($pV2.Body.counts | ConvertTo-Json -Compress))
  Check ($pV2.Body.counts.source_rows -eq 12) "共读取 12 条源入站" ("rows=" + $pV2.Body.counts.source_rows)
  Check ((Get-FileHash $v2 -Algorithm SHA256).Hash -eq $shaV2Before) "v2 源库哈希未变"
  Check ($pV2.Body.source.modified_during_capture -eq $false) "抓取期间无并发写"

  $importItems = @($pV2.Body.items | Where-Object { $_.kind -eq "import" })
  $io = @($importItems | Where-Object { $_.source_id -eq 1 })[0]
  Check ($io.mapped.clients.Count -eq 2) "Reality 入站继承 2 个客户端"
  Check ($io.mapped.clients[0].quota_bytes -eq (10 * 1024 * 1024 * 1024)) "totalGB 按字节继承（10485760 KB → 10 GiB）" ("quota=" + $io.mapped.clients[0].quota_bytes)
  Check ([bool]$io.mapped.clients[0].expires_at) "毫秒 expiryTime 转为 ISO 时间" $io.mapped.clients[0].expires_at
  Check ($io.mapped.reality.public_key -eq "qiOGSbhAOlBTvto_J5F05B8ELVjqqrmoq1lpyFK0gBs") "Reality publicKey 继承"
  Check ($io.private_key_present -eq $true) "Reality privateKey 存在标记"
  Check (-not $pV2.Raw.Contains($PRIV)) "预览响应不含 Reality privateKey 明文"
  Check ($io.mapped.source -eq "3x-ui" -and $io.mapped.source_ref.StartsWith("3xui:")) "写入 source_ref 供幂等"

  $skipTrojan = @($pV2.Body.items | Where-Object { $_.source_id -eq 3 })[0]
  Check ($skipTrojan.kind -eq "skip" -and ($skipTrojan.reasons -join " ").Contains("trojan")) "trojan 明确跳过并说明原因"
  $conflictPriv = @($pV2.Body.items | Where-Object { $_.source_id -eq 4 })[0]
  Check ($conflictPriv.kind -eq "conflict" -and ($conflictPriv.reasons -join " ").Contains("privateKey")) "缺 privateKey 判冲突" ($conflictPriv.reasons -join " | ")
  $conflictPort = @($pV2.Body.items | Where-Object { $_.source_id -eq 5 })[0]
  Check ($conflictPort.kind -eq "conflict" -and ($conflictPort.reasons -join " ").Contains("端口 443")) "同库端口重复判冲突"
  $conflictFb = @($pV2.Body.items | Where-Object { $_.source_id -eq 6 })[0]
  Check ($conflictFb.kind -eq "conflict" -and ($conflictFb.reasons -join " ").Contains("fallbacks")) "fallbacks 判冲突"
  $skipPlain = @($pV2.Body.items | Where-Object { $_.source_id -eq 8 })[0]
  Check ($skipPlain.kind -eq "skip" -and ($skipPlain.reasons -join " ").Contains("security=none")) "security=none 跳过"
  $skipGrpc = @($pV2.Body.items | Where-Object { $_.source_id -eq 11 })[0]
  Check ($skipGrpc.kind -eq "skip" -and ($skipGrpc.reasons -join " ").Contains("grpc")) "grpc 跳过"

  # ---------- 先预览后落地 ----------
  $previewId = $pV2.Body.preview_id
  $planHash = $pV2.Body.plan_hash
  $noConfirm = Call "POST" "/api/migrate/apply" @{ db_path = $v2; preview_id = $previewId; plan_hash = $planHash } -sess $sess -headers $H
  Check ($noConfirm.Status -eq 400) "未 confirm=true 拒绝写入" ("status=" + $noConfirm.Status)
  $bogus = Call "POST" "/api/migrate/apply" @{ db_path = $v2; preview_id = $previewId; plan_hash = "deadbeef"; confirm = $true } -sess $sess -headers $H
  Check ($bogus.Status -eq 409 -and $bogus.Body.error.code -eq "plan_changed") "plan_hash 不符 → 409，不写入" ("status=" + $bogus.Status)
  $list0 = Call "GET" "/api/inbound/inbounds" -sess $sess
  Check ($list0.Body.total -eq 0) "409 后仍为 0 条入站"

  $apply = Call "POST" "/api/migrate/apply" @{ db_path = $v2; preview_id = $previewId; plan_hash = $planHash; confirm = $true } -sess $sess -headers $H
  Check ($apply.Status -eq 200 -and $apply.Body.imported -eq 3) "确认后导入 3 条" ("status=" + $apply.Status + " imported=" + $apply.Body.imported)
  Check ((Get-FileHash $v2 -Algorithm SHA256).Hash -eq $shaV2Before) "apply 后 v2 源库仍未被修改"
  $list1 = Call "GET" "/api/inbound/inbounds" -sess $sess
  Check ($list1.Body.total -eq 3) "Berthio 状态新增 3 条入站" ("total=" + $list1.Body.total)
  $tags = @($list1.Body.items | ForEach-Object { $_.tag })
  Check ($tags -contains "jp-reality" -and $tags -contains "sg-tls" -and $tags -contains "paused-reality") "导入的 tag 正确" ($tags -join ",")
  $paused = @($list1.Body.items | Where-Object { $_.tag -eq "paused-reality" })[0]
  Check ($paused.enabled -eq $false) "源端停用状态被继承"

  # ---------- 幂等 ----------
  $pAgain = Call "POST" "/api/migrate/preview" @{ db_path = $v2 } -sess $sess -headers $H
  Check ($pAgain.Status -eq 200 -and $pAgain.Body.counts.import -eq 0 -and $pAgain.Body.counts.already_imported -eq 3) "重复预览：0 新导入 / 3 条幂等命中" (($pAgain.Body.counts | ConvertTo-Json -Compress))
  $applyAgain = Call "POST" "/api/migrate/apply" @{
    db_path = $v2; preview_id = $pAgain.Body.preview_id; plan_hash = $pAgain.Body.plan_hash; confirm = $true
  } -sess $sess -headers $H
  Check ($applyAgain.Status -eq 200 -and $applyAgain.Body.imported -eq 0) "重复 apply 不产生重复入站" ("imported=" + $applyAgain.Body.imported)
  $list2 = Call "GET" "/api/inbound/inbounds" -sess $sess
  Check ($list2.Body.total -eq 3) "幂等后入站总数仍为 3" ("total=" + $list2.Body.total)

  # ---------- 与 Berthio 现有状态的 tag 冲突 ----------
  $pCol = Call "POST" "/api/migrate/preview" @{ db_path = $collision } -sess $sess -headers $H
  $colItem = @($pCol.Body.items)[0]
  Check ($pCol.Status -eq 200 -and $colItem.kind -eq "conflict" -and ($colItem.reasons -join " ").Contains("jp-reality")) "已存在的 tag 判冲突" ($colItem.reasons -join " | ")

  # ---------- 订阅链路 ----------
  # 订阅节点地址来自面板设置；未配置时导出会被 L0 拦截（server 为空），这是预期行为。
  $patch = Call "PATCH" "/api/panel" @{ panel_address = "panel.example.com" } -sess $sess -headers $H
  Check ($patch.Status -eq 200) "设置面板公网地址（订阅节点 server）"

  $jp = @($list2.Body.items | Where-Object { $_.tag -eq "jp-reality" })[0]
  $clients = Call "GET" ("/api/inbound/inbounds/" + $jp.id + "/clients") -sess $sess
  $clientId = @($clients.Body.items)[0].id
  Check ([bool]$clientId) "取得导入客户端的 id"

  $mk = Call "POST" "/api/export/tokens" @{ client_id = $clientId; target = "mihomo" } -sess $sess -headers $H
  Check ($mk.Status -eq 201 -and $mk.Body.token.StartsWith("bto_")) "创建 mihomo token"
  $t1 = $mk.Body.token

  $yaml = CallSub $t1 "" "mihomo/1.19.33"
  Check ($yaml.Status -eq 200 -and $yaml.Raw.Contains("proxies:")) "mihomo 订阅返回 YAML" ("status=" + $yaml.Status)
  Check ($yaml.Raw.Contains("qiOGSbhAOlBTvto_J5F05B8ELVjqqrmoq1lpyFK0gBs")) "mihomo 输出含 Reality publicKey"
  Check ($yaml.Raw.Contains("panel.example.com")) "mihomo 输出含面板地址"
  Check ($yaml.Raw.Contains("11111111-1111-4111-8111-111111111111")) "mihomo 输出含客户端 UUID"
  Check ($yaml.Headers["X-Berthio-Target"] -contains "mihomo") "target 头为 mihomo"
  $val = ($yaml.Headers["X-Berthio-Validation"] -join "")
  Check ($val -eq "PASS" -or $val -eq "WARN") "订阅通过 L0/L1（L2 见下）" ("validation=" + $val)

  $sb = CallSub $t1 "" "sing-box/1.14.4"
  Check ($sb.Status -eq 200) "UA=sing-box 自动选择 sing-box" ("target=" + ($sb.Headers["X-Berthio-Target"] -join ""))
  Check ($sb.Headers["X-Berthio-Target"] -contains "sing-box") "UA 推断 target=sing-box"
  $sbBody = $sb.Body
  Check ($null -ne $sbBody.outbounds) "sing-box 输出是合法 JSON"
  Check ($sb.Raw.Contains("qiOGSbhAOlBTvto_J5F05B8ELVjqqrmoq1lpyFK0gBs")) "sing-box 输出含 Reality publicKey"
  Check ($sb.Raw.Contains("11111111-1111-4111-8111-111111111111")) "sing-box 输出含客户端 UUID"

  $sbQ = CallSub $t1 "?target=sing-box"
  Check ($sbQ.Status -eq 200) "显式 target=sing-box 通过"
  $badTarget = CallSub $t1 "?target=v2ray"
  Check ($badTarget.Status -eq 400) "非法 target 400" ("status=" + $badTarget.Status)

  $missing = CallSub ""
  Check ($missing.Status -eq 401) "缺少 token → 401" ("status=" + $missing.Status)
  $wrong = CallSub "bto_000000000000000000000000000000000000000000000000"
  Check ($wrong.Status -eq 401 -and $wrong.Body.error.code -eq "subscription_unauthorized") "无效 token → 401" ("status=" + $wrong.Status)

  # 真实二进制校验
  $yamlPath = Join-Path $bh "sub.mihomo.yaml"
  Write-Utf8NoBom $yamlPath $yaml.Raw
  & $Mihomo -t -f $yamlPath *> (Join-Path $bh "mihomo-check.log")
  $mihomoOk = ($LASTEXITCODE -eq 0)
  Check $mihomoOk "mihomo 1.19.33 -t 真实通过" ("exit=" + $LASTEXITCODE)
  Check (Test-Path $Mihomo) "使用真实 mihomo 1.19.33 二进制" $Mihomo

  $jsonPath = Join-Path $bh "sub.singbox.json"
  Write-Utf8NoBom $jsonPath $sbQ.Raw
  & $SingBox check -c $jsonPath *> (Join-Path $bh "singbox-check.log")
  $singboxOk = ($LASTEXITCODE -eq 0)
  Check $singboxOk "sing-box 1.14.4 check -c 真实通过" ("exit=" + $LASTEXITCODE)
  if (-not $singboxOk) { Get-Content (Join-Path $bh "singbox-check.log") -Tail 20 }
  if (-not $mihomoOk) { Get-Content (Join-Path $bh "mihomo-check.log") -Tail 20 }

  # ---------- 轮换 / 吊销 ----------
  $rot = Call "POST" ("/api/export/tokens/" + $mk.Body.id + "/rotate") $null -sess $sess -headers $H
  Check ($rot.Status -eq 200 -and $rot.Body.token -ne $t1) "token 轮换返回新明文"
  $oldAfterRotate = CallSub $t1
  Check ($oldAfterRotate.Status -eq 401) "旧 token 轮换后立即 401" ("status=" + $oldAfterRotate.Status)
  $newAfterRotate = CallSub $rot.Body.token
  Check ($newAfterRotate.Status -eq 200) "新 token 可用"

  $rev = Call "POST" ("/api/export/tokens/" + $mk.Body.id + "/revoke") $null -sess $sess -headers $H
  Check ($rev.Status -eq 200) "吊销成功"
  $revokedGet = CallSub $rot.Body.token
  Check ($revokedGet.Status -eq 403 -and $revokedGet.Body.error.code -eq "revoked") "吊销后立即 403" ("status=" + $revokedGet.Status)
  $rotateRevoked = Call "POST" ("/api/export/tokens/" + $mk.Body.id + "/rotate") $null -sess $sess -headers $H
  Check ($rotateRevoked.Status -eq 409) "已吊销 token 不能轮换" ("status=" + $rotateRevoked.Status)

  # 服务端只存哈希
  $stateRaw = Get-Content (Join-Path $bh "state\state.json") -Raw
  Check (-not $stateRaw.Contains($t1) -and -not $stateRaw.Contains($rot.Body.token)) "state.json 不含 token 明文"
  Check ($stateRaw.Contains("token_hash")) "state.json 只存 token_hash"

  # ---------- xray Reality 只认 privateKey（实测，不是照抄旧结论）----------
  if (Test-Path $Xray) {
    $uuid = "11111111-1111-4111-8111-111111111111"
    $base = @{
      log = @{ loglevel = "warning" }
      inbounds = @(@{
        listen = "127.0.0.1"; port = 24443; protocol = "vless"
        settings = @{ clients = @(@{ id = $uuid; flow = "xtls-rprx-vision" }); decryption = "none" }
        streamSettings = @{
          network = "tcp"; security = "reality"
          realitySettings = @{
            show = $false; dest = "www.microsoft.com:443"; xver = 0
            serverNames = @("www.microsoft.com"); shortIds = @("0123456789abcdef")
          }
        }
      })
      outbounds = @(@{ protocol = "freedom" })
    }
    $withPriv = DeepCopy $base
    $withPriv.inbounds[0].streamSettings.realitySettings.privateKey = $PRIV
    $onlyPub = DeepCopy $base
    $onlyPub.inbounds[0].streamSettings.realitySettings.settings = @{ publicKey = "qiOGSbhAOlBTvto_J5F05B8ELVjqqrmoq1lpyFK0gBs"; fingerprint = "chrome"; spiderX = "/" }
    $cfg1 = Join-Path $bh "xray-with-priv.json"
    $cfg2 = Join-Path $bh "xray-only-pub.json"
    Write-Utf8NoBom $cfg1 ($withPriv | ConvertTo-Json -Depth 12)
    Write-Utf8NoBom $cfg2 ($onlyPub | ConvertTo-Json -Depth 12)
    & $Xray -test -c $cfg1 *> (Join-Path $bh "xray-priv.log")
    Check ($LASTEXITCODE -eq 0) "xray -test：Reality 只给 privateKey 可通过" ("exit=" + $LASTEXITCODE)
    & $Xray -test -c $cfg2 *> (Join-Path $bh "xray-pub.log")
    $pubOk = ($LASTEXITCODE -eq 0)
    Check (-not $pubOk) "xray -test：只给 publicKey（无 privateKey）失败" ("exit=" + $LASTEXITCODE)
    if ($pubOk) { Get-Content (Join-Path $bh "xray-pub.log") -Tail 10 }
  } else {
    Write-Host "SKIP  xray 不在 $Xray，未验证 privateKey 结论"
  }
} finally {
  if ($panel -and -not $panel.HasExited) { Stop-Process -Id $panel.Id -Force }
  Write-Host "== 结果：$($script:fail) 个失败，临时目录 $bh =="
}
exit $script:fail
