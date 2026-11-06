# M3 回归：首次初始化、口令哈希、会话 Cookie、CSRF、登录锁定、会话过期、注销、远程监听二次确认、反代转发头。
# 脚本自己起一个临时面板（临时 HOME、临时端口），结束后清理，不碰现有数据。
#
# 用法：
#   .\scripts\test-m3-auth.ps1 -Exe .\target\debug\berthio.exe
param(
  [string]$Exe = "",
  [int]$Port = 18183
)
$ErrorActionPreference = "Stop"
# 脚本使用 -SkipHttpErrorCheck，需要 PowerShell 7+；5.1 下自动用 pwsh 重新执行。
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
$bh = Join-Path $env:TEMP ("berthio-m3-auth-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
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
  [pscustomobject]@{ Status = [int]$r.StatusCode; Body = $j; Headers = $r.Headers }
}

function Start-Panel {
  $probe = $null
  try { $probe = Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck } catch {}
  if ($probe) { throw "端口 $Port 已被占用，换 -Port" }
  $env:BERTHIO_HOME = $bh
  $env:BERTHIO_LISTEN = "127.0.0.1:$Port"
  $env:BERTHIO_SESSION_TTL_SECS = "4"
  $env:BERTHIO_LOGIN_MAX_FAILS = "3"
  $env:BERTHIO_LOGIN_LOCK_SECS = "3"
  $env:BERTHIO_TRUST_PROXY = ""
  $sp = Start-Process -FilePath $Exe -PassThru `
    -RedirectStandardOutput (Join-Path $bh "panel.out.log") `
    -RedirectStandardError (Join-Path $bh "panel.err.log")
  $script:panel = $sp
  for ($i = 0; $i -lt 60; $i++) {
    try { Invoke-WebRequest "$Base/healthz" -SkipHttpErrorCheck | Out-Null; return $sp } catch { Start-Sleep -Milliseconds 300 }
  }
  throw "面板未在预期时间内启动（$Base），见 $bh\panel.err.log"
}

