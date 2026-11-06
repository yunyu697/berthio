# M3 回归：内核进程管理（校验→端口→启动→停止→自动拉起上限→日志轮转→面板重启接管→孤儿清理→失败回滚）。
# 需要真实内核二进制：xray 必装，sing-box / mihomo 有则做配置校验，没有则跳过。
#
# 用法：
#   .\scripts\test-m3-kernels.ps1 -Exe .\target\debug\berthio.exe -KernelDir .\.tools\kernel
param(
  [string]$Exe = "",
  [string]$KernelDir = "",
  [int]$Port = 18184,
  [int]$InboundPort = 29501
)
$ErrorActionPreference = "Stop"
# 脚本使用 -SkipHttpErrorCheck，需要 PowerShell 7+；5.1 下自动用 pwsh 重新执行。
if ($PSVersionTable.PSVersion.Major -lt 6) {
  $pwsh = Get-Command pwsh -ErrorAction SilentlyContinue
  if ($pwsh) {
    & $pwsh.Source -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Exe $Exe -KernelDir $KernelDir -Port $Port -InboundPort $InboundPort
    exit $LASTEXITCODE
  }
  throw "需要 PowerShell 7+（pwsh）"
}
$script:fail = 0
$root = Split-Path -Parent $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $root "target\debug\berthio.exe" }
if (-not $KernelDir) { $KernelDir = Join-Path $root ".tools\kernel" }
if (-not (Test-Path $Exe)) { throw "找不到 $Exe，先 cargo build" }
if (-not (Test-Path $KernelDir)) { throw "找不到内核目录 $KernelDir" }

