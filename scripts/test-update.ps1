# Berthio 自更新验收（本机、不联网）：
#   1) 发现新版本才出现两个按钮（无头 UI）
#   2) 已最新时「更新 / 仅下载」不出现（无头 UI）
#   3) 错包拒收（签名不匹配）
#   4) 镜像失败自动回退官方源
#   5) 下载中断清理（.part 不残留）
#   6) 「仅下载」后按钮变「安装」，已最新仍保留「安装」（无头 UI）
#   7) （可选）真实替换可执行文件并自动重启，需要本机 minisign 签名工具与夹具私钥。
#
# 用法：
#   pwsh scripts/test-update.ps1
#   pwsh scripts/test-update.ps1 -SignTool <msign.exe> -SignKey <berthio.sk>
#
# 所有子进程都用 -NoNewWindow，浏览器由 UI 脚本强制 --headless=new。

param(
  [string]$Bin = "",
  [int]$Port = 18220,
  [int]$FeedPort = 18221,
  [string]$Work = "",
  [string]$SignTool = "",
  [string]$SignKey = "",
  [string]$UiScript = "",
  [switch]$SkipUi
)

$ErrorActionPreference = "Stop"
$Repo = Split-Path -Parent $PSScriptRoot
if (-not $Bin) { $Bin = Join-Path $Repo "target\debug\berthio.exe" }
if (-not (Test-Path $Bin)) { throw "找不到 $Bin，请先 cargo build" }
if (-not $Work) { $Work = Join-Path $env:TEMP "berthio-update-test" }
$Work = (New-Item -ItemType Directory -Force -Path $Work).FullName
$HomeDir = Join-Path $Work "home"
$Downloads = Join-Path $Work "downloads"
$FeedRoot = Join-Path $Work "feed"
$OutDir = Join-Path $Work "out"
foreach ($d in @($HomeDir, $Downloads, $FeedRoot, $OutDir)) {
  if (Test-Path $d) { Remove-Item $d -Recurse -Force -ErrorAction SilentlyContinue }
  New-Item -ItemType Directory -Force -Path $d | Out-Null
}
$Base = "http://127.0.0.1:$Port"
$FeedBase = "http://127.0.0.1:$FeedPort"
$Fixture = Join-Path $Repo "scripts\fixtures\update"
$Password = "Berthio#2026"
$fail = 0
$isWin = ($env:OS -eq "Windows_NT")
$Platform = if ($isWin) { "windows-x86_64" } else { "linux-x86_64" }
$AssetName = "berthio-1.0.1-$Platform" + $(if ($isWin) { ".exe" } else { "" })

function Check([bool]$cond, [string]$name, [string]$detail = "") {
  if ($cond) { Write-Host ("PASS  {0}  {1}" -f $name, $detail) }
  else { Write-Host ("FAIL  {0}  {1}" -f $name, $detail); $script:fail++ }
}

function Call([string]$method, [string]$path, $body = $null, $sess = $null, [hashtable]$headers = @{}) {
  $p = @{ Uri = ($Base + $path); Method = $method; SkipHttpErrorCheck = $true; TimeoutSec = 60 }
  if ($sess) { $p.WebSession = $sess }
  if ($null -ne $body) {
    $p.ContentType = "application/json; charset=utf-8"
    $p.Body = [System.Text.Encoding]::UTF8.GetBytes(($body | ConvertTo-Json -Compress -Depth 12))
  }
  if ($headers.Count -gt 0) { $p.Headers = $headers }
  $r = Invoke-WebRequest @p
  $j = $null
  try { $j = $r.Content | ConvertFrom-Json } catch {}
  [pscustomobject]@{ Status = [int]$r.StatusCode; Body = $j; Raw = $r.Content }
}

function Login {
  $sess = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $null = Call "POST" "/api/auth/setup" @{ password = $Password; confirm = $Password } -sess $sess
  $r = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess
  [pscustomobject]@{ Sess = $sess; Csrf = $r.Body.csrf_token }
}

function Headers($auth) { @{ "X-Berthio-CSRF" = $auth.Csrf } }

function Wait-Health([int]$seconds = 20) {
  $deadline = (Get-Date).AddSeconds($seconds)
  while ((Get-Date) -lt $deadline) {
    try {
      $r = Invoke-WebRequest -Uri ($Base + "/healthz") -UseBasicParsing -TimeoutSec 2 -SkipHttpErrorCheck
      if ($r.StatusCode -eq 200) { return $true }
    } catch {}
    Start-Sleep -Milliseconds 250
  }
  return $false
}