Write-Host "== M3 鉴权回归 @ $Base（HOME=$bh） =="
$panel = $null
try {
  $panel = Start-Panel

  # 1) 首次启动：未初始化，公开状态不泄露哈希
  $st = Call "GET" "/api/auth/status"
  Check ($st.Status -eq 200 -and $st.Body.initialized -eq $false) "首次启动未初始化" ("initialized=" + $st.Body.initialized)

  # 2) 弱口令被拒
  $weak = Call "POST" "/api/auth/setup" @{ password = "abc123"; confirm = "abc123" }
  Check ($weak.Status -eq 400) "弱口令被拒" ("status=" + $weak.Status)

  # 3) 初始化 + 会话 Cookie 属性
  $s = $null
  $setup = Call "POST" "/api/auth/setup" @{ password = $Password; confirm = $Password }
  Check ($setup.Status -eq 200 -and $setup.Body.initialized) "管理员口令初始化成功"
  $authFile = Join-Path $bh "state\auth.json"
  $raw = Get-Content $authFile -Raw
  Check (-not $raw.Contains($Password)) "auth.json 不含明文口令"
  Check ($raw.Contains("pbkdf2-hmac-sha256")) "口令以 PBKDF2 哈希存储"

  $sess = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $login = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess
  Check ($login.Status -eq 200 -and $login.Body.authenticated) "登录成功" ("csrf=" + $login.Body.csrf_token.Substring(0, 8))
  $cookie = $sess.Cookies.GetCookies($Base) | Where-Object { $_.Name -eq "berthio_session" }
  Check ($null -ne $cookie) "会话 Cookie 已下发"
  Check ($cookie.HttpOnly) "Cookie 带 HttpOnly"
  Check ($cookie.Path -eq "/") "Cookie Path=/ (无 domain 属性即可避免跨站)"
  $setCookie = ($login.Headers["Set-Cookie"] -join ";")
  Check ($setCookie -match "SameSite=Strict") "Cookie 带 SameSite=Strict" $setCookie
  $csrf = $login.Body.csrf_token

  # 4) 无会话访问写接口 / 读接口 → 401
  $anon = Call "GET" "/api/panel"
  Check ($anon.Status -eq 401) "未登录读接口 401" ("code=" + $anon.Body.error.code)
  $anonPatch = Call "PATCH" "/api/panel" @{ panel_address = "x" }
  Check ($anonPatch.Status -eq 401) "未登录写接口 401"

  # 5) 错误口令 ×3 → 第 3 次直接锁定（429）
  $codes = @()
  for ($i = 0; $i -lt 3; $i++) { $codes += (Call "POST" "/api/auth/login" @{ password = "wrong-pass-1" }).Status }
  Check ($codes[0] -eq 401 -and $codes[1] -eq 401 -and $codes[2] -eq 429) "连续失败触发临时锁定" ("codes=" + ($codes -join ","))
  $locked = Call "POST" "/api/auth/login" @{ password = $Password }
  Check ($locked.Status -eq 429 -and $locked.Body.error.code -eq "login_locked") "锁定期内正确口令也拒绝" ("retry_after=" + $locked.Body.error.fields[0].message)

  # 6) 等待锁定结束 → 可登录
  Start-Sleep -Seconds 4
  $sess2 = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $login2 = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess2
  Check ($login2.Status -eq 200) "锁定到期后可登录"
  $csrf2 = $login2.Body.csrf_token

  # 7) CSRF：无令牌 403，带令牌 200
  $noCsrf = Call "PATCH" "/api/panel" @{ listen = "127.0.0.1:$Port" } -sess $sess2
  Check ($noCsrf.Status -eq 403 -and $noCsrf.Body.error.code -eq "csrf_required") "写操作缺少 CSRF 被拒" ("status=" + $noCsrf.Status)
  $badCsrf = Call "PATCH" "/api/panel" @{ listen = "127.0.0.1:$Port" } -sess $sess2 -headers @{ "X-Berthio-CSRF" = "deadbeef" }
  Check ($badCsrf.Status -eq 403 -and $badCsrf.Body.error.code -eq "csrf_invalid") "CSRF 令牌错误被拒"
  $okPatch = Call "PATCH" "/api/panel" @{ listen = "127.0.0.1:$Port" } -sess $sess2 -headers @{ "X-Berthio-CSRF" = $csrf2 }
  Check ($okPatch.Status -eq 200) "带正确 CSRF 的写操作通过"

  # 8) 伪造 X-Forwarded-For 不被信任（未配置受信代理时以 peer 为准）
  $spoof = Call "PATCH" "/api/panel" @{ panel_address = "panel.example.com" } -sess $sess2 -headers @{
    "X-Berthio-CSRF" = $csrf2; "X-Forwarded-For" = "10.9.9.9"
  }
  $auditFile = Join-Path $bh ("logs\audit\" + (Get-Date -Format "yyyy-MM") + ".jsonl")
  $lastAudit = (Get-Content $auditFile -Tail 5 | Select-String "panel.config.updated" | Select-Object -Last 1)
  $auditText = if ($lastAudit) { $lastAudit.Line } else { "" }
  Check ($spoof.Status -eq 200 -and $auditText.Contains("127.0.0.1") -and -not $auditText.Contains("10.9.9.9")) "伪造 XFF 被忽略，审计记录 peer IP" ("audit=" + $auditText)

  # 9) 远程监听：未二次确认必须拒绝，确认后放行并要求重启
  $remoteNoAck = Call "PATCH" "/api/panel" @{ listen = "0.0.0.0:$Port" } -sess $sess2 -headers @{ "X-Berthio-CSRF" = $csrf2 }
  Check ($remoteNoAck.Status -eq 400 -and $remoteNoAck.Body.error.code -eq "bad_request") "非回环监听未确认被拒" ("status=" + $remoteNoAck.Status)
  $remoteRisk = ($remoteNoAck.Body.error.fields | Where-Object { $_.field -eq "risk" }).message
  Check ([bool]$remoteRisk) "拒绝时给出风险提示"
  $remoteAck = Call "PATCH" "/api/panel" @{
    listen = "0.0.0.0:$Port"; remote_enabled = $true; confirm_remote = "ENABLE-REMOTE"
  } -sess $sess2 -headers @{ "X-Berthio-CSRF" = $csrf2 }
  Check ($remoteAck.Status -eq 200 -and $remoteAck.Body.restart_required) "二次确认后允许远程监听并要求重启"
  $disable = Call "PATCH" "/api/panel" @{ listen = "127.0.0.1:$Port"; remote_enabled = $false } -sess $sess2 -headers @{ "X-Berthio-CSRF" = $csrf2 }
  Check ($disable.Status -eq 200) "可恢复回环监听"

  # 10) 会话有效期（脚本把 TTL 设为 4 秒）
  Start-Sleep -Seconds 6
  $expired = Call "GET" "/api/auth/status" -sess $sess2
  Check ($expired.Status -eq 200 -and $expired.Body.authenticated -eq $false) "会话到期后自动失效"
  $expiredApi = Call "GET" "/api/panel" -sess $sess2
  Check ($expiredApi.Status -eq 401) "过期会话访问接口 401"

  # 11) 主动注销
  $sess3 = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $login3 = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess3
  $csrf3 = $login3.Body.csrf_token
  $logout = Call "POST" "/api/auth/logout" $null -sess $sess3 -headers @{ "X-Berthio-CSRF" = $csrf3 }
  Check ($logout.Status -eq 200 -and $logout.Body.session_revoked) "会话主动注销成功"
  $afterLogout = Call "GET" "/api/panel" -sess $sess3
  Check ($afterLogout.Status -eq 401) "注销后旧会话立即失效"

  # 12) 修改口令会吊销所有会话
  $sess4 = New-Object Microsoft.PowerShell.Commands.WebRequestSession
  $login4 = Call "POST" "/api/auth/login" @{ password = $Password } -sess $sess4
  $csrf4 = $login4.Body.csrf_token
  $pwd = Call "POST" "/api/auth/password" @{ current = $Password; new = "Berthio#2027"; confirm = "Berthio#2027" } -sess $sess4 -headers @{ "X-Berthio-CSRF" = $csrf4 }
  Check ($pwd.Status -eq 200 -and $pwd.Body.sessions_revoked -ge 1) "改口令后所有会话失效"
  $oldSess = Call "GET" "/api/panel" -sess $sess4
  Check ($oldSess.Status -eq 401) "改口令后旧会话 401"
} finally {
  if ($script:panel) { Stop-Process -Id $script:panel.Id -Force -ErrorAction SilentlyContinue }
  $env:BERTHIO_HOME = $null; $env:BERTHIO_LISTEN = $null
  $env:BERTHIO_SESSION_TTL_SECS = $null; $env:BERTHIO_LOGIN_MAX_FAILS = $null; $env:BERTHIO_LOGIN_LOCK_SECS = $null
  Remove-Item -Recurse -Force $bh -ErrorAction SilentlyContinue
}

Write-Host ""
if ($script:fail -eq 0) { Write-Host "M3 鉴权回归：全部通过" } else { Write-Host ("M3 鉴权回归：{0} 项失败" -f $script:fail) }
exit $(if ($script:fail -eq 0) { 0 } else { 1 })
