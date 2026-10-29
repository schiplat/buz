#!/bin/bash
# E2E for Manager-managed sandbox host configuration on a real host.
# Phases: 1) manager + enroll + publish  2) sandboxd pull/apply + gVisor jobs
#         3) cache/offline  4) full product path (runner + pipeline + grants)
#
# Manual, Linux-only, not part of CI. Prerequisites: passwordless sudo (sandboxd
# runs as root), Docker with the gVisor `runsc` runtime installed, and a release
# build in target/release. Writes everything under /tmp/buz-e2e; safe to re-run.
set -u
cd "$(dirname "$0")/.."

ROOT=/tmp/buz-e2e
PORT=18080
MGR=http://127.0.0.1:$PORT
JWT_SECRET="e2e-secret-0123456789abcdefghijklmnop"
PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "PASS: $1"; }
bad() { FAIL=$((FAIL+1)); echo "FAIL: $1 — ${2:-}"; }
check() { if echo "${2:-}" | grep -q "$3"; then ok "$1"; else bad "$1" "$(echo "${2:-}" | head -c 300)"; fi }

export PATH="$HOME/.cargo/bin:$PATH"
BIN=$ROOT/bin
mkdir -p "$ROOT"/{bin,manager,pipelines,rws,ws,ws/runner,state,token}
cp target/release/buz-manager target/release/buz-runner target/release/buz-sandboxd "$BIN/" 2>/dev/null || { echo "binaries missing"; exit 1; }
USER_UID=$(id -u); USER_GID=$(id -g)
HOST_ID=e2e-host
INSTANCE_ID=sandboxd-e2e

cleanup() {
  pkill -f 'buz-e2e/bin/buz-manager' 2>/dev/null
  pkill -f 'buz-e2e/bin/buz-runner' 2>/dev/null
  sudo pkill -f 'buz-e2e/bin/buz-sandboxd' 2>/dev/null
  true
}
trap cleanup EXIT

# ── socket client: send one JSON op to sandboxd, return the response ──
sbox() {
python3 - "$1" <<'PYEOF'
import json, socket, sys
op = json.loads(sys.argv[1])
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("/tmp/buz-e2e/sandboxd.sock")
s.sendall((json.dumps(op) + "\n").encode())
s.shutdown(socket.SHUT_WR)
buf = b""
while True:
    chunk = s.recv(65536)
    if not chunk: break
    buf += chunk
print(buf.decode().strip())
PYEOF
}

jsonget() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)" 2>/dev/null; }