function Start-Panel([string]$exe, [string]$homePath) {
  $env:BERTHIO_HOME = $homePath
  $env:BERTHIO_DOWNLOAD_DIR = $Downloads
  $p = Start-Process -FilePath $exe -NoNewWindow -PassThru -RedirectStandardOutput (Join-Path $homePath "panel.out.log") -RedirectStandardError (Join-Path $homePath "panel.err.log")
  Remove-Item Env:\BERTHIO_HOME -ErrorAction SilentlyContinue
  Remove-Item Env:\BERTHIO_DOWNLOAD_DIR -ErrorAction SilentlyContinue
  return $p
}

function Stop-Panel($p) {
  if ($null -eq $p) { return }
  try { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } } catch {}
}

function Write-Manifest([string]$version, [string]$mode, [string]$sigFile, [int]$size) {
  $sigPath = Join-Path $FeedRoot $sigFile
  if (-not (Test-Path $sigPath)) { $sigPath = Join-Path $Fixture $sigFile }
  $sig = [Convert]::ToBase64String([IO.File]::ReadAllBytes($sigPath))
  $name = "berthio-$version-$Platform" + $(if ($isWin) { ".exe" } else { "" })
  $obj = @{
    version = $version
    notes = "本机离线测试清单"
    pub_date = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = @{
      $Platform = @{ signature = $sig; url = "$FeedBase/asset/$mode/$name"; size = $size }
    }
  }
  [IO.File]::WriteAllText((Join-Path $FeedRoot "latest.json"), ($obj | ConvertTo-Json -Depth 6), (New-Object System.Text.UTF8Encoding($false)))
}

function Status($auth) {
  $r = Call "GET" "/api/update/status" -sess $auth.Sess
  return $r.Body
}

function Wait-Update($auth, [int]$seconds = 40) {
  $deadline = (Get-Date).AddSeconds($seconds)
  $s = $null
  while ((Get-Date) -lt $deadline) {
    $s = Status $auth
    if (-not $s.busy) { return $s }
    Start-Sleep -Milliseconds 300
  }
  return $s
}

function Part-Files {
  $dir = Join-Path $HomeDir "tmp\update"
  if (-not (Test-Path $dir)) { return @() }
  return @(Get-ChildItem $dir -Filter "*.part" -ErrorAction SilentlyContinue)
}

# ---------------------------------------------------------------- 准备
Copy-Item (Join-Path $Fixture $AssetName) (Join-Path $FeedRoot "bin-ok.exe") -Force
$pubkey = [Convert]::ToBase64String([IO.File]::ReadAllBytes((Join-Path $Fixture "test-pubkey.txt")))
$cfg = @{
  listen = "127.0.0.1:$Port"
  update = @{
    feed = "$FeedBase/latest.json"
    mirror = "auto"
    pubkey = $pubkey
    mirrors = @(
      @{ id = "bad"; prefix = "$FeedBase/mirror/" },
      @{ id = "bad2"; prefix = "$FeedBase/mirror2/" }
    )
  }
}
New-Item -ItemType Directory -Force -Path (Join-Path $HomeDir "config") | Out-Null
[IO.File]::WriteAllText((Join-Path $HomeDir "config\panel.json"), ($cfg | ConvertTo-Json -Depth 8), (New-Object System.Text.UTF8Encoding($false)))