$Base = "http://127.0.0.1:$Port"
$bh = Join-Path $env:TEMP ("berthio-m3-kern-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Force -Path $bh | Out-Null
$Password = "Berthio#2026"

function Check([bool]$cond, [string]$name, [string]$detail = "") {
  if ($cond) { Write-Host ("PASS  {0}  {1}" -f $name, $detail) }
  else { Write-Host ("FAIL  {0}  {1}" -f $name, $detail); $script:fail++ }
}

function Call([string]$method, [string]$path, $body = $null, $sess = $null, [hashtable]$headers = @{}) {
  $p = @{ Uri = ($Base + $path); Method = $method; SkipHttpErrorCheck = $true }
  if ($sess) { $p.WebSession = $sess }
  if ($null -ne $body) {
    $p.ContentType = 'application/json; charset=utf-8'
    $p.Body = [System.Text.Encoding]::UTF8.GetBytes(($body | ConvertTo-Json -Compress -Depth 10))
  }
  if ($headers.Count -gt 0) { $p.Headers = $headers }
  $r = Invoke-WebRequest @p
  $j = $null
  try { $j = $r.Content | ConvertFrom-Json } catch {}
  [pscustomobject]@{ Status = [int]$r.StatusCode; Body = $j }
}

function Start-Panel {
  $probe = $null
  try { $probe = Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck } catch {}
  if ($probe) { throw "端口 $Port 已被占用，换 -Port" }
  $env:BERTHIO_HOME = $bh
  $env:BERTHIO_LISTEN = "127.0.0.1:$Port"
  $env:BERTHIO_KERNEL_DIR = $KernelDir
  $env:BERTHIO_KERNEL_LOG_MAX = "1048576"
  $sp = Start-Process -FilePath $Exe -PassThru `
    -RedirectStandardOutput (Join-Path $bh "panel.out.log") `
    -RedirectStandardError (Join-Path $bh "panel.err.log")
  $script:panel = $sp
  for ($i = 0; $i -lt 60; $i++) {
    try { Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck | Out-Null; return $sp } catch { Start-Sleep -Milliseconds 300 }
  }
  throw "面板未在预期时间内启动（$Base）"
}

function Login {
  $sess = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $r = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess
  if ($r.Status -ne 200) { throw ("登录失败: " + $r.Body.error.code) }
  $script:sess = $sess
  $r.Body.csrf_token
}

function Runtime([string]$kernel) {
  (Call "GET" "/api/kernels" -sess $script:sess).Body.runtime.$kernel
}

Write-Host "== M3 内核进程回归 @ $Base（HOME=$bh，KERNEL_DIR=$KernelDir） =="
$panel = $null
$listener = $null
try {
  $panel = Start-Panel
  $setup = Call "POST" "/api/auth/setup" @{ password = $Password; confirm = $Password }
  if ($setup.Status -ne 200) { throw ("初始化失败: " + $setup.Body.error.code) }
  $script:sess = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $login = Call "POST" "/api/auth/login" @{ password = $Password } -sess $script:sess
  if ($login.Status -ne 200) { throw "登录失败" }
  $csrf = $login.Body.csrf_token

  # 建一条 xray reality 入站（密钥由向导生成）
  $mk = Call "POST" "/api/inbound/inbounds" @{
    remark = "m3-kern"; protocol = "vless"; network = "tcp"; security = "reality"
    listen = "127.0.0.1"; port = $InboundPort; server_kernel = "xray"
    reality = @{ dest = "www.microsoft.com:443" }
  } -sess $script:sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($mk.Status -eq 201) "创建 xray reality 入站" ("status=" + $mk.Status)

  # 1) 配置校验（真实调用 xray -test）
  $val = Call "POST" "/api/kernels/xray/validate" $null -sess $script:sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($val.Status -eq 200 -and $val.Body.ok -eq $true) "xray 配置校验通过" ("cmd=" + $val.Body.command)
  Check ($val.Body.command -match "xray -test") "校验命令为 xray -test"

  # 可选的 mihomo / sing-box 配置校验（有二进制才做）
  foreach ($kn in @("mihomo", "sing-box")) {
    $bin = Get-ChildItem -Path $KernelDir -Recurse -Filter "$kn.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $bin) { Write-Host "SKIP  $kn 配置校验（未安装）"; continue }
    if ($kn -eq "sing-box") {
      $mk2 = Call "POST" "/api/inbound/inbounds" @{
        remark = "m3-kern-sb"; protocol = "vless"; network = "tcp"; security = "reality"
        listen = "127.0.0.1"; port = ($InboundPort + 1); server_kernel = "sing-box"
        reality = @{ dest = "www.microsoft.com:443" }
      } -sess $script:sess -headers @{ "X-Berthio-CSRF" = $csrf }
      if ($mk2.Status -ne 201) { Write-Host ("SKIP  sing-box 入站创建失败: " + $mk2.Body.error.code); continue }
    }
    $v = Call "POST" "/api/kernels/$kn/validate" $null -sess $script:sess -headers @{ "X-Berthio-CSRF" = $csrf }
    Check ($v.Status -eq 200 -and $v.Body.ok -eq $true) "$kn 配置校验通过" ("cmd=" + $v.Body.command)
  }

  # 2) 日志轮转：先塞一个超过阈值的旧日志，启动时应轮转为 .log.1
  $logDir = Join-Path $bh "logs\kernels"
  New-Item -ItemType Directory -Force -Path $logDir | Out-Null
  $big = New-Object System.Text.StringBuilder
  while ($big.Length -lt 1300000) { [void]$big.AppendLine("old-log-line " + ("y" * 80)) }
  Set-Content (Join-Path $logDir "xray.log") $big.ToString() -Encoding UTF8
  $start1 = Call "POST" "/api/kernels/xray/start" $null -sess $script:sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($start1.Status -eq 200 -and $start1.Body.status -eq "running") "xray 真实启动" ("pid=" + $start1.Body.pid)
  $rotated = Join-Path $logDir "xray.log.1"
  Check ((Test-Path $rotated) -and ((Get-Item $rotated).Length -gt 1048576)) "超过阈值时日志轮转出 .log.1" ("size=" + (Get-Item $rotated -ErrorAction SilentlyContinue).Length)
  $pid1 = $start1.Body.pid
  Check ([bool](Get-Process -Id $pid1 -ErrorAction SilentlyContinue)) "进程存活"

  # 3) 端口冲突：先检测再启动，并指出占用者
  $stop1 = Call "POST" "/api/kernels/xray/stop" $null -sess $script:sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($stop1.Status -eq 200 -and $stop1.Body.status -eq "stopped") "stop 结束进程"
  Start-Sleep -Milliseconds 500
  $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Parse("127.0.0.1"), $InboundPort)
  $listener.Start()
  $conflict = Call "POST" "/api/kernels/xray/start" $null -sess $script:sess -headers @{ "X-Berthio-CSRF" = $csrf }
  $owner = ($conflict.Body.error.fields | Where-Object { $_.field -eq "owner" }).message
  Check ($conflict.Status -eq 409 -and $conflict.Body.error.code -eq "port_in_use") "端口占用时拒绝启动" ("status=" + $conflict.Status)
  Check ([bool]$owner -and $owner -match ([string]$PID)) "错误里指出占用者 PID/进程名" ("owner=" + $owner)
  $listener.Stop(); $listener = $null

  $start2 = Call "POST" "/api/kernels/xray/start" $null -sess $script:sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($start2.Status -eq 200 -and $start2.Body.status -eq "running") "端口释放后可启动" ("pid=" + $start2.Body.pid)
  $pid2 = $start2.Body.pid

  # 4) 异常退出自动拉起，超过上限停止并告警
  $seen = @()
  for ($i = 0; $i -lt 4; $i++) {
    Stop-Process -Id $pid2 -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 3
    $rt = Runtime "xray"
    $seen += ("$($rt.status)/r=$($rt.restarts)")
    if ($rt.status -eq "running") { $pid2 = $rt.pid }
  }
  $rtLimit = Runtime "xray"
  Check ($seen[0] -match "^running/r=1") "第 1 次异常退出自动拉起" ($seen[0])
  Check ($rtLimit.status -eq "failed" -and $rtLimit.alert) "超过拉起上限后停止并告警" ("status=" + $rtLimit.status + " alert=" + $rtLimit.alert)
  Check ($rtLimit.restarts -eq 3) "拉起次数等于上限" ("restarts=" + $rtLimit.restarts)

  # 5) 手动 start 重置拉起计数
  $start3 = Call "POST" "/api/kernels/xray/start" $null -sess $script:sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($start3.Status -eq 200 -and $start3.Body.status -eq "running") "告警后手动启动可恢复" ("pid=" + $start3.Body.pid)
  $pid3 = $start3.Body.pid
  $rt3 = Runtime "xray"
  Check ($rt3.restarts -eq 0) "手动启动重置 restarts" ("restarts=" + $rt3.restarts)

  # 6) 面板重启后接管内核（PID 不变、adopted=true）
  Stop-Process -Id $panel.Id -Force
  Start-Sleep -Seconds 1
  $panel = Start-Panel
  $csrf = Login
  $rt4 = Runtime "xray"
  Check ($rt4.status -eq "running" -and $rt4.adopted -eq $true -and $rt4.pid -eq $pid3) "面板重启后接管内核" ("pid=" + $rt4.pid + " adopted=" + $rt4.adopted)
  Check ([bool](Get-Process -Id $pid3 -ErrorAction SilentlyContinue)) "接管后进程仍存活"

  # 7) 孤儿清理：状态文件丢失后，面板应清理本面板配置残留的进程
  Stop-Process -Id $panel.Id -Force
  Start-Sleep -Seconds 1
  Remove-Item (Join-Path $bh "state\kernels.json") -Force
  $panel = Start-Panel
  $csrf = Login
  Start-Sleep -Seconds 2
  Check (-not (Get-Process -Id $pid3 -ErrorAction SilentlyContinue)) "状态丢失后孤儿进程被清理" ("pid=" + $pid3)

  # 8) 配置校验失败：不启动 + 回滚 last-known-good
  Stop-Process -Id $panel.Id -Force
  Start-Sleep -Seconds 1
  $statePath = Join-Path $bh "state\state.json"
  $st = Get-Content $statePath -Raw | ConvertFrom-Json
  $st.inbounds[0].reality.private_key = "INVALID_PRIVATE_KEY_FOR_M3_TEST"
  $st | ConvertTo-Json -Depth 20 | Set-Content $statePath -Encoding UTF8
  $panel = Start-Panel
  $csrf = Login
  $bad = Call "POST" "/api/kernels/xray/start" $null -sess $script:sess -headers @{ "X-Berthio-CSRF" = $csrf }
  Check ($bad.Status -eq 422 -and $bad.Body.error.code -eq "config_check_failed") "校验失败拒绝启动" ("status=" + $bad.Status)
  $output = ($bad.Body.error.fields | Where-Object { $_.field -eq "output" }).message
  Check ([bool]$output -and $output -match "privateKey") "错误里带二进制输出的具体原因"
  $gen = Join-Path $bh "configs\generated\runtime-xray.json"
  $lkg = Join-Path $bh "state\kernels\xray.lkg"
  $genSha = (Get-FileHash $gen -Algorithm SHA256).Hash.ToLower()
  $lkgSha = (Get-FileHash $lkg -Algorithm SHA256).Hash.ToLower()
  Check ($genSha -eq $lkgSha) "失败后回滚到 last-known-good 配置" ("sha=" + $genSha.Substring(0, 12))
  $rtBad = Runtime "xray"
  Check ($rtBad.status -eq "config_invalid" -and $rtBad.pid -eq 0) "失败状态落盘且无进程" ("status=" + $rtBad.status)
  Check (@(Get-Process xray -ErrorAction SilentlyContinue).Count -eq 0) "校验失败没有启动真实进程"
} finally {
  if ($listener) { try { $listener.Stop() } catch {} }
  if ($script:panel) { Stop-Process -Id $script:panel.Id -Force -ErrorAction SilentlyContinue }
  # 清掉本脚本 HOME 下可能残留的内核进程
  Get-CimInstance Win32_Process -Filter "Name='xray.exe' OR Name='sing-box.exe' OR Name='mihomo.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -and $_.CommandLine.Contains($bh) } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
  $env:BERTHIO_HOME = $null; $env:BERTHIO_LISTEN = $null
  $env:BERTHIO_KERNEL_DIR = $null; $env:BERTHIO_KERNEL_LOG_MAX = $null
  Remove-Item -Recurse -Force $bh -ErrorAction SilentlyContinue
}

Write-Host ""
if ($script:fail -eq 0) { Write-Host "M3 内核进程回归：全部通过" } else { Write-Host ("M3 内核进程回归：{0} 项失败" -f $script:fail) }
exit $(if ($script:fail -eq 0) { 0 } else { 1 })