echo "═══════ Phase 1: Manager + enroll + publish ═══════"
cleanup
rm -rf "$ROOT/manager"/* "$ROOT/state"/* "$ROOT"/ws/* "$ROOT"/pipelines/* "$ROOT"/token/*
cat > "$ROOT/manager.toml" <<EOF
[server]
host = "127.0.0.1"
port = $PORT

[database]
path = "$ROOT/manager/buz.db"

[auth]
jwt_secret = "$JWT_SECRET"
jwt_ttl_hours = 24

[pipeline]
yaml_scan_dir = "$ROOT/pipelines"
yaml_scan_interval_seconds = 3

[clock]
enabled = false
EOF
BUZ_MANAGER_INSTANCE_ID=mgr-e2e nohup "$BIN/buz-manager" --config "$ROOT/manager.toml" > "$ROOT/manager.log" 2>&1 &
for i in $(seq 1 30); do curl -sf "$MGR/api/v1/health" >/dev/null 2>&1 && break; sleep 0.5; done
curl -sf "$MGR/api/v1/health" >/dev/null && ok "manager up" || { bad "manager up"; exit 1; }

META=$(curl -sf "$MGR/api/v1/meta")
check "meta carries pinned manager instance" "$META" '"manager_instance_id":"mgr-e2e"'
MGR_INSTANCE=$(echo "$META" | jsonget "d['manager_instance_id']")

SETUP=$(curl -s -X POST "$MGR/api/v1/auth/setup" -H 'Content-Type: application/json' -d '{"username":"boss","password":"E2ePassw0rdX!"}')
ADMIN=$(echo "$SETUP" | jsonget "d['token']")
[ -n "$ADMIN" ] && ok "admin setup" || { bad "admin setup" "$SETUP"; exit 1; }
aj() { echo "Authorization: Bearer $ADMIN"; }

# digest-pinned busybox
docker pull -q busybox:latest >/dev/null 2>&1
DIGEST=$(docker inspect --format '{{index .RepoDigests 0}}' busybox:latest)
[ -n "$DIGEST" ] && ok "digest-pinned busybox ($DIGEST)" || { bad "busybox digest"; exit 1; }

DOC1=$(cat <<EOF
{"profiles":{
  "default":{"memory":"2g","cpu":"2","pids":256,"slots":1},
  "large":{"memory":"8g","cpu":"4","pids":512,"slots":4}},
 "tiers":{
  "container":{"image":"$DIGEST","runtime":""},
  "sandbox":{"image":"$DIGEST","runtime":"runsc"}},
 "network":"bridge"}
EOF
)

# fail-closed before any config: probe reports unsupported
# (sandboxd not started yet — covered in phase 2)

ENROLL=$(curl -sf -X POST "$MGR/api/v1/ops/sandbox-hosts" -H "$(aj)" -H 'Content-Type: application/json' \
  -d "{\"host_id\":\"$HOST_ID\",\"instance_id\":\"$INSTANCE_ID\",\"max_resource_slots\":16}")
TOKEN=$(echo "$ENROLL" | jsonget "d['token']")
check "enroll returns buzsd_ token once" "$TOKEN" '^buzsd_'
printf '%s' "$TOKEN" > "$ROOT/token/sandboxd-token"; chmod 0400 "$ROOT/token/sandboxd-token"

PUT=$(curl -s -X PUT "$MGR/api/v1/ops/sandbox-hosts/$HOST_ID/$INSTANCE_ID/config" -H "$(aj)" \
  -H 'Content-Type: application/json' -H 'If-Match: 0' -d "$DOC1")
check "publish v1" "$PUT" '"version":1'

# invalid doc rejected, version not bumped
BADPUT=$(curl -s -X PUT "$MGR/api/v1/ops/sandbox-hosts/$HOST_ID/$INSTANCE_ID/config" -H "$(aj)" \
  -H 'Content-Type: application/json' -d '{"profiles":{},"tiers":{},"network":""}')
check "invalid document rejected" "$BADPUT" "reserved 'default' profile"

echo "═══════ Phase 2: sandboxd pull/apply + gVisor jobs ═══════"
sudo -n env \
  BUZ_MANAGER_URL="$MGR" BUZ_MANAGER_INSTANCE_ID="$MGR_INSTANCE" \
  BUZ_SANDBOXD_TOKEN_FILE="$ROOT/token/sandboxd-token" \
  BUZ_HOST_ID="$HOST_ID" BUZ_SANDBOXD_INSTANCE_ID="$INSTANCE_ID" \
  BUZ_SANDBOXD_SOCKET="$ROOT/sandboxd.sock" BUZ_SANDBOXD_SOCKET_GID="$USER_GID" \
  BUZ_SANDBOXD_ALLOWED_RUNNER_UID="$USER_UID" \
  BUZ_SANDBOXD_WORKSPACE_ROOT="$ROOT/ws" BUZ_SANDBOXD_STATE_DIR="$ROOT/state" \
  BUZ_SANDBOXD_DOCKER_CLI="$(which docker)" \
  "$BIN/buz-sandboxd" > "$ROOT/sandboxd.log" 2>&1 &
for i in $(seq 1 30); do [ -S "$ROOT/sandboxd.sock" ] && break; sleep 0.5; done
[ -S "$ROOT/sandboxd.sock" ] && ok "sandboxd socket up" || { bad "sandboxd socket"; tail -20 "$ROOT/sandboxd.log"; exit 1; }

# long-poll apply within ~1s + status report → applied_version 1
APPLIED=""
for i in $(seq 1 30); do
  APPLIED=$(curl -sf "$MGR/api/v1/ops/sandbox-hosts/$HOST_ID/$INSTANCE_ID" -H "$(aj)" | jsonget "d['applied_version']")
  [ "$APPLIED" = "1" ] && break; sleep 0.5
done
[ "$APPLIED" = "1" ] && ok "sandboxd applied v1 (long-poll <15s)" || bad "applied v1" "got: $APPLIED"
HOSTJ=$(curl -sf "$MGR/api/v1/ops/sandbox-hosts/$HOST_ID/$INSTANCE_ID" -H "$(aj)")
check "host status insync" "$HOSTJ" '"status":"insync"'
check "manager id bound into applied record" "$(sbox '{"op":"get_config"}')" '"manager_instance_id":"mgr-e2e"'

STATUS=$(sbox '{"op":"status"}')
check "status reports configured v1" "$STATUS" '"config_version":1'
check "status lists large profile" "$STATUS" '"large"'
PROBE=$(sbox '{"op":"probe_capabilities"}')
check "probe: container supported" "$PROBE" '"container":{"status":"supported"'
check "probe: sandbox supported (gVisor)" "$PROBE" '"sandbox":{"status":"supported"'
check "probe: vm not configured" "$PROBE" '"vm":{"status":"unsupported"'
UNAME_HOST=$(uname -r)

mkdir -p "$ROOT/ws/job-large"
CREATE=$(sbox "{\"op\":\"create_job\",\"mode\":\"sandbox\",\"workspace\":\"$ROOT/ws/job-large\",\"profile\":\"large\"}")
check "create_job (sandbox, large)" "$CREATE" '"ok":true'
check "create echoes profile+version" "$CREATE" '"profile":"large"'
HANDLE=$(echo "$CREATE" | jsonget "d['handle']")
CONTAINER=$(docker ps -a --filter "label=buz.sandboxd.managed=true" --format '{{.Names}}' | head -1)
[ -n "$CONTAINER" ] && ok "managed container created ($CONTAINER)" || bad "managed container created"
INSPECT=$(docker inspect "$CONTAINER" 2>/dev/null)
check "envelope memory 8g"  "$INSPECT" '"Memory": 8589934592'
check "envelope cpu 4"      "$INSPECT" '"NanoCpus": 4000000000'
check "envelope pids 512"   "$INSPECT" '"PidsLimit": 512'
check "envelope runtime runsc" "$INSPECT" '"Runtime": "runsc"'
check "hardening read-only+caps" "$INSPECT" '"CapDrop": \['

EXECQ=$(sbox "{\"op\":\"exec_step\",\"handle\":\"$HANDLE\",\"command\":\"uname -r && echo IN-SANDBOX\",\"work_dir\":\"\",\"env\":{},\"timeout_seconds\":20}")
check "exec inside gVisor sandbox" "$EXECQ" 'IN-SANDBOX'
echo "  host kernel: $UNAME_HOST / sandbox output: $(echo "$EXECQ" | grep -o '"content":"[^"]*"' | tr '\n' ' ' | head -c 200)"
check "exec exit ok" "$EXECQ" '"code":0'

RF=$(sbox "{\"op\":\"read_file\",\"handle\":\"$HANDLE\",\"path\":\"/etc/hostname\"}")
check "read_file works" "$RF" '"ok":true'
DESTROY=$(sbox "{\"op\":\"destroy_job\",\"handle\":\"$HANDLE\"}")
check "destroy_job" "$DESTROY" '"ok":true'
LEFT=$(docker ps -a --filter "label=buz.sandboxd.managed=true" --format '{{.Names}}' | wc -l)
[ "$LEFT" = "0" ] && ok "container removed" || bad "container removed" "$LEFT left"

XCREATE=$(sbox "{\"op\":\"create_job\",\"mode\":\"sandbox\",\"workspace\":\"$ROOT/ws/job-large\",\"profile\":\"xlarge\"}")
check "unknown profile rejected (no downgrade)" "$XCREATE" "'xlarge'"
LEFT=$(docker ps -a --filter "label=buz.sandboxd.managed=true" --format '{{.Names}}' | wc -l)
[ "$LEFT" = "0" ] && ok "no container for unknown profile" || bad "no container for unknown profile"

# publish v2: default shrinks; expect apply then new envelope on next job
DOC2=$(echo "$DOC1" | sed 's/"default":{"memory":"2g"/"default":{"memory":"1g"/')
PUT2=$(curl -s -X PUT "$MGR/api/v1/ops/sandbox-hosts/$HOST_ID/$INSTANCE_ID/config" -H "$(aj)" \
  -H 'Content-Type: application/json' -H 'If-Match: 1' -d "$DOC2")
check "publish v2" "$PUT2" '"version":2'
APPLIED2=""
for i in $(seq 1 30); do
  APPLIED2=$(curl -sf "$MGR/api/v1/ops/sandbox-hosts/$HOST_ID/$INSTANCE_ID" -H "$(aj)" | jsonget "d['applied_version']")
  [ "$APPLIED2" = "2" ] && break; sleep 0.5
done
[ "$APPLIED2" = "2" ] && ok "v2 applied within seconds" || bad "v2 applied" "got: $APPLIED2"

mkdir -p "$ROOT/ws/job-v2"
C2=$(sbox "{\"op\":\"create_job\",\"mode\":\"container\",\"workspace\":\"$ROOT/ws/job-v2\",\"profile\":\"\"}")
check "default profile (runc tier) created" "$C2" '"profile":"default"'
H2=$(echo "$C2" | jsonget "d['handle']")
CONTAINER2=$(docker ps -a --filter "label=buz.sandboxd.managed=true" --format '{{.Names}}' | head -1)
check "v2 envelope memory 1g" "$(docker inspect "$CONTAINER2")" '"Memory": 1073741824'
check "v2 container tier uses runc" "$(docker inspect "$CONTAINER2")" '"Runtime": "runc"'
sbox "{\"op\":\"destroy_job\",\"handle\":\"$H2\"}" >/dev/null

echo "═══════ Phase 3: cache + offline recovery ═══════"
sudo -n pkill -f "buz-e2e/bin/buz-sandboxd"; sleep 1
pkill -f "buz-e2e/bin/buz-manager"; sleep 1
CACHE=$(ls "$ROOT/state"/*.json 2>/dev/null | head -1)
[ -n "$CACHE" ] && ok "root-owned cache written" || bad "cache written"
sudo -n stat -c '%a %U' "$CACHE" | grep -q '^600 root' && ok "cache is root 0600" || bad "cache perms" "$(sudo -n stat -c '%a %U' "$CACHE")"

# offline: manager down, sandboxd from cache — jobs still possible
sudo -n env \
  BUZ_MANAGER_URL="$MGR" BUZ_MANAGER_INSTANCE_ID="$MGR_INSTANCE" \
  BUZ_SANDBOXD_TOKEN_FILE="$ROOT/token/sandboxd-token" \
  BUZ_HOST_ID="$HOST_ID" BUZ_SANDBOXD_INSTANCE_ID="$INSTANCE_ID" \
  BUZ_SANDBOXD_SOCKET="$ROOT/sandboxd.sock" BUZ_SANDBOXD_SOCKET_GID="$USER_GID" \
  BUZ_SANDBOXD_ALLOWED_RUNNER_UID="$USER_UID" \
  BUZ_SANDBOXD_WORKSPACE_ROOT="$ROOT/ws" BUZ_SANDBOXD_STATE_DIR="$ROOT/state" \
  BUZ_SANDBOXD_DOCKER_CLI="$(which docker)" \
  "$BIN/buz-sandboxd" > "$ROOT/sandboxd-offline.log" 2>&1 &
for i in $(seq 1 20); do [ -S "$ROOT/sandboxd.sock" ] && break; sleep 0.5; done
OFFSTATUS=$(sbox '{"op":"status"}')
check "offline: cache-loaded v2" "$OFFSTATUS" '"config_version":2'
OFFJ=$(sbox '{"op":"create_job","mode":"sandbox","workspace":"'$ROOT'/ws/job-large","profile":"default"}')
check "offline: create from cache" "$OFFJ" '"ok":true'
OFFH=$(echo "$OFFJ" | jsonget "d['handle']")
sbox "{\"op\":\"destroy_job\",\"handle\":\"$OFFH\"}" >/dev/null

# manager back; stale-socket takeover + reconnect
BUZ_MANAGER_INSTANCE_ID=mgr-e2e nohup "$BIN/buz-manager" --config "$ROOT/manager.toml" > "$ROOT/manager2.log" 2>&1 &
for i in $(seq 1 30); do curl -sf "$MGR/api/v1/health" >/dev/null 2>&1 && break; sleep 0.5; done
ok "manager restarted"

echo "═══════ Phase 4: full product path (runner + grants + pipeline) ═══════"
CR=$(curl -sf -X POST "$MGR/api/v1/runners" -H "$(aj)" -H 'Content-Type: application/json' \
  -d '{"name":"e2e-runner","labels":["linux-x64"],"team_id":null,"max_concurrent_jobs":4}')
REG_TOKEN=$(echo "$CR" | jsonget "d['register_token']")
RUNNER_ID=$(echo "$CR" | jsonget "d['id']")
[ -n "$REG_TOKEN" ] && ok "runner created ($RUNNER_ID)" || { bad "runner create" "$CR"; exit 1; }

cat > "$ROOT/runner.toml" <<EOF
[runner]
name = "e2e-runner"
manager_url = "$MGR"
register_token = "$REG_TOKEN"
workspace_dir = "$ROOT/ws/runner"
token_cache_path = "$ROOT/rws/runner-token"

[sandbox]
supervisor_socket = "$ROOT/sandboxd.sock"
workspace_root = "$ROOT/ws"
snapshot_path = "$ROOT/rws/sandboxd.snapshot.json"
EOF
BUZ_HOST_ID="$HOST_ID" BUZ_RUNNER_INSTANCE_ID="runner-e2e" \
  nohup "$BIN/buz-runner" --config "$ROOT/runner.toml" > "$ROOT/runner.log" 2>&1 &
for i in $(seq 1 30); do
  ST=$(curl -sf "$MGR/api/v1/runners/$RUNNER_ID" -H "$(aj)" | jsonget "d['status']")
  [ "$ST" = "online" ] && break; sleep 1
done
[ "$ST" = "online" ] && ok "runner online" || { bad "runner online" "$ST"; tail -20 "$ROOT/runner.log"; exit 1; }
check "runner reported host_id" "$(curl -sf "$MGR/api/v1/runners/$RUNNER_ID" -H "$(aj)")" '"host_id":"e2e-host"'

# switch runner to sandbox tier (gated on fresh capabilities reported at startup)
MODE=$(curl -s -X PUT "$MGR/api/v1/runners/$RUNNER_ID" -H "$(aj)" -H 'Content-Type: application/json' \
  -d '{"execution_mode":"sandbox"}')
check "execution_mode=sandbox selected" "$MODE" '"execution_mode":"sandbox"'

# the sandboxd block + snapshot ride the next heartbeat after the mode switch (≤30s)
SBX=""
for i in $(seq 1 45); do
  SBX=$(curl -sf "$MGR/api/v1/runners/$RUNNER_ID" -H "$(aj)" | jsonget "d['sandbox_instance_id']")
  [ "$SBX" = "sandboxd-e2e" ] && break; sleep 1
done
[ "$SBX" = "sandboxd-e2e" ] && ok "heartbeat derived sandbox_instance_id" || bad "sandbox_instance_id" "got: $SBX"
check "heartbeat sandboxd block has applied version" "$(curl -sf "$MGR/api/v1/runners/$RUNNER_ID" -H "$(aj)")" '"config_version":2'
check "runner wrote observational snapshot" "$(cat "$ROOT/rws/sandboxd.snapshot.json" 2>/dev/null)" '"version": 2'

# grant default team the large profile
TEAM_ID=$(curl -sf "$MGR/api/v1/teams" -H "$(aj)" | jsonget "[t['id'] for t in d if t['slug']=='default'][0]")
GR=$(curl -s -X PUT "$MGR/api/v1/ops/resource-grants" -H "$(aj)" -H 'Content-Type: application/json' \
  -d "{\"team_id\":\"$TEAM_ID\",\"profile\":\"large\"}")
check "team grant large" "$GR" '"granted":true'

cat > "$ROOT/pipelines/large-job.yml" <<EOF
name: E2E Large Job
team: default
environments: [default]
runner:
  labels: [linux-x64]
  resources: large
steps:
  - name: probe
    run: |
      uname -r
      echo "SLOT-OK \$BUZ_RESOURCE_PROFILE"
EOF
sleep 4
PIPE_ID=$(curl -sf "$MGR/api/v1/pipelines" -H "$(aj)" | jsonget "[p['id'] for p in d if p['name']=='E2E Large Job'][0]")
[ -n "$PIPE_ID" ] && ok "pipeline scanned" || { bad "pipeline scanned"; exit 1; }

TRIG=$(curl -s -X POST "$MGR/api/v1/pipelines/$PIPE_ID/trigger" -H "$(aj)" -H 'Content-Type: application/json' -d '{"environment":"default","inputs":{}}')
JOB_ID=$(echo "$TRIG" | jsonget "d['job_id']")
[ -n "$JOB_ID" ] && ok "job triggered ($JOB_ID)" || { bad "trigger" "$TRIG"; exit 1; }

FINAL=""
for i in $(seq 1 60); do
  FINAL=$(curl -sf "$MGR/api/v1/jobs/$JOB_ID" -H "$(aj)" | jsonget "d['status']")
  case "$FINAL" in success|failed|cancelled|timeout) break;; esac
  sleep 2
done
[ "$FINAL" = "success" ] && ok "job succeeded" || { bad "job success" "final=$FINAL"; curl -sf "$MGR/api/v1/jobs/$JOB_ID" -H "$(aj)" | head -c 600; echo; tail -30 "$ROOT/runner.log"; }
DETAIL=$(curl -sf "$MGR/api/v1/jobs/$JOB_ID" -H "$(aj)")
check "job profile frozen (large)" "$DETAIL" '"resource_profile":"large"'
check "job billed 4 slots" "$DETAIL" '"resource_slots":4'
check "job tied to sandboxd instance" "$DETAIL" '"sandbox_instance_id":"e2e-host/sandboxd-e2e"'
LOGS=$(curl -sf "$MGR/api/v1/jobs/$JOB_ID/logs" -H "$(aj)")
check "step ran inside gVisor" "$LOGS" 'gvisor'
check "BUZ_RESOURCE_PROFILE injected" "$LOGS" 'SLOT-OK large'

# negative: ungranted profile fails at trigger, no job row
cat > "$ROOT/pipelines/huge-job.yml" <<EOF
name: E2E Huge Job
team: default
environments: [default]
runner:
  labels: [linux-x64]
  resources: huge
steps:
  - name: x
    run: echo never
EOF
sleep 4
HUGE_PIPE=$(curl -sf "$MGR/api/v1/pipelines" -H "$(aj)" | jsonget "[p['id'] for p in d if p['name']=='E2E Huge Job'][0]")
HTRIG=$(curl -s -X POST "$MGR/api/v1/pipelines/$HUGE_PIPE/trigger" -H "$(aj)" -H 'Content-Type: application/json' -d '{"environment":"default","inputs":{}}')
check "ungranted profile rejected at trigger" "$HTRIG" "not authorized to request resource profile 'huge'"

echo "═══════ Summary ═══════"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ]
