# Linux 迁移实跑：容器内构建 Linux 二进制，对同一份 3x-ui v3.11.0 真库做只读迁移，
# 断言源库 SHA-256 前后一致、源目录不产生 -wal/-shm、导入计数与 Windows 一致。
set -eu
SRC=/data/x-ui.db
H=/work/home
JAR=/tmp/cookies.txt
BASE=http://127.0.0.1:18080
# shellcheck source=../lib/path-guard.sh
. "$(dirname "$0")/../lib/path-guard.sh"

fail=0
say() { echo "$1"; }
ck() { if [ "$1" = "1" ]; then say "PASS  $2  ${3:-}"; else say "FAIL  $2  ${3:-}"; fail=$((fail+1)); fi; }

mkdir -p "$H" /tmp
guard_rm_rf "$H" /work --apply
mkdir -p "$H"
berthio --version

before=$(sha256sum "$SRC" | cut -d' ' -f1)
before_wal=$(sha256sum /data/x-ui.db-wal 2>/dev/null | cut -d' ' -f1 || true)
before_perm=$(stat -c '%a %s' "$SRC")
before_list=$(ls -A /data | sort | tr '\n' ',')
say "source-before sha=$before perms=$before_perm files=$before_list"

BERTHIO_HOME="$H" BERTHIO_LISTEN=127.0.0.1:18080 berthio >/tmp/panel.out 2>/tmp/panel.err &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT

up=0
for _ in $(seq 1 75); do
  if curl -sf -o /dev/null "$BASE/healthz"; then up=1; break; fi
  sleep 0.2
done
ck "$up" "面板在 Linux 上启动" "$BASE/healthz"

curl -s -c "$JAR" -b "$JAR" -H 'Content-Type: application/json' \
  -d '{"password":"Berthio#2026","confirm":"Berthio#2026"}' "$BASE/api/auth/setup" >/tmp/setup.json
tok=$(curl -s -c "$JAR" -b "$JAR" -H 'Content-Type: application/json' \
  -d '{"password":"Berthio#2026"}' "$BASE/api/auth/login" | jq -r '.csrf_token // empty')
ck "$([ -n "$tok" ] && echo 1 || echo 0)" "登录并取得 CSRF"

prev=$(curl -s -b "$JAR" -c "$JAR" -H "X-Berthio-CSRF: $tok" -H 'Content-Type: application/json' \
  -d "{\"db_path\":\"$SRC\",\"label\":\"x-ui.db\"}" "$BASE/api/migrate/preview")
echo "$prev" | jq -c '{shape: .schema.shape, counts: .counts}'
pi=$(echo "$prev" | jq -r '.counts.import')
ps=$(echo "$prev" | jq -r '.counts.skip')
pc=$(echo "$prev" | jq -r '.counts.conflict')
ck "$([ "$pi" = 3 ] && [ "$ps" = 3 ] && [ "$pc" = 2 ] && echo 1 || echo 0)" \
  "Linux 预览计数 3/3/2（与 Windows 一致）" "import=$pi skip=$ps conflict=$pc"

ap=$(curl -s -b "$JAR" -c "$JAR" -H "X-Berthio-CSRF: $tok" -H 'Content-Type: application/json' \
  -d "{\"db_path\":\"$SRC\",\"label\":\"x-ui.db\",\"preview_id\":$(echo "$prev" | jq '.preview_id'),\"plan_hash\":$(echo "$prev" | jq '.plan_hash'),\"confirm\":true}" \
  "$BASE/api/migrate/apply")
imp=$(echo "$ap" | jq -r '.imported')
ck "$([ "$imp" = 3 ] && echo 1 || echo 0)" "Linux apply 导入 3 条" "imported=$imp"

total=$(curl -s -b "$JAR" -c "$JAR" "$BASE/api/inbound/inbounds" | jq -r '.items | length')
ck "$([ "$total" = 3 ] && echo 1 || echo 0)" "Linux 入站总数 3" "total=$total"

after=$(sha256sum "$SRC" | cut -d' ' -f1)
after_wal=$(sha256sum /data/x-ui.db-wal 2>/dev/null | cut -d' ' -f1 || true)
after_perm=$(stat -c '%a %s' "$SRC")
after_list=$(ls -A /data | sort | tr '\n' ',')
ck "$([ "$before" = "$after" ] && echo 1 || echo 0)" "源库 SHA-256 不变" "before=$before after=$after"
ck "$([ "$before_wal" = "$after_wal" ] && echo 1 || echo 0)" "源库 -wal SHA-256 不变"
ck "$([ "$before_perm" = "$after_perm" ] && echo 1 || echo 0)" "源库权限/大小不变" "before=$before_perm after=$after_perm"
ck "$([ "$before_list" = "$after_list" ] && echo 1 || echo 0)" "源目录无新增文件" "before=$before_list after=$after_list"
if [ -d "$H/tmp/migrate" ]; then
  ck "$([ -z "$(ls -A "$H/tmp/migrate")" ] && echo 1 || echo 0)" "tmp/migrate 已清理"
else
  ck 1 "tmp/migrate 未残留（目录不存在）"
fi

kill "$PID" 2>/dev/null || true
if [ "$fail" -gt 0 ]; then say "LINUX-MIGRATE: $fail 项失败"; exit 1; fi
say "LINUX-MIGRATE: PASS"