$server = Start-Process -FilePath "python" -NoNewWindow -PassThru -ArgumentList @("`"$(Join-Path $Fixture 'serve.py')`"", "--port", $FeedPort, "--root", "`"$FeedRoot`"") -RedirectStandardOutput (Join-Path $Work "feed.out.log") -RedirectStandardError (Join-Path $Work "feed.err.log")
$panel = $null
$auth = $null
$inst = $null
$copy = $null
try {
  Start-Sleep -Milliseconds 800
  Check ((Test-NetConnection -ComputerName 127.0.0.1 -Port $FeedPort -InformationLevel Quiet -WarningAction SilentlyContinue)) "离线更新源已启动"
  Write-Manifest "1.0.0" "ok" "asset.sig" 16384
  $panel = Start-Panel $Bin $HomeDir
  Check (Wait-Health) "面板启动"
  $auth = Login
  Check ($null -ne $auth.Csrf) "登录并取得 CSRF"

  # 1) 已最新
  $null = Call "POST" "/api/update/check" @{} -sess $auth.Sess -headers (Headers $auth)
  $s = Wait-Update $auth
  Check ($s.phase -eq "latest" -and $s.available -eq $false) "已最新时 phase=latest / available=false" ("phase=" + $s.phase)

  # 2) 发现新版本
  Write-Manifest "1.0.1" "ok" "asset.sig" 16384
  $null = Call "POST" "/api/update/check" @{} -sess $auth.Sess -headers (Headers $auth)
  $s = Wait-Update $auth
  Check ($s.phase -eq "available" -and $s.available -eq $true -and $s.version -eq "1.0.1") "发现新版本 1.0.1" ("phase=" + $s.phase + " version=" + $s.version)
  Check ($s.asset_size -eq 16384 -and $s.source -eq "官方源") "包大小与来源可见" ("size=" + $s.asset_size + " source=" + $s.source)

  # 3) 错包拒收（清单改写后先重新检查，下载沿用最近一次清单）
  Write-Manifest "1.0.1" "tamper" "asset.sig" 16384
  $null = Call "POST" "/api/update/check" @{} -sess $auth.Sess -headers (Headers $auth)
  $s = Wait-Update $auth
  $dl = Call "POST" "/api/update/download" @{ save = $true } -sess $auth.Sess -headers (Headers $auth)
  Check ($dl.Status -eq 202) "错包场景下载任务已受理" ("status=" + $dl.Status)
  $s = Wait-Update $auth
  $sigAttempt = $s.attempts | Where-Object { $_.error -like "*签名校验失败*" } | Select-Object -First 1
  Check (($s.phase -eq "error") -and ($null -ne $sigAttempt)) "错包拒收（签名不匹配，逐源记录）" ("sig=" + $(if ($sigAttempt) { $sigAttempt.error } else { "" }) + " error=" + $s.error)
  Check ($null -eq $s.saved) "错包不产生已下载记录"
  Check ((Part-Files).Count -eq 0) "错包不残留 .part"

  # 4) 下载中断清理
  Write-Manifest "1.0.1" "truncate" "asset.sig" 16384
  $null = Call "POST" "/api/update/check" @{} -sess $auth.Sess -headers (Headers $auth)
  $s = Wait-Update $auth
  $dl = Call "POST" "/api/update/download" @{ save = $true } -sess $auth.Sess -headers (Headers $auth)
  Check ($dl.Status -eq 202) "中断场景下载任务已受理" ("status=" + $dl.Status)
  $s = Wait-Update $auth
  Check ($s.phase -eq "error") "下载中断报错" ("error=" + $s.error)
  Check ((Part-Files).Count -eq 0) "下载中断后 .part 已清理"
  Check ($null -eq $s.saved) "下载中断不产生已下载记录"

  # 5) 镜像失败自动回退官方源
  $m = Call "PATCH" "/api/panel" @{ update_mirror = "bad" } -sess $auth.Sess -headers (Headers $auth)
  Check ($m.Status -eq 200) "切换所选镜像为 bad"
  Write-Manifest "1.0.1" "ok" "asset.sig" 16384
  $null = Call "POST" "/api/update/check" @{} -sess $auth.Sess -headers (Headers $auth)
  $s = Wait-Update $auth
  $badAttempt = $s.attempts | Where-Object { $_.source -like "*bad*" } | Select-Object -First 1
  Check (($s.phase -eq "available") -and ($s.source -eq "官方源")) "镜像失败后回退官方源" ("phase=" + $s.phase + " source=" + $s.source)
  Check (($null -ne $badAttempt) -and (-not $badAttempt.ok) -and ($badAttempt.error -like "*HTTP 500*")) "失败原因按源逐条记录" ("bad=" + $badAttempt.error)
  $null = Call "POST" "/api/update/download" @{ save = $true } -sess $auth.Sess -headers (Headers $auth)
  $s = Wait-Update $auth
  $saved = $s.saved
  Check (($s.phase -eq "ready") -and ($null -ne $saved) -and (Test-Path $saved.path)) "仅下载成功并记录（镜像失败仍回退）" ("path=" + $saved.path)
  Check (Test-Path ($saved.path + ".sig")) "签名旁挂文件已写出"
  Check ((Part-Files).Count -eq 0) "成功下载后无 .part 残留"
  $null = Call "PATCH" "/api/panel" @{ update_mirror = "auto" } -sess $auth.Sess -headers (Headers $auth)

  # 6) 重启后保留「安装」入口（状态持久化）
  Stop-Panel $panel
  $panel = Start-Panel $Bin $HomeDir
  Check (Wait-Health) "面板重启（模拟升级后）"
  $auth = Login
  $s = Status $auth
  Check (($null -ne $s.saved) -and (Test-Path $s.saved.path)) "重启后已下载且校验通过的包仍保留" ("path=" + $s.saved.path)

  # 7) 无头 UI：两按钮 / 已最新无按钮 / 仅下载变安装 / 镜像失败逐条展示
  if (-not $SkipUi) {
    if (-not $UiScript) { $UiScript = Join-Path $env:TEMP "berthio-update-ui\check.mjs" }
    if (-not (Test-Path $UiScript)) {
      Write-Host "SKIP  无头 UI 脚本不存在：$UiScript（用 -UiScript 指定）"
    } else {
      $node = Get-Command node -ErrorAction SilentlyContinue
      if (-not $node) { Write-Host "SKIP  未找到 node，未运行无头 UI 检查" }
      else {
        # UI 从「未下载」起步：清掉上一段产生的 saved 记录并重启
        Stop-Panel $panel
        Remove-Item (Join-Path $HomeDir "state\update.json") -Force -ErrorAction SilentlyContinue
        $panel = Start-Panel $Bin $HomeDir
        Check (Wait-Health) "UI 前重启（清空已下载记录）"
        $auth = Login
        Write-Manifest "1.0.1" "ok" "asset.sig" 16384
        Copy-Item (Join-Path $FeedRoot "latest.json") (Join-Path $FeedRoot "ui-new.json") -Force
        Write-Manifest "1.0.0" "ok" "asset.sig" 16384
        Copy-Item (Join-Path $FeedRoot "latest.json") (Join-Path $FeedRoot "ui-latest.json") -Force
        Write-Manifest "1.0.1" "ok" "asset.sig" 16384
        & node $UiScript $Base $FeedRoot $OutDir $Password
        if ($LASTEXITCODE -ne 0) { $script:fail++ }
      }
    }
  }

  # 8) 真实替换并自动重启（可选）
  if ($SignTool -and $SignKey) {
    $keyDir = Split-Path -Parent $SignKey
    $realPub = [Convert]::ToBase64String([IO.File]::ReadAllBytes((Join-Path $keyDir "berthio.pub")))
    $installDir = Join-Path $Work "install"
    $installHome = Join-Path $Work "install-home"
    New-Item -ItemType Directory -Force -Path $installDir, (Join-Path $installHome "config") | Out-Null
    $copy = Join-Path $installDir $(Split-Path -Leaf $Bin)
    Copy-Item $Bin $copy -Force
    Copy-Item $Bin (Join-Path $FeedRoot "bin.exe") -Force
    $sigOut = Join-Path $Work "install.sig"
    & $SignTool sign $SignKey (Join-Path $FeedRoot "bin.exe") "1.0.1" $AssetName $sigOut | Out-Null
    Copy-Item $sigOut (Join-Path $FeedRoot "install.sig") -Force
    Write-Manifest "1.0.1" "exe" "install.sig" (Get-Item (Join-Path $FeedRoot "bin.exe")).Length

    $cfg2 = @{
      listen = "127.0.0.1:18230"
      update = @{ feed = "$FeedBase/latest.json"; mirror = "auto"; pubkey = $realPub; mirrors = @(@{ id = "bad"; prefix = "$FeedBase/mirror/" }) }
    }
    [IO.File]::WriteAllText((Join-Path $installHome "config\panel.json"), ($cfg2 | ConvertTo-Json -Depth 8), (New-Object System.Text.UTF8Encoding($false)))
    $env:BERTHIO_HOME = $installHome
    $env:BERTHIO_DOWNLOAD_DIR = $Downloads
    $inst = Start-Process -FilePath $copy -NoNewWindow -PassThru -RedirectStandardOutput (Join-Path $installHome "panel.out.log") -RedirectStandardError (Join-Path $installHome "panel.err.log")
    Remove-Item Env:\BERTHIO_HOME -ErrorAction SilentlyContinue
    Remove-Item Env:\BERTHIO_DOWNLOAD_DIR -ErrorAction SilentlyContinue
    $base2 = "http://127.0.0.1:18230"
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
      try { $r = Invoke-WebRequest -Uri "$base2/healthz" -UseBasicParsing -TimeoutSec 2; if ($r.StatusCode -eq 200) { break } } catch {}
      Start-Sleep -Milliseconds 250
    }
    $sess2 = New-Object Microsoft.PowerShell.Commands.WebRequestSession
    $null = Invoke-WebRequest -Uri "$base2/api/auth/setup" -Method POST -ContentType "application/json" -Body (@{ password = $Password; confirm = $Password } | ConvertTo-Json) -WebSession $sess2 -SkipHttpErrorCheck
    $lr = Invoke-WebRequest -Uri "$base2/api/auth/login" -Method POST -ContentType "application/json" -Body (@{ password = $Password } | ConvertTo-Json) -WebSession $sess2 -SkipHttpErrorCheck
    $csrf2 = ($lr.Content | ConvertFrom-Json).csrf_token
    $oldPid = $inst.Id
    $null = Invoke-WebRequest -Uri "$base2/api/update/check" -Method POST -ContentType "application/json" -Body "{}" -Headers @{ "X-Berthio-CSRF" = $csrf2 } -WebSession $sess2 -SkipHttpErrorCheck
    $avail = $false
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
      Start-Sleep -Milliseconds 400
      $st = Invoke-WebRequest -Uri "$base2/api/update/status" -WebSession $sess2 -SkipHttpErrorCheck | Select-Object -ExpandProperty Content | ConvertFrom-Json
      if (-not $st.busy) { $avail = ($st.available -eq $true); break }
    }
    Check $avail "安装场景：检测到 1.0.1"
    $null = Invoke-WebRequest -Uri "$base2/api/update/download" -Method POST -ContentType "application/json" -Body (@{ save = $false } | ConvertTo-Json) -Headers @{ "X-Berthio-CSRF" = $csrf2 } -WebSession $sess2 -SkipHttpErrorCheck
    # 旧进程应退出，新进程在 18230 上重新应答
    $deadline = (Get-Date).AddSeconds(45)
    $back = $false
    while ((Get-Date) -lt $deadline) {
      Start-Sleep -Milliseconds 500
      $alive = Get-Process -Id $oldPid -ErrorAction SilentlyContinue
      if (-not $alive) {
        try {
          $h = Invoke-WebRequest -Uri "$base2/healthz" -UseBasicParsing -TimeoutSec 2 -SkipHttpErrorCheck
          if ($h.StatusCode -eq 200) { $back = $true; break }
        } catch {}
      }
    }
    Check $back "替换后旧进程退出并自动重启"
    $newProc = Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($copy)) -ErrorAction SilentlyContinue | Where-Object { $_.Id -ne $oldPid -and $_.Path -eq $copy } | Select-Object -First 1
    Check ($null -ne $newProc -and $newProc.Id -ne $oldPid) "新进程已接管（PID 变化）" ("old=" + $oldPid + " new=" + $(if ($newProc) { $newProc.Id } else { "none" }))
    if ($back) {
      $sess3 = New-Object Microsoft.PowerShell.Commands.WebRequestSession
      $lr3 = Invoke-WebRequest -Uri "$base2/api/auth/login" -Method POST -ContentType "application/json" -Body (@{ password = $Password } | ConvertTo-Json) -WebSession $sess3 -SkipHttpErrorCheck
      $s3 = Invoke-WebRequest -Uri "$base2/api/update/status" -WebSession $sess3 -SkipHttpErrorCheck | Select-Object -ExpandProperty Content | ConvertFrom-Json
      Check (($s3.last_install.version -eq "1.0.1") -and (-not [string]::IsNullOrEmpty($s3.last_install.restarted_at))) "重启标记写入（last_install.restarted_at）" ("restarted_at=" + $s3.last_install.restarted_at)
    }
    Stop-Panel $inst
    Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($copy)) -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $copy } | Stop-Process -Force -ErrorAction SilentlyContinue
  } else {
    Write-Host "SKIP  真实替换重启（未提供 -SignTool / -SignKey）"
  }
} finally {
  Stop-Panel $panel
  Stop-Panel $inst
  if ($copy) {
    Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($copy)) -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $copy } | Stop-Process -Force -ErrorAction SilentlyContinue
  }
  if ($server) { try { if (-not $server.HasExited) { Stop-Process -Id $server.Id -Force } } catch {} }
}

Write-Host ("== 自更新验收：{0} 项失败 ==" -f $fail)
if ($fail -gt 0) { exit 1 }
