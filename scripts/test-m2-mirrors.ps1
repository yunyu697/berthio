# M2 回归：规则集换源下载 + 官方 SHA-256 校验 + 失败回退。
# 会对真实网络发起下载（小文件 geoip:private / geosite:youtube），并临时添加两个反例源，结束后删除。
#
# 用法：先启动服务（demo 模式即可，状态写临时目录）
#   .\target\release\berthio.exe --demo
#   .\scripts\test-m2-mirrors.ps1 -BaseUrl http://127.0.0.1:18090
param(
  [string]$BaseUrl = "http://127.0.0.1:18090"
)
$ErrorActionPreference = "Stop"
$script:fail = 0

function Post-Json([string]$path, $obj) {
  $json = $obj | ConvertTo-Json -Compress
  Invoke-RestMethod -Uri ($BaseUrl + $path) -Method Post -ContentType 'application/json; charset=utf-8' `
    -Body ([System.Text.Encoding]::UTF8.GetBytes($json))
}
function Delete-Json([string]$path) {
  Invoke-RestMethod -Uri ($BaseUrl + $path) -Method Delete
}
function Put-Json([string]$path, $obj) {
  $json = $obj | ConvertTo-Json -Compress
  Invoke-RestMethod -Uri ($BaseUrl + $path) -Method Put -ContentType 'application/json; charset=utf-8' `
    -Body ([System.Text.Encoding]::UTF8.GetBytes($json))
}
function Check([bool]$cond, [string]$name, [string]$detail = "") {
  if ($cond) { Write-Host ("PASS  {0}  {1}" -f $name, $detail) }
  else { Write-Host ("FAIL  {0}  {1}" -f $name, $detail); $script:fail++ }
}

Write-Host "== M2 镜像换源回归 @ $BaseUrl =="

# 1) 先建立两条官方基准（首次镜像校验必须先有基准）
$probe = "geoip:private"
$o1 = Post-Json "/api/rulesets/fetch" @{ id = $probe; format = "mihomo"; source = "official" }
Check ([bool]$o1.ok -and $o1.chosen.source -eq "official") "官方基准 mihomo:geoip:private" ("ms=" + $o1.total_ms)
$o2 = Post-Json "/api/rulesets/fetch" @{ id = "geosite:youtube"; format = "mihomo"; source = "official" }
Check ([bool]$o2.ok) "官方基准 mihomo:geosite:youtube" ("ms=" + $o2.total_ms)

# 2) 两条镜像真实拉取 + 与官方基准校验（严格手动模式，只走指定源）
$m1 = Post-Json "/api/rulesets/fetch" @{ id = $probe; format = "mihomo"; source = "gh-proxy" }
Check ([bool]$m1.ok -and $m1.chosen.source -eq "gh-proxy") "镜像 gh-proxy 拉取+校验" ("ms=" + $m1.total_ms + " sha=" + $m1.chosen.sha256.Substring(0, 12))
Check ($m1.chosen.sha256 -eq $m1.baseline.sha256) "gh-proxy 哈希等于官方基准"

$m2 = Post-Json "/api/rulesets/fetch" @{ id = "geosite:youtube"; format = "mihomo"; source = "jsdelivr" }
Check ([bool]$m2.ok -and $m2.chosen.source -eq "jsdelivr") "镜像 jsDelivr 拉取+校验" ("ms=" + $m2.total_ms)
Check ($m2.chosen.sha256 -eq $m2.baseline.sha256) "jsDelivr 哈希等于官方基准"

Write-Host ("      官方 ms={0} / gh-proxy ms={1} / jsDelivr ms={2}" -f $o1.total_ms, $m1.total_ms, $m2.total_ms)

# 3) 反例 A：内容错误的镜像（返回别的文件）必须被拦截，并回退到后面的正常镜像
$bad = Post-Json "/api/rulesets/sources" @{
  label = "M2 反例·错文件"; priority = 1; enabled = $true
  url_mihomo = "https://gh-proxy.com/https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/meta/geo/geoip/cn.yaml"
  url_singbox = "https://gh-proxy.com/https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/sing/geo/geoip/cn.srs"
}
$autoBad = Post-Json "/api/rulesets/fetch" @{ id = $probe; format = "mihomo"; source = "auto" }
$first = $autoBad.attempts[0]
Check ($first.status -eq "hash_mismatch") "错文件镜像被拦截" ("first=" + $first.source + " status=" + $first.status)
Check ([bool]$autoBad.ok -and $autoBad.chosen.source -ne $bad.id) "拦截后回退到正常镜像" ("chosen=" + $autoBad.chosen.source)
Check ($autoBad.chosen.sha256 -eq $autoBad.baseline.sha256) "回退结果仍然匹配官方基准"
Delete-Json ("/api/rulesets/sources/" + $bad.id) | Out-Null

# 4) 反例 B：不可达镜像（传输失败）必须被跳过
$dead = Post-Json "/api/rulesets/sources" @{
  label = "M2 反例·不可达"; priority = 1; enabled = $true
  url_mihomo = "https://127.0.0.1:9/{kind}/{id}.yaml"
  url_singbox = "https://127.0.0.1:9/{kind}/{id}.srs"
}
$autoDead = Post-Json "/api/rulesets/fetch" @{ id = $probe; format = "mihomo"; source = "auto" }
$firstDead = $autoDead.attempts[0]
Check ($firstDead.status -eq "transport" -or $firstDead.status -eq "http_error") "不可达镜像被跳过" ("status=" + $firstDead.status)
Check ([bool]$autoDead.ok) "不可达后仍成功回退" ("chosen=" + $autoDead.chosen.source)
Delete-Json ("/api/rulesets/sources/" + $dead.id) | Out-Null

# 5) 无基准的镜像必须拒绝落盘（安全性：镜像不能自证）
$noBase = Post-Json "/api/rulesets/fetch" @{ id = "geoip:cn"; format = "singbox"; source = "gh-proxy" }
Check (-not $noBase.ok -and $noBase.error.code -eq "no_baseline") "无官方基准拒绝落盘" ("error=" + $noBase.error.code)

# 6) 官方源不可删除、不可禁用
try { Delete-Json "/api/rulesets/sources/official" | Out-Null; Check $false "官方源不可删除" "未拦截" }
catch { Check $true "官方源不可删除" "已拦截" }
try {
  Post-Json "/api/rulesets/fetch" @{ id = $probe; format = "mihomo"; source = "ghfast" } | Out-Null
  Check $false "禁用源不可手动选择" "未拦截"
} catch { Check $true "禁用源不可手动选择" "已拦截" }

# 7) 收尾：清理测试期间的状态（恢复 auto）
Put-Json "/api/rulesets/sources/mode" @{ mode = "auto" } | Out-Null

if ($script:fail -gt 0) {
  Write-Host ("== 失败 {0} 项 ==" -f $script:fail)
  exit 1
}
Write-Host "== 全部通过 =="
exit 0
