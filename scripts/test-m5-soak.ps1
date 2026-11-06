# M5 稳定性 soak：持续运行观察 CPU/内存、日志轮转、并发请求、异常退出自动拉起、面板重启后状态恢复。
#
# 用法：
#   .\scripts\test-m5-soak.ps1 -Exe .\target\debug\berthio.exe -Minutes 35
# 结果落在 -OutDir（默认 D:\...\temp_20261105_09\m5\soak）：samples.csv + summary.json。
param(
  [string]$Exe = "",
  [string]$KernelDir = "",
  [int]$Port = 18195,
  [int]$InboundPort = 29601,
  [int]$Minutes = 35,
  [int]$IntervalSec = 5,
  [string]$OutDir = ""
)
$ErrorActionPreference = "Stop"
if ($PSVersionTable.PSVersion.Major -lt 6) {
  $pwsh = Get-Command pwsh -ErrorAction SilentlyContinue
  if ($pwsh) {
    & $pwsh.Source -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Exe $Exe -KernelDir $KernelDir -Port $Port -InboundPort $InboundPort -Minutes $Minutes -IntervalSec $IntervalSec -OutDir $OutDir
    exit $LASTEXITCODE
  }
  throw "需要 PowerShell 7+（pwsh）"
}
$script:fail = 0
$root = Split-Path -Parent $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $root "target\debug\berthio.exe" }
if (-not $KernelDir) { $KernelDir = Join-Path $root ".tools\kernel" }
if (-not $OutDir) { $OutDir = "D:\Users\Tyler Zhang\Documents\Trae-CN\temp_20261105_09\m5\soak" }
if (-not (Test-Path $Exe)) { throw "找不到 $Exe，先 cargo build" }
if (-not (Test-Path (Join-Path $KernelDir "xray"))) { throw "缺少 $KernelDir\xray，无法验证内核重启" }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$Base = "http://127.0.0.1:$Port"
$bh = Join-Path $env:TEMP ("berthio-m5-soak-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Force -Path $bh | Out-Null
$Password = "Berthio#2026"
$LogMax = 1048576
$LogDir = Join-Path $bh "logs\kernels"

function Check([bool]$cond, [string]$name, [string]$detail = "") {
  if ($cond) { Write-Host ("PASS  {0}  {1}" -f $name, $detail) }
  else { Write-Host ("FAIL  {0}  {1}" -f $name, $detail); $script:fail++ }
}
function Call([string]$method, [string]$path, $body = $null, $sess = $null, [hashtable]$headers = @{}) {
  $p = @{ Uri = ($Base + $path); Method = $method; SkipHttpErrorCheck = $true; TimeoutSec = 20 }
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
function Start-Panel {
  $out = Join-Path $bh "panel.out.log"; $err = Join-Path $bh "panel.err.log"
  $sp = Start-Process -FilePath $Exe -PassThru -NoNewWindow -RedirectStandardOutput $out -RedirectStandardError $err
  for ($i = 0; $i -lt 120; $i++) {
    if ($sp.HasExited) { return $null }
    try { Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck -TimeoutSec 1 | Out-Null; return $sp } catch { Start-Sleep -Milliseconds 250 }
  }
  return $null
}
function Login {
  $sess = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $r = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess
  if ($r.Status -ne 200) { throw ("登录失败: " + $r.Status + " " + $r.Raw) }
  [pscustomobject]@{ Sess = $sess; Csrf = $r.Body.csrf_token }
}
function Wait-PortFree {
  for ($i = 0; $i -lt 40; $i++) {
    try { Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck -TimeoutSec 1 | Out-Null } catch { return $true }
    Start-Sleep -Milliseconds 250
  }
  return $false
}
function Runtime([string]$kernel) { (Call "GET" "/api/kernels" -sess ($script:auth.Sess)).Body.runtime.$kernel }
function Seed-BigLog([int]$bytes) {
  New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
  $line = ("old-log-line " + ("y" * 80) + "`n")
  $n = [int]($bytes / $line.Length) + 1
  Set-Content -LiteralPath (Join-Path $LogDir "xray.log") -Value ($line * $n) -Encoding UTF8 -NoNewline
}
function Sha([string]$s) {
  $h = [System.Security.Cryptography.SHA256]::HashData([System.Text.Encoding]::UTF8.GetBytes($s))
  ([BitConverter]::ToString($h) -replace "-", "").ToLower()
}
function Burst([int]$n) {
  $client = [System.Net.Http.HttpClient]::new()
  $client.Timeout = [TimeSpan]::FromSeconds(15)
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $tasks = New-Object System.Collections.Generic.List[System.Threading.Tasks.Task]
  $clocks = @()
  for ($i = 0; $i -lt $n; $i++) {
    $clocks += [Diagnostics.Stopwatch]::StartNew()
    $tasks.Add($client.GetAsync($Base + $script:subPath))
  }
  [System.Threading.Tasks.Task]::WaitAll($tasks.ToArray())
  $rows = @()
  for ($i = 0; $i -lt $n; $i++) {
    try {
      $resp = $tasks[$i].Result
      $content = $resp.Content.ReadAsStringAsync().Result
      $h = [System.Security.Cryptography.SHA256]::HashData([System.Text.Encoding]::UTF8.GetBytes($content))
      $rows += [pscustomobject]@{
        status = [int]$resp.StatusCode; ms = $clocks[$i].ElapsedMilliseconds; bytes = $content.Length
        sha = (([BitConverter]::ToString($h) -replace "-", "").ToLower())
      }
    } catch {
      $rows += [pscustomobject]@{ status = 0; ms = $clocks[$i].ElapsedMilliseconds; bytes = 0; sha = "" }
    }
  }
  $client.Dispose()
  [pscustomobject]@{ Rows = @($rows); Ms = $sw.ElapsedMilliseconds }
}

$env:BERTHIO_HOME = $bh
$env:BERTHIO_LISTEN = "127.0.0.1:$Port"
$env:BERTHIO_KERNEL_DIR = $KernelDir
$env:BERTHIO_KERNEL_LOG_MAX = [string]$LogMax
Remove-Item Env:BERTHIO_TRUST_PROXY -ErrorAction SilentlyContinue

$panel = $null; $script:auth = $null; $script:subPath = ""
$samples = @()
$bursts = @()
try {
  Write-Host "== M5 soak @ $Base（HOME=$bh，时长=$Minutes 分钟，采样=${IntervalSec}s） =="
  $panel = Start-Panel
  Check ($null -ne $panel) "面板启动"
  $null = Call "POST" "/api/auth/setup" @{ password = $Password; confirm = $Password }
  $script:auth = Login
  $csrf = $script:auth.Csrf
  $setAddr = Call "PATCH" "/api/panel" @{ panel_address = "panel.example.com" } -sess $script:auth.Sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($setAddr.Status -eq 200) "设置对外地址" ("status=" + $setAddr.Status)

  $inb = Call "POST" "/api/inbound/inbounds" @{
    remark = "soak-xray"; protocol = "vless"; network = "tcp"; security = "reality"
    listen = "127.0.0.1"; port = $InboundPort; server_kernel = "xray"
    reality = @{ dest = "www.microsoft.com:443" }
  } -sess $script:auth.Sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($inb.Status -eq 201) "创建 xray 入站" ("status=" + $inb.Status)
  $inboundId = $inb.Body.item.id
  $clientId = $inb.Body.item.clients[0].id

  $tok = Call "POST" "/api/export/tokens" @{ client_id = $clientId; target = "mihomo" } -sess $script:auth.Sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($tok.Status -eq 201) "创建订阅令牌" ("status=" + $tok.Status)
  $script:subPath = "/sub/" + $tok.Body.token + "?target=mihomo"

  # 启动前先塞超阈值旧日志，启动时轮转（与 M3 同一机制；soak 期间再验一次）。
  Seed-BigLog ($LogMax + 300000)
  $st = Call "POST" "/api/kernels/xray/start" $null -sess $script:auth.Sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($st.Status -eq 200 -and $st.Body.status -eq "running") "xray 真实启动" ("pid=" + $st.Body.pid)
  $kernelPid = [int]$st.Body.pid
  $configSha = (Get-FileHash (Join-Path $bh "configs\generated\runtime-xray.json") -Algorithm SHA256).Hash.ToLower()
  $rot1 = Join-Path $LogDir "xray.log.1"
  Check ((Test-Path $rot1) -and ((Get-Item $rot1).Length -ge $LogMax)) "启动时超阈值日志轮转为 .log.1" ("size=" + (Get-Item $rot1 -ErrorAction SilentlyContinue).Length)

  $sub0 = Call "GET" $script:subPath
  Check ($sub0.Status -eq 200) "soak 前订阅可用" ("status=" + $sub0.Status)

  $t0 = Get-Date
  $deadline = $t0.AddMinutes($Minutes)
  $lastBurst = $t0
  $prevPanelCpu = $null; $prevKernelCpu = $null; $prevAt = $t0
  $prevKernelMem = $null; $prevPanelMem = $null
  $prevHandles = $null
  $didKill = $false; $didRotate = $false; $didPanelRestart = $false
  $killDetail = ""; $rotateDetail = ""; $panelRestartDetail = ""
  $maxHealthMs = 0; $healthFails = 0; $burstErrors = 0; $burstRows = 0; $burstShaMismatch = 0
  $sampleIndex = 0

  while ((Get-Date) -lt $deadline) {
    $now = Get-Date
    $elapsedMin = ($now - $t0).TotalMinutes
    $p = Get-Process -Id $panel.Id -ErrorAction SilentlyContinue
    if (-not $p) { Check $false "soak 期间面板进程存活（异常退出）" ("at=" + [int]($now - $t0).TotalSeconds + "s"); break }
    $k = Get-Process -Id $kernelPid -ErrorAction SilentlyContinue

    $wall = ($now - $prevAt).TotalSeconds
    $pcpu = $null
    if ($prevPanelCpu -and $wall -gt 0) { $pcpu = [math]::Round((($p.TotalProcessorTime.TotalSeconds - $prevPanelCpu) / $wall) * 100, 2) }
    $kcpu = $null
    if ($k -and $prevKernelCpu -and $wall -gt 0) { $kcpu = [math]::Round((($k.TotalProcessorTime.TotalSeconds - $prevKernelCpu) / $wall) * 100, 2) }
    $pmem = [math]::Round($p.WorkingSet64 / 1MB, 1)
    $kmem = if ($k) { [math]::Round($k.WorkingSet64 / 1MB, 1) } else { $null }

    $hs = [Diagnostics.Stopwatch]::StartNew()
    try {
      $r = Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck -TimeoutSec 5
      $healthMs = $hs.ElapsedMilliseconds
      if ([int]$r.StatusCode -ne 200) { $healthFails++ }
    } catch { $healthMs = 5000; $healthFails++ }
    if ($healthMs -gt $maxHealthMs) { $maxHealthMs = $healthMs }

    $note = ""
    # 25%：杀掉内核，验证异常退出自动拉起 + 状态恢复。
    if (-not $didKill -and $elapsedMin -ge [math]::Max(2, $Minutes * 0.25)) {
      $didKill = $true
      Write-Host "  [soak] 杀掉内核进程，验证自动拉起"
      $oldPid = $kernelPid
      Stop-Process -Id $kernelPid -Force -ErrorAction SilentlyContinue
      $restarted = $false
      for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 500
        $rt = Runtime "xray"
        if ($rt.status -eq "running" -and $rt.pid -gt 0 -and $rt.pid -ne $oldPid) { $restarted = $true; break }
      }
      $rt = Runtime "xray"
      Check $restarted "内核异常退出后自动拉起" ("old=$oldPid new=" + $rt.pid + " restarts=" + $rt.restarts)
      Check ($rt.pid -gt 0 -and (Get-Process -Id $rt.pid -ErrorAction SilentlyContinue)) "拉起后进程存活" ("pid=" + $rt.pid)
      $newSha = (Get-FileHash (Join-Path $bh "configs\generated\runtime-xray.json") -Algorithm SHA256).Hash.ToLower()
      Check ($newSha -eq $configSha) "拉起后配置与稳定态一致" ("sha=" + $newSha.Substring(0, 12))
      $s1 = Call "GET" $script:subPath
      Check ($s1.Status -eq 200) "拉起后订阅仍可用" ("status=" + $s1.Status)
      $kernelPid = [int]$rt.pid
      $prevKernelCpu = $null; $prevKernelMem = $null
      $killDetail = "old=$oldPid new=" + $rt.pid + " restarts=" + $rt.restarts
      $note = "kernel_auto_restart"
    }
    # 50%：停内核→塞超阈值日志→手动启动，验证再次轮转（.log.1/.log.2/.log.3 滚动）。
    if (-not $didRotate -and $elapsedMin -ge $Minutes * 0.5) {
      $didRotate = $true
      $null = Call "POST" "/api/kernels/xray/stop" $null -sess $script:auth.Sess -headers @{ "X-Berthio-CSRF" = $script:auth.Csrf }
      Start-Sleep -Milliseconds 800
      Seed-BigLog ($LogMax + 300000)
      $st2 = Call "POST" "/api/kernels/xray/start" $null -sess $script:auth.Sess -headers @{ "X-Berthio-CSRF" = $script:auth.Csrf }
      Check ($st2.Status -eq 200 -and $st2.Body.status -eq "running") "轮转测试后内核重新启动" ("pid=" + $st2.Body.pid)
      $kernelPid = [int]$st2.Body.pid
      $rotA = Join-Path $LogDir "xray.log.1"; $rotB = Join-Path $LogDir "xray.log.2"
      $sizeA = if (Test-Path $rotA) { (Get-Item $rotA).Length } else { 0 }
      Check ($sizeA -ge $LogMax) "第二次轮转 .log.1 仍是旧的大日志" ("size=" + $sizeA)
      # 轮转保留 3 份：第二次轮转后 .log.1（本次）与 .log.2（上一次）都在。
      Check ((Test-Path $rotB) -and ((Get-Item $rotB).Length -ge $LogMax)) "上一份轮转档滚到 .log.2（保留 3 份机制生效）" ("l2=" + (Test-Path $rotB))
      $s2 = Call "GET" $script:subPath
      Check ($s2.Status -eq 200) "轮转后订阅仍可用" ("status=" + $s2.Status)
      $prevKernelCpu = $null; $prevKernelMem = $null
      $rotateDetail = "log1=$sizeA log2=$((Test-Path $rotB))"
      $note = "log_rotation"
    }
    # 70%：重启面板进程，接管已有内核并恢复状态。
    if (-not $didPanelRestart -and $elapsedMin -ge $Minutes * 0.7) {
      $didPanelRestart = $true
      Write-Host "  [soak] 重启面板进程（接管内核）"
      $beforePid = $kernelPid
      Stop-Process -Id $panel.Id -Force -ErrorAction SilentlyContinue
      $free = Wait-PortFree
      Check $free "旧面板进程已退出、端口释放"
      Start-Sleep -Milliseconds 500
      $panel = Start-Panel
      Check (($null -ne $panel) -and (-not $panel.HasExited)) "面板重启成功"
      $script:auth = Login
      $rt = Runtime "xray"
      Check ($rt.status -eq "running" -and $rt.pid -eq $beforePid -and $rt.adopted -eq $true) "面板重启后接管内核（PID 不变）" ("pid=" + $rt.pid + " adopted=" + $rt.adopted)
      $listInb = Call "GET" "/api/inbound/inbounds" -sess $script:auth.Sess
      Check (@($listInb.Body.items).Count -ge 1 -and (@($listInb.Body.items | Where-Object { $_.id -eq $inboundId }).Count -eq 1)) "重启后入站状态完整" ("total=" + @($listInb.Body.items).Count)
      $s3 = Call "GET" $script:subPath
      Check ($s3.Status -eq 200) "重启后订阅仍可用" ("status=" + $s3.Status)
      $prevPanelCpu = $null; $prevPanelMem = $null
      $panelRestartDetail = "pid=" + $rt.pid + " adopted=" + $rt.adopted
      $note = "panel_restart"
    }

    # 并发突发：默认每 60 秒一次。
    if (($now - $lastBurst).TotalSeconds -ge 60) {
      $b = Burst 6
      $lastBurst = $now
      $bad = @($b.Rows | Where-Object { $_.status -ne 200 }).Count
      $shas = @($b.Rows | ForEach-Object { $_.sha } | Sort-Object -Unique)
      $bytes = @($b.Rows | ForEach-Object { $_.bytes } | Sort-Object -Unique)
      if ($bad -gt 0) { $burstErrors += $bad }
      if ($shas.Count -gt 1) { $burstShaMismatch++ }
      $burstRows += @($b.Rows).Count
      $bursts += [pscustomobject]@{ at_sec = [int]($now - $t0).TotalSeconds; n = 6; ok = 6 - $bad; wall_ms = $b.Ms; max_ms = (@($b.Rows | Measure-Object ms -Maximum).Maximum) }
      $note = ($note + " burst_ok=" + (6 - $bad) + "/6 wall=" + $b.Ms + "ms")
    }

    $samples += [pscustomobject]@{
      at_sec = [int]($now - $t0).TotalSeconds
      panel_cpu = $pcpu; panel_mem_mb = $pmem; panel_threads = $p.Threads.Count; panel_handles = $p.HandleCount
      kernel_cpu = $kcpu; kernel_mem_mb = $kmem
      health_ms = $healthMs; note = $note
    }
    $sampleIndex++
    if ($sampleIndex % 12 -eq 0) {
      Write-Host ("  t={0,5}s panel_cpu={1}% mem={2}MB threads={3} kernel_cpu={4}% kmem={5}MB health={6}ms" -f [int]($now - $t0).TotalSeconds, $pcpu, $pmem, $p.Threads.Count, $kcpu, $kmem, $healthMs)
    }

    # 面板/内核可能在本次迭代内被重启，统一按当前 PID 重新取句柄，避免负的 CPU 差值。
    $pNow = Get-Process -Id $panel.Id -ErrorAction SilentlyContinue
    $kNow = Get-Process -Id $kernelPid -ErrorAction SilentlyContinue
    $prevPanelCpu = if ($pNow) { $pNow.TotalProcessorTime.TotalSeconds } else { $null }
    $prevKernelCpu = if ($kNow) { $kNow.TotalProcessorTime.TotalSeconds } else { $null }
    $prevAt = $now
    Start-Sleep -Seconds $IntervalSec
  }

  # ---- 汇总与判定 ----
  $csv = Join-Path $OutDir "samples.csv"
  $samples | Export-Csv -NoTypeInformation -Encoding UTF8 -LiteralPath $csv
  $bursts | Export-Csv -NoTypeInformation -Encoding UTF8 -LiteralPath (Join-Path $OutDir "bursts.csv")

  $warm = @($samples | Where-Object { $_.at_sec -ge 60 })
  $pm = @($warm | ForEach-Object { $_.panel_mem_mb } | Where-Object { $_ })
  $pc = @($warm | ForEach-Object { $_.panel_cpu } | Where-Object { $null -ne $_ })
  $kc = @($warm | ForEach-Object { $_.kernel_cpu } | Where-Object { $null -ne $_ })
  $memMin = ($pm | Measure-Object -Minimum).Minimum
  $memMax = ($pm | Measure-Object -Maximum).Maximum
  $memAvg = [math]::Round(($pm | Measure-Object -Average).Average, 1)
  $memEnd = $pm[-1]; $memStart = $pm[0]
  $cpuAvg = [math]::Round(($pc | Measure-Object -Average).Average, 2)
  $cpuMax = ($pc | Measure-Object -Maximum).Maximum
  $kCpuAvg = if ($kc.Count) { [math]::Round(($kc | Measure-Object -Average).Average, 2) } else { $null }
  $slopePer10Min = if ($pm.Count -ge 4) {
    $n = $pm.Count
    $xs = @(0..($n - 1) | ForEach-Object { $warm[$_].at_sec / 600 })
    $meanX = ($xs | Measure-Object -Average).Average
    $meanY = ($pm | Measure-Object -Average).Average
    $num = 0; $den = 0
    for ($i = 0; $i -lt $n; $i++) { $num += ($xs[$i] - $meanX) * ($pm[$i] - $meanY); $den += [math]::Pow($xs[$i] - $meanX, 2) }
    if ($den -gt 0) { [math]::Round($num / $den, 2) } else { 0 }
  } else { 0 }

  $panelAlive = [bool](Get-Process -Id $panel.Id -ErrorAction SilentlyContinue)
  Check $panelAlive "soak 结束时面板仍存活" ([string]$panelAlive)
  Check ($healthFails -eq 0) "全程 healthz 无失败" ("fails=" + $healthFails)
  Check ($maxHealthMs -lt 2000) "healthz 最大延迟 < 2s" ("max=" + $maxHealthMs + "ms")
  Check ($burstErrors -eq 0) "并发订阅请求全部 200" ("rows=" + $burstRows + " errors=" + $burstErrors)
  Check ($burstShaMismatch -eq 0) "并发响应内容一致（无错乱/错配置）" ("mismatch_bursts=" + $burstShaMismatch)
  Check ($memMin -ge 0 -and (($memMax - $memMin) -lt 40)) "内存波动 < 40MB（无持续增长）" ("min=$memMin max=$memMax")
  Check (($slopePer10Min -lt 2) -and (($memEnd - $memStart) -lt 20)) "内存趋势平稳（无泄漏）" ("slope=" + $slopePer10Min + "MB/10min start=$memStart end=$memEnd")
  Check ($cpuAvg -lt 10) "面板平均 CPU < 10%（单核基准）" ("avg=" + $cpuAvg + "% max=" + $cpuMax + "%")
  Check ($null -eq $kCpuAvg -or $kCpuAvg -lt 10) "内核平均 CPU < 10%" ("avg=" + $kCpuAvg + "%")
  Check ([bool]$killDetail) "至少完成一轮异常退出自动拉起" $killDetail
  Check ([bool]$rotateDetail) "soak 期间日志完成第二轮轮转" $rotateDetail
  Check ([bool]$panelRestartDetail) "soak 期间完成面板重启接管" $panelRestartDetail

  $audit = (@(Get-ChildItem (Join-Path $bh "logs/audit") -Filter "*.jsonl" -ErrorAction SilentlyContinue) | ForEach-Object { Get-Content -Raw $_.FullName }) -join "`n"
  Check ($audit -match '"event":"kernel\.auto_restart"') "审计记录 kernel.auto_restart"
  Check ($audit -match '"event":"kernel\.adopted"') "审计记录面板重启接管 kernel.adopted"
  Check ($audit -match '"event":"sub\.access"') "审计记录订阅访问"

  $summary = [ordered]@{
    duration_min = $Minutes; samples = $samples.Count; csv = $csv
    panel_cpu_avg_pct = $cpuAvg; panel_cpu_max_pct = $cpuMax
    kernel_cpu_avg_pct = $kCpuAvg
    panel_mem_min_mb = $memMin; panel_mem_max_mb = $memMax; panel_mem_avg_mb = $memAvg
    panel_mem_start_mb = $memStart; panel_mem_end_mb = $memEnd; mem_slope_mb_per_10min = $slopePer10Min
    health_max_ms = $maxHealthMs; health_fails = $healthFails
    burst_rows = $burstRows; burst_errors = $burstErrors; burst_sha_mismatch = $burstShaMismatch
    kernel_auto_restart = $killDetail; log_rotation = $rotateDetail; panel_restart = $panelRestartDetail
    fail = $script:fail
  }
  $summary | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutDir "summary.json") -Encoding UTF8
  Write-Host ("汇总：" + ($summary | ConvertTo-Json -Compress))
} finally {
  try { $null = Call "POST" "/api/kernels/xray/stop" $null -sess $script:auth.Sess -headers @{ "X-Berthio-CSRF" = $script:auth.Csrf } } catch {}
  if ($panel -and -not $panel.HasExited) { Stop-Process -Id $panel.Id -Force -ErrorAction SilentlyContinue }
  Get-CimInstance Win32_Process -Filter "Name='xray.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -and $_.CommandLine.Contains($bh) } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
  $env:BERTHIO_HOME = $null; $env:BERTHIO_LISTEN = $null
  $env:BERTHIO_KERNEL_DIR = $null; $env:BERTHIO_KERNEL_LOG_MAX = $null
  Write-Host "临时目录：$bh（结果：$OutDir）"
}
if ($script:fail -gt 0) { Write-Host ("M5 soak：{0} 项失败" -f $script:fail); exit 1 }
Write-Host "M5 soak：全部通过"
exit 0
