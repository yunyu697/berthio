# 把运行中的 demo 服务快照成一个纯静态单文件 HTML（CSS/JS 全内联，无 CDN、无外部字体）。
# 用的是真实前端代码（web/index.html + web/app.js），只把数据换成内置假数据，不另写一套 mock。
#
# 用法（先启动 demo 服务）：
#   .\target\release\berthio.exe --demo
#   .\scripts\build-demo.ps1 -Milestone m1
param(
  [string]$Milestone = "m1",
  [string]$BaseUrl = "http://127.0.0.1:18090",
  [string]$OutDir = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$root = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $root ("demo\" + $Milestone) }

function Get-Json([string]$path) {
  Invoke-RestMethod -Uri ($BaseUrl + $path) -Method Get
}
function Post-Json([string]$path, $obj) {
  $json = $obj | ConvertTo-Json -Compress
  Invoke-RestMethod -Uri ($BaseUrl + $path) -Method Post -ContentType 'application/json; charset=utf-8' `
    -Body ([System.Text.Encoding]::UTF8.GetBytes($json))
}

Write-Host "[demo] 采集快照: $BaseUrl"
$panel    = Get-Json "/api/panel"
$inbounds = Get-Json "/api/inbound/inbounds"
$kernels  = Get-Json "/api/kernels"
$logs     = Get-Json "/api/logs"
$defaults = Get-Json "/api/inbound/wizard/defaults"

# --- M2：真实换源下载（会真的走网络，demo 里展示的是当时的真实耗时与校验结论）---
Write-Host "[demo] 采集镜像换源结果（真实下载 + SHA-256 校验）"
function Fetch([string]$id, [string]$format, [string]$source) {
  Post-Json "/api/rulesets/fetch" @{ id = $id; format = $format; source = $source }
}
$fetchResults = [ordered]@{}
$fetchResults["geoip:private|mihomo|official"] = Fetch "geoip:private" "mihomo" "official"
$fetchResults["geoip:private|mihomo|auto"] = Fetch "geoip:private" "mihomo" "auto"
$fetchResults["geoip:private|singbox|official"] = Fetch "geoip:private" "singbox" "official"
$fetchResults["geoip:private|singbox|ghproxy-net"] = Fetch "geoip:private" "singbox" "ghproxy-net"
$fetchResults["geosite:youtube|mihomo|official"] = Fetch "geosite:youtube" "mihomo" "official"
$fetchResults["geosite:youtube|mihomo|jsdelivr"] = Fetch "geosite:youtube" "mihomo" "jsdelivr"

# 每个启用源的测速结论（写入 last 状态，界面“最近状态”列用真实数据）
$sourcesNow = Get-Json "/api/rulesets/sources"
$sourceTests = [ordered]@{}
foreach ($s in $sourcesNow.sources) {
  if (-not $s.enabled) { continue }
  $sourceTests[$s.id] = Post-Json "/api/rulesets/sources/test" @{ id = $s.id }
}
$sources = Get-Json "/api/rulesets/sources"
$rulesets = Get-Json "/api/rulesets"

# 每个入站的客户端单独取，拼成 clients 映射
$clients = @{}
foreach ($item in $inbounds.items) {
  $r = Get-Json ("/api/inbound/inbounds/" + $item.id + "/clients")
  $clients[$item.id] = $r.items
}

$ids = ($inbounds.items | Where-Object { $_.enabled } | Select-Object -First 2 | ForEach-Object { $_.id }) -join ","
$preview = Get-Json ("/api/export/preview?format=clash&scenario=desktop&bucket=112&inbound_ids=" + $ids)

# 强制标注演示数据
$panel.demo = $true
$panel.demo_label = "演示数据"

# 脱敏：demo 会随 Release 公开，不能带真实本机路径 / 用户名
$realHome = $panel.home
$panel.home = "/demo/home"
foreach ($b in $kernels.binaries) {
  if ($b.path) {
    $leaf = Split-Path -Leaf $b.path
    $b.path = "/demo/kernel/" + $b.name + "/" + $(if ($b.version) { $b.version } else { "unknown" }) + "/" + $leaf
  }
}
foreach ($layer in $preview.validation.layers) {
  if ($layer.command) { $layer.command = $layer.command.Replace($realHome, "/demo/home") }
  if ($layer.output)  { $layer.output  = $layer.output.Replace($realHome, "/demo/home") }
}

$fixture = [ordered]@{
  panel         = $panel
  inbounds      = $inbounds
  rulesets      = $rulesets
  kernels       = $kernels
  logs          = $logs
  defaults      = $defaults
  clients       = $clients
  exportPreview = $preview
  sources       = $sources
  sourceTests   = $sourceTests
  fetchResults  = $fetchResults
}
$fixtureJson = $fixture | ConvertTo-Json -Depth 24 -Compress
# 防止 JSON 串里出现 </script> 提前闭合
$fixtureJson = $fixtureJson -replace '<', '\u003c' -replace '>', '\u003e' -replace '&', '\u0026'

$htmlPath = Join-Path $root "web\index.html"
$appPath  = Join-Path $root "web\app.js"
$html = [System.IO.File]::ReadAllText($htmlPath)
$app  = [System.IO.File]::ReadAllText($appPath)

$marker = '<script src="/app.js"></script>'
if (-not $html.Contains($marker)) { throw "web/index.html 里找不到 $marker，请先确认设计稿接入点" }

$inline = @"
<script>window.__BERTHIO_DEMO_DATA__ = $fixtureJson;</script>
<script>
$app
</script>
"@

$html = $html.Replace($marker, $inline)
# 静态 demo 去掉对后端的健康检查提示（没有后端，但代码已走 DEMO 分支）
$html = $html.Replace('<title>', '<title>[演示数据] ')

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$out = Join-Path $OutDir "index.html"
[System.IO.File]::WriteAllText($out, $html, (New-Object System.Text.UTF8Encoding($false)))

$size = (Get-Item $out).Length
Write-Host ("[demo] 已生成 {0}（{1:N0} KB，单文件；双击即可打开）" -f $out, ($size / 1024))
$clientTotal = ($inbounds.items | ForEach-Object { $_.clients } | Measure-Object -Sum).Sum
Write-Host ("[demo] 内置入站 {0} 条 / 客户端 {1} 个 / 校验 {2}" -f $inbounds.items.Count, $clientTotal, $preview.validation.overall)
