#!/usr/bin/env bash
#
# dev-deploy.sh — one-shot deployment of Buz Manager + Runner on a Linux VM.
#
# Pulls buz-manager / buz-runner images from Alibaba Cloud ACR (personal) and
# runs them with docker compose. Designed for a Linux VM on macOS (arm64 or
# x86_64 — the images are multi-arch, the platform is picked up automatically).
#
# Runs as a non-root user with sudo: every docker invocation is auto-prefixed
# with `sudo` when the current user cannot reach the docker daemon directly
# (set DOCKER to override). DEPLOY_DIR is created with sudo when needed, and
# the bind-mounted data dirs are chown'ed to the in-container uid (10001) so
# the buz-manager / buz-runner containers (which run as USER buz) can write.
#
# Quick start:
#   ./dev-deploy.sh login               # docker login to ACR (interactive)
#   ./dev-deploy.sh up                  # pull + start Manager (prints Runner bootstrap steps)
#   ./dev-deploy.sh runner              # start Runner; register_token is auto-generated
#   ./dev-deploy.sh status              # container + health status
#   ./dev-deploy.sh logs                # follow compose logs
#   ./dev-deploy.sh down                # stop everything (data volumes are kept)
#   IMAGE_TAG=v1.3.5 ./dev-deploy.sh upgrade
#                                       # pull + recreate Manager & Runner at a new tag;
#                                       # waits for Manager /health, then force-recreates
#                                       # Runner and verifies it can reach Manager
#                                       # (persists BUZ_JWT_SECRET and data/ workspace)
#
# Environment overrides (all optional):
#   REGISTRY        docker.io (default; set to registry.cn-hongkong.aliyuncs.com etc. for a mirror)
#   NAMESPACE       schiplat (default)
#   IMAGE_TAG       latest  — multi-arch tag: Docker auto-selects the manifest
#                   for the VM's architecture (arm64 or amd64). To pin a single
#                   platform instead (e.g. while the merge job is still running,
#                   or to bypass a broken index) use a platform tag such as
#                   `latest-arm64` / `latest-amd64` or `1.3.0-arm64`.
#   MANAGER_PORT    8080              (host bind port -> container 8080)
#   DEPLOY_DIR      /opt/buz
#   DOCKER          docker invocation (auto-detected; prefers the daemon that
#                   already runs buz-manager — often `sudo docker` if first
#                   deploy used root/sudo). Override with DOCKER='sudo docker'.
#   BUZ_CONTAINER_UID  10001  in-container uid for the buz images; bind-mounted
#                   data dirs are chown'ed to it so the containers can write.
#   BUZ_JWT_SECRET  auto-generated on first `up` and persisted in $DEPLOY_DIR/.env
#   RUNNER_NAME     linux-vm-runner   (must match the Runner name in Manager UI)
#   RUNNER_TOKEN    optional; when unset, `runner` auto-creates the runner.
#   ADMIN_USERNAME / ADMIN_PASSWORD  Manager platform-admin credentials used to
#                   obtain a JWT for `gen-runner-token`. Required when RUNNER_TOKEN
#                   is unset. A runner can only be provisioned by a platform admin.
#   BUZ_ADMIN_TOKEN optional admin credential (JWT from the auth/login API, or an
#                   API token created in Manager UI -> Access -> API tokens). Skips
#                   password login; preferred for automation since it can be revoked
#                   independently and is audited (api_token.* events).
#   SKIP_PULL       if set to 1, skip registry pull and use local images only
#                   (fails if the tag is not already present on the host).
#
# Notes:
#   * REGISTRY_USERNAME / REGISTRY_PASSWORD may be set for non-interactive docker login.
#   * BUZ_JWT_SECRET encrypts stored secrets/hosts credentials. It is generated
#     once and reused on later runs; losing it means re-entering stored secrets.
#   * To let Runner drive host Docker (`engine: docker` steps), uncomment the
#     docker.sock mounts in the generated compose file.
#   * `docker pull` always contacts the registry even when the tag exists locally.
#     On registry TLS/network blips, pull falls back to the local image when present.
set -euo pipefail

# ── Configuration ─────────────────────────────────────────────────────────────
REGISTRY="${REGISTRY:-docker.io}"
NAMESPACE="${NAMESPACE:-schiplat}"
IMAGE_TAG="${IMAGE_TAG:-latest}"
MANAGER_PORT="${MANAGER_PORT:-8080}"
DEPLOY_DIR="${DEPLOY_DIR:-/opt/buz}"
RUNNER_NAME="${RUNNER_NAME:-linux-vm-runner}"
RUNNER_TOKEN="${RUNNER_TOKEN:-}"
DOCKER="${DOCKER:-}"
BUZ_CONTAINER_UID="${BUZ_CONTAINER_UID:-10001}"
SKIP_PULL="${SKIP_PULL:-0}"

MANAGER_IMAGE="${REGISTRY}/${NAMESPACE}/buz-manager:${IMAGE_TAG}"
RUNNER_IMAGE="${REGISTRY}/${NAMESPACE}/buz-runner:${IMAGE_TAG}"
COMPOSE_FILE="${DEPLOY_DIR}/docker-compose.yml"
ENV_FILE="${DEPLOY_DIR}/.env"
RUNNER_CONF="${DEPLOY_DIR}/conf/runner.toml"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }
warn() { printf '\033[1;33mWARN:\033[0m %s\n' "$*" >&2; }

# True when the image:tag is already present on the local docker daemon.
image_present() {
  # shellcheck disable=SC2086
  $DOCKER image inspect "$1" >/dev/null 2>&1
}

# Pull an image from ACR. `docker pull` always hits the registry (even when the
# tag exists locally) to refresh the manifest — that is why a TLS timeout can
# abort upgrade despite `docker images` already listing the tag.
# Fallback: if pull fails but the image is local, continue with a warning.
# SKIP_PULL=1 skips the registry entirely (requires a local image).
pull_image() {
  local image="$1"
  if [[ "${SKIP_PULL}" == "1" ]]; then
    if image_present "${image}"; then
      say "SKIP_PULL=1 — using local ${image}"
      return 0
    fi
    die "SKIP_PULL=1 but ${image} is not present locally — pull it first or unset SKIP_PULL"
  fi
  say "Pulling ${image}…"
  # shellcheck disable=SC2086
  if $DOCKER pull "${image}"; then
    return 0
  fi
  if image_present "${image}"; then
    warn "pull failed for ${image}; continuing with local image"
    return 0
  fi
  die "failed to pull ${image} and no local copy exists"
}

# ── Helpers ───────────────────────────────────────────────────────────────────
# Detect a working `docker` invocation. Prefer the daemon that already runs
# buz-manager (rootful `sudo docker` vs rootless/group `docker` are different
# daemons — mixing them is a common cause of "Cannot reach Buz Manager" after
# upgrade). Override with DOCKER='sudo docker' or DOCKER=docker.
require_docker() {
  command -v docker >/dev/null 2>&1 || die "docker is not installed on this VM"
  if [[ -z "${DOCKER}" ]]; then
    local can_plain=0 can_sudo=0
    docker info >/dev/null 2>&1 && can_plain=1
    sudo docker info >/dev/null 2>&1 && can_sudo=1

    if (( can_plain == 0 && can_sudo == 0 )); then
      die "docker daemon is not reachable as 'docker' or 'sudo docker' — add your user to the docker group, or set DOCKER='sudo docker'"
    fi

    # Prefer the daemon that already has our Manager container (rootful vs
    # rootless). Operators who first deployed with sudo/root must keep using it.
    local plain_id="" sudo_id=""
    if (( can_plain == 1 )); then
      plain_id="$(docker inspect -f '{{.Id}}' buz-manager 2>/dev/null || true)"
    fi
    if (( can_sudo == 1 )); then
      sudo_id="$(sudo docker inspect -f '{{.Id}}' buz-manager 2>/dev/null || true)"
    fi
    if [[ -n "${plain_id}" && -n "${sudo_id}" && "${plain_id}" == "${sudo_id}" ]]; then
      # Same rootful daemon via docker group — no need for sudo.
      DOCKER="docker"
    elif [[ -n "${sudo_id}" ]]; then
      DOCKER="sudo docker"
    elif [[ -n "${plain_id}" ]]; then
      DOCKER="docker"
    elif (( can_sudo == 1 )) && (( can_plain == 1 )); then
      # Both work, no existing Manager: prefer sudo when roots differ (rootless).
      local plain_root sudo_root
      plain_root="$(docker info -f '{{.DockerRootDir}}' 2>/dev/null || true)"
      sudo_root="$(sudo docker info -f '{{.DockerRootDir}}' 2>/dev/null || true)"
      if [[ -n "${plain_root}" && -n "${sudo_root}" && "${plain_root}" != "${sudo_root}" ]]; then
        say "Detected distinct docker daemons (plain=${plain_root} vs sudo=${sudo_root})"
        say "Defaulting to sudo docker — override with DOCKER=docker if unintended"
        DOCKER="sudo docker"
      else
        DOCKER="docker"
      fi
    elif (( can_sudo == 1 )); then
      DOCKER="sudo docker"
    else
      DOCKER="docker"
    fi
    say "Using: ${DOCKER}"
  fi
  # shellcheck disable=SC2086
  $DOCKER compose version >/dev/null 2>&1 || die "docker compose v2 is required"
}

# docker compose wrapper (word-splits on purpose so DOCKER='sudo docker' works).
dc() {
  # shellcheck disable=SC2086
  $DOCKER compose -f "${COMPOSE_FILE}" "$@"
}

# mkdir that transparently falls back to sudo for root-owned parent dirs.
sudomkdir() { mkdir -p "$@" 2>/dev/null || sudo mkdir -p "$@"; }

# Replace dest with src (src is removed). Uses sudo when dest is root-owned.
install_file() {
  local src="$1" dest="$2"
  if mv "${src}" "${dest}" 2>/dev/null; then
    return 0
  fi
  sudo mv "${src}" "${dest}" || die "cannot write ${dest} — re-run with sudo, or: sudo chown -R \$(id -u):\$(id -g) ${DEPLOY_DIR}/conf ${COMPOSE_FILE}"
}

# Write stdin to dest (sudo tee when not writable).
write_file() {
  local dest="$1"
  if [[ -e "${dest}" ]] && [[ -w "${dest}" ]]; then
    cat > "${dest}"
  elif [[ ! -e "${dest}" ]] && [[ -w "$(dirname "${dest}")" ]]; then
    cat > "${dest}"
  else
    sudo tee "${dest}" >/dev/null || die "cannot write ${dest} — re-run as root/sudo"
  fi
}

ensure_dir() {
  if [[ ! -d "${DEPLOY_DIR}" ]]; then
    sudomkdir "${DEPLOY_DIR}"
  fi
  if [[ "$(id -u)" != "$(stat -c %u "${DEPLOY_DIR}")" ]]; then
    # Running as root to fix a root-owned tree is fine; otherwise reclaim for caller.
    if [[ "$(id -u)" == "0" ]] || sudo chown "$(id -u):$(id -g)" "${DEPLOY_DIR}" 2>/dev/null; then
      :
    else
      die "${DEPLOY_DIR} is not owned by $(id -un) and chown failed — re-run with sudo"
    fi
  fi
  local dirs=(conf data pipelines workspace logs)
  for d in "${dirs[@]}"; do
    local dir="${DEPLOY_DIR}/${d}"
    [[ -d "${dir}" ]] || sudomkdir "${dir}"
    if [[ "${d}" == "conf" ]]; then
      # conf stays with the deploying user so runner.toml can be (re)written.
      [[ "$(id -u)" == "$(stat -c %u "${dir}")" ]] || sudo chown "$(id -u):$(id -g)" "${dir}"
    else
      # Containers run as USER buz (uid ${BUZ_CONTAINER_UID}) — data dirs must
      # be writable by that uid even though the host user is not root.
      [[ "$(stat -c %u "${dir}")" == "${BUZ_CONTAINER_UID}" ]] || sudo chown "${BUZ_CONTAINER_UID}:${BUZ_CONTAINER_UID}" "${dir}"
    fi
  done
}

# Persist JWT secret across runs so stored credentials stay decryptable.
ensure_jwt_secret() {
  if [[ -f "${ENV_FILE}" ]] && grep -q '^BUZ_JWT_SECRET=' "${ENV_FILE}"; then
    # shellcheck disable=SC1090
    source "${ENV_FILE}"
  fi
  if [[ -z "${BUZ_JWT_SECRET:-}" ]]; then
    BUZ_JWT_SECRET="$(openssl rand -hex 32)"
    cat >> "${ENV_FILE}" <<EOF
BUZ_JWT_SECRET=${BUZ_JWT_SECRET}
EOF
    say "Generated BUZ_JWT_SECRET -> ${ENV_FILE}"
  fi
  export BUZ_JWT_SECRET
}

write_compose() {
  if [[ -f "${COMPOSE_FILE}" ]]; then
    say "compose file exists, keeping ${COMPOSE_FILE}"
    return
  fi
  say "Writing ${COMPOSE_FILE}"
  write_file "${COMPOSE_FILE}" <<COMPOSE
# Generated by dev-deploy.sh — edit with care, then \`docker compose -f ${COMPOSE_FILE} up -d\`.
services:
  manager:
    image: ${MANAGER_IMAGE}
    container_name: buz-manager
    restart: unless-stopped
    ports:
      - "${MANAGER_PORT}:8080"
    environment:
      BUZ_JWT_SECRET: \${BUZ_JWT_SECRET}
    volumes:
      - ${DEPLOY_DIR}/data:/app/buz/data
      - ${DEPLOY_DIR}/pipelines:/app/buz/pipelines
    healthcheck:
      test: ["CMD", "bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 10s

  runner:
    image: ${RUNNER_IMAGE}
    container_name: buz-runner
    restart: unless-stopped
    # Wait until Manager accepts connections before starting Runner.
    depends_on:
      manager:
        condition: service_healthy
    volumes:
      - ${RUNNER_CONF}:/app/buz/conf/runner.toml:ro
      - ${DEPLOY_DIR}/workspace:/app/buz/workspace
      - ${DEPLOY_DIR}/logs:/app/buz/logs
    # Optional: give Runner access to host Docker for \`engine: docker\` steps.
    # user: root
    # volumes:
    #   - /var/run/docker.sock:/var/run/docker.sock
COMPOSE
}

write_runner_conf() {
  [[ -z "${RUNNER_TOKEN}" ]] && die "RUNNER_TOKEN is required (Manager UI -> Runners -> New Runner). Re-run with RUNNER_TOKEN=..."
  say "Writing ${RUNNER_CONF} (name=${RUNNER_NAME})"
  write_file "${RUNNER_CONF}" <<TOML
[runner]
name = "${RUNNER_NAME}"
manager_url = "http://manager:8080"
register_token = "${RUNNER_TOKEN}"
workspace_dir = "./workspace"

[logging]
level = "info"
# file = "./logs/runner.log"
TOML
}

# Read register_token from an existing runner.toml (empty/missing → empty string).
read_runner_conf_token() {
  [[ -f "${RUNNER_CONF}" ]] || { echo ""; return 0; }
  sed -nE 's/^[[:space:]]*register_token[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p' "${RUNNER_CONF}" \
    | head -n1
}

# Fail fast before recreating Runner if the mounted conf cannot start the process.
ensure_runner_conf_has_token() {
  local token
  token="$(read_runner_conf_token)"
  if [[ -z "${token}" ]]; then
    die "register_token is missing/empty in ${RUNNER_CONF} — Runner will refuse to start.

  Quick fix when this runner already registered successfully once (Manager has its JWT):
    set any non-empty token in ${RUNNER_CONF}, e.g.
      register_token = \"reuse\"
    then:
      sudo SKIP_PULL=1 ${BASH_SOURCE[0]} runner

  Or mint a fresh runner (new name) / delete the old one in Manager UI → Runners,
  then:
      ADMIN_USERNAME=… ADMIN_PASSWORD=… sudo ${BASH_SOURCE[0]} runner"
  fi
}

# Keep same-compose container↔container reachability:
#   manager_url = http://manager:8080
#   depends_on manager with condition: service_healthy
# Migrates away from the short-lived host.docker.internal experiment when present.
ensure_runner_reachability() {
  if [[ -f "${COMPOSE_FILE}" ]] && grep -q 'host.docker.internal' "${COMPOSE_FILE}"; then
    say "Patching ${COMPOSE_FILE}: remove host.docker.internal (use compose DNS manager)"
    local tmp
    tmp="$(mktemp)"
    awk '
      /^[[:space:]]*extra_hosts:[[:space:]]*$/ {
        if ((getline n) > 0 && n ~ /host\.docker\.internal/) next
        print
        print n
        next
      }
      /host\.docker\.internal/ { next }
      { print }
    ' "${COMPOSE_FILE}" > "${tmp}"
    install_file "${tmp}" "${COMPOSE_FILE}"
  fi

  if [[ -f "${COMPOSE_FILE}" ]] && grep -qE '^\s+- manager\s*$' "${COMPOSE_FILE}"; then
    say "Patching ${COMPOSE_FILE}: depends_on manager → service_healthy"
    local tmp
    tmp="$(mktemp)"
    awk '
      /^    depends_on:$/ {
        if ((getline n) > 0 && n ~ /^      - manager[[:space:]]*$/) {
          print "    depends_on:"
          print "      manager:"
          print "        condition: service_healthy"
          next
        }
        print
        print n
        next
      }
      { print }
    ' "${COMPOSE_FILE}" > "${tmp}"
    install_file "${tmp}" "${COMPOSE_FILE}"
  fi

  if [[ -f "${RUNNER_CONF}" ]] \
    && grep -qE 'manager_url\s*=\s*"https?://host\.docker\.internal(:[0-9]+)?"' "${RUNNER_CONF}"; then
    say "Updating ${RUNNER_CONF}: manager_url → http://manager:8080"
    local rtmp
    rtmp="$(mktemp)"
    sed -E 's|manager_url *= *"https?://host\.docker\.internal(:[0-9]+)?"|manager_url = "http://manager:8080"|' \
      "${RUNNER_CONF}" > "${rtmp}"
    install_file "${rtmp}" "${RUNNER_CONF}"
    grep -qE 'manager_url[[:space:]]*=[[:space:]]*"http://manager:8080"' "${RUNNER_CONF}" \
      || die "failed to rewrite manager_url in ${RUNNER_CONF}"
  fi
}

wait_manager_healthy() {
  say "Waiting for Manager /health…"
  if ! command -v curl >/dev/null 2>&1; then
    say "curl not found on host — sleeping 8s then continuing"
    sleep 8
    return 0
  fi
  local i
  for i in $(seq 1 45); do
    if curl -fsS --max-time 3 "http://127.0.0.1:${MANAGER_PORT}/health" >/dev/null 2>&1; then
      say "Manager is healthy → http://127.0.0.1:${MANAGER_PORT}"
      return 0
    fi
    sleep 2
  done
  return 1
}

# True when recent runner logs show a successful register (no reachability error).
runner_registered_in_logs() {
  local logs status
  status="$($DOCKER inspect -f '{{.State.Status}}' buz-runner 2>/dev/null)" || return 1
  [[ "${status}" == "running" ]] || return 1
  logs="$($DOCKER logs --tail=40 buz-runner 2>&1)" || return 1
  printf '%s' "${logs}" | grep -qi 'Cannot reach Buz Manager' && return 1
  printf '%s' "${logs}" | grep -qiE 'registered with Manager|runner registered'
}

# Force-recreate runner and wait until it is online (API) or registered (logs).
start_runner_and_wait() {
  say "Starting Runner (${RUNNER_NAME})"
  dc up -d --force-recreate runner

  local use_api=0
  if [[ -n "${BUZ_ADMIN_TOKEN:-}" ]] \
    || { [[ -n "${ADMIN_USERNAME:-}" && -n "${ADMIN_PASSWORD:-}" ]]; }; then
    use_api=1
  fi

  say "Waiting for ${RUNNER_NAME} to come online…"
  local attempt _
  for attempt in 1 2 3 4 5; do
    for _ in $(seq 1 12); do
      if (( use_api )); then
        if runner_online; then
          say "Runner ${RUNNER_NAME} is online"
          return 0
        fi
      elif runner_registered_in_logs; then
        say "Runner ${RUNNER_NAME} registered (confirmed via logs)"
        return 0
      fi
      sleep 2
    done
    if (( attempt < 5 )); then
      say "Not ready yet (attempt ${attempt}/5) — force-recreating runner"
      dc up -d --force-recreate runner
      sleep 3
    fi
  done
  return 1
}

# Obtain a platform-admin credential for `gen-runner-token`: reuse a provided
# BUZ_ADMIN_TOKEN (JWT or API token), else log in with password to get a JWT.
admin_login() {
  if [[ -n "${BUZ_ADMIN_TOKEN:-}" ]]; then
    echo "${BUZ_ADMIN_TOKEN}"
    return 0
  fi
  [[ -n "${ADMIN_USERNAME:-}" && -n "${ADMIN_PASSWORD:-}" ]] || die \
    "ADMIN_USERNAME/ADMIN_PASSWORD are required to auto-create the runner

  How to fix (pick one):
    1) Provide a platform-admin credential and let the script create the runner:
         ADMIN_USERNAME='<admin>' ADMIN_PASSWORD='<password>' ./dev-deploy.sh runner
       Or, to avoid storing a password (recommended for automation), mint an API
       token in Manager UI -> Access -> API tokens and pass:
         BUZ_ADMIN_TOKEN='<api-token>' ./dev-deploy.sh runner
    2) Or create the runner in Manager UI -> Runners -> New Runner, then re-run:
         RUNNER_TOKEN='<token-from-ui>' ./dev-deploy.sh runner
    3) Or set ADMIN_PASSWORD via stdin instead of the shell env:
         read -r -s -p 'admin password: ' ADMIN_PASSWORD; export ADMIN_PASSWORD
       then ./dev-deploy.sh runner"
  say "Logging in to Manager as ${ADMIN_USERNAME}…"
  local body
  body="$(curl -fsS -X POST "http://127.0.0.1:${MANAGER_PORT}/api/v1/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"${ADMIN_USERNAME}\",\"password\":\"${ADMIN_PASSWORD}\"}")" \
    || die "Manager login failed — check ADMIN_USERNAME/ADMIN_PASSWORD"
  local token
  token="$(printf '%s' "${body}" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
  [[ -n "${token}" ]] || die "could not extract token from login response"
  echo "${token}"
}

# Resolve RUNNER_TOKEN: env → existing runner.toml → gen-runner-token.
# If the runner name already exists (common on repair/upgrade), write a
# placeholder: after the first successful register Manager clears the one-shot
# DB token and accepts re-registration without checking register_token again,
# but the Runner process still requires a non-empty value in runner.toml.
ensure_runner_token() {
  if [[ -n "${RUNNER_TOKEN}" ]]; then
    return
  fi

  local existing
  existing="$(read_runner_conf_token)"
  if [[ -n "${existing}" ]]; then
    RUNNER_TOKEN="${existing}"
    say "Reusing register_token from ${RUNNER_CONF}"
    return
  fi

  say "No RUNNER_TOKEN set — generating one via buz-manager gen-runner-token (name=${RUNNER_NAME})"
  local admin_token
  admin_token="$(admin_login)"
  local out rc=0
  # shellcheck disable=SC2086
  out="$($DOCKER exec -e BUZ_ADMIN_TOKEN="${admin_token}" buz-manager \
    /app/buz/buz-manager \
    -c /app/buz/conf/manager.toml \
    gen-runner-token "${RUNNER_NAME}" 2>&1)" || rc=$?
  if (( rc != 0 )); then
    if printf '%s' "${out}" | grep -qi 'already exists'; then
      # Re-registration path: any non-empty token satisfies local config validation.
      RUNNER_TOKEN="reuse-${RUNNER_NAME}"
      say "Runner '${RUNNER_NAME}' already exists — using placeholder register_token for re-registration"
      return
    fi
    die "failed to create runner in Manager: ${out}"
  fi
  RUNNER_TOKEN="$(printf '%s\n' "${out}" | sed -n 's/^register_token: //p')"
  [[ -n "${RUNNER_TOKEN}" ]] || die "could not parse register_token from: ${out}"
  say "Runner '${RUNNER_NAME}' created — register_token captured"
}

cmd_login() {
  require_docker
  if [[ -n "${REGISTRY_USERNAME:-}" && -n "${REGISTRY_PASSWORD:-}" ]]; then
    # shellcheck disable=SC2086
    echo "${REGISTRY_PASSWORD}" | $DOCKER login "${REGISTRY}" -u "${REGISTRY_USERNAME}" --password-stdin
  else
    # shellcheck disable=SC2086
    $DOCKER login "${REGISTRY}"
  fi
}

cmd_up() {
  require_docker
  ensure_dir
  ensure_jwt_secret
  write_compose

  say "Pulling images…"
  pull_image "${MANAGER_IMAGE}"
  pull_image "${RUNNER_IMAGE}"

  say "Starting Manager on :${MANAGER_PORT}"
  dc up -d manager

  if wait_manager_healthy; then
    :
  else
    say "Manager did not respond yet — check \`${DOCKER} compose -f ${COMPOSE_FILE} logs manager\`"
  fi

  echo
  say "Next: bootstrap the Runner on this VM"
  echo "  Just run: ${BASH_SOURCE[0]} runner"
  echo "  It auto-creates runner '${RUNNER_NAME}' with a platform-admin credential:"
  echo "    ADMIN_USERNAME='<admin>' ADMIN_PASSWORD='<password>' ./dev-deploy.sh runner"
  echo "    # or an API token (Manager UI -> Access -> API tokens):"
  echo "    BUZ_ADMIN_TOKEN='<api-token>' ./dev-deploy.sh runner"
  echo "  ...or pre-create the runner in Manager UI -> Runners -> New Runner and set:"
  echo "    RUNNER_TOKEN='<token-from-ui>' ./dev-deploy.sh runner"
  echo "  It then writes ${RUNNER_CONF}."
}

# True when the Manager API reports ${RUNNER_NAME} as online. Uses
# BUZ_ADMIN_TOKEN when set, else ADMIN_USERNAME/ADMIN_PASSWORD (same sources as
# the runner bootstrap); without any credential the check is skipped (returns
# false, the caller falls back to the "check the UI" hint).
runner_online() {
  local token="${BUZ_ADMIN_TOKEN:-}"
  if [[ -z "${token}" ]]; then
    [[ -n "${ADMIN_USERNAME:-}" && -n "${ADMIN_PASSWORD:-}" ]] || return 1
    token="$(admin_login 2>/dev/null)" || return 1
  fi
  local body
  body="$(curl -fsS --max-time 5 \
    "http://127.0.0.1:${MANAGER_PORT}/api/v1/runners" \
    -H "Authorization: Bearer ${token}" 2>/dev/null)" || return 1
  printf '%s' "${body}" | grep -q "\"name\": *\"${RUNNER_NAME}\"" \
    && printf '%s' "${body}" | grep -q '"status": *"online"'
}

cmd_runner() {
  require_docker
  ensure_dir
  ensure_runner_token
  write_runner_conf
  ensure_runner_reachability
  ensure_runner_conf_has_token

  pull_image "${RUNNER_IMAGE}"

  # --force-recreate + online wait: repairs stale Manager DNS / race after
  # Manager recreate ("Cannot reach Buz Manager").
  if ! start_runner_and_wait; then
    say "Runner not ready after 5 attempts."
    echo "  Check Manager UI → Runners, or:"
    echo "  ${DOCKER} compose -f ${COMPOSE_FILE} logs --tail=50 runner"
    echo "  If this host was first deployed with sudo/root, re-run as:"
    echo "    sudo SKIP_PULL=1 IMAGE_TAG=${IMAGE_TAG} ${BASH_SOURCE[0]} runner"
    echo "    # or: DOCKER='sudo docker' SKIP_PULL=1 ${BASH_SOURCE[0]} runner"
    exit 1
  fi
}

cmd_render() {
  ensure_dir
  ensure_jwt_secret
  write_compose
  if [[ -n "${RUNNER_TOKEN}" ]]; then
    write_runner_conf
  else
    say "Skipped runner.toml (no RUNNER_TOKEN set — use 'runner' command to auto-generate it)"
  fi
  say "Generated files:"
  ls -l "${COMPOSE_FILE}" "${ENV_FILE}" "${RUNNER_CONF}" 2>/dev/null || true
}

cmd_status() {
  require_docker
  [[ -f "${COMPOSE_FILE}" ]] || die "not deployed yet (${COMPOSE_FILE} missing)"
  dc ps
  echo
  if command -v curl >/dev/null 2>&1 && curl -fsS "http://127.0.0.1:${MANAGER_PORT}/health" >/dev/null 2>&1; then
    say "Manager health: OK (http://127.0.0.1:${MANAGER_PORT})"
  else
    say "Manager health: unreachable (or no curl on host)"
  fi
}

cmd_logs() {
  require_docker
  dc logs -f --tail=100
}
cmd_down() {
  require_docker
  dc down
}

# Upgrade the deployed Manager + Runner to the tag in IMAGE_TAG. Rewrites the
# `image:` lines in the compose file (keeps any other user edits), pulls the new
# images, force-recreates Manager (waits for /health), then force-recreates
# Runner and waits until it registers. Restores container↔container
# manager_url (http://manager:8080) if a prior host.docker.internal migration
# is still present.
cmd_upgrade() {
  require_docker
  [[ -f "${COMPOSE_FILE}" ]] || die "not deployed yet (${COMPOSE_FILE} missing) — run 'up' first"

  say "Upgrading to ${IMAGE_TAG}"
  say "  ${MANAGER_IMAGE}"
  say "  ${RUNNER_IMAGE}"

  # Rewrite image lines via temp file so root-owned compose still works under sudo.
  local ctmp
  ctmp="$(mktemp)"
  sed -E \
    -e "s|image: ${REGISTRY}/${NAMESPACE}/buz-manager:.*|image: ${MANAGER_IMAGE}|" \
    -e "s|image: ${REGISTRY}/${NAMESPACE}/buz-runner:.*|image: ${RUNNER_IMAGE}|" \
    "${COMPOSE_FILE}" > "${ctmp}"
  install_file "${ctmp}" "${COMPOSE_FILE}"

  # Fail loudly if the expected image lines were not found (e.g. the user
  # pointed the compose file at a different registry/namespace).
  if ! grep -q "image: ${MANAGER_IMAGE}" "${COMPOSE_FILE}" \
    || ! grep -q "image: ${RUNNER_IMAGE}" "${COMPOSE_FILE}"; then
    die "could not update the image lines in ${COMPOSE_FILE} — expected 'buz-manager:${IMAGE_TAG}' / 'buz-runner:${IMAGE_TAG}'"
  fi
  say "Compose file now references ${IMAGE_TAG}:"
  grep -E '^\s+image:' "${COMPOSE_FILE}"

  ensure_runner_reachability
  ensure_runner_conf_has_token

  pull_image "${MANAGER_IMAGE}"
  pull_image "${RUNNER_IMAGE}"

  say "Recreating Manager with ${IMAGE_TAG}…"
  dc up -d --force-recreate manager
  wait_manager_healthy \
    || die "Manager did not become healthy after upgrade — check: ${DOCKER} compose -f ${COMPOSE_FILE} logs --tail=80 manager"

  # Brief settle time after Manager is healthy before recreating Runner
  # (port publish / compose DNS can lag the /health success).
  say "Waiting 3s before recreating Runner…"
  sleep 3

  # Runner after Manager is healthy; force-recreate + wait (same as `runner`).
  if ! start_runner_and_wait; then
    echo
    die "Upgrade pulled ${IMAGE_TAG} but Runner did not come online.
  Check: ${DOCKER} compose -f ${COMPOSE_FILE} logs --tail=50 runner
  If first deploy used sudo/root, re-run:
    sudo SKIP_PULL=1 IMAGE_TAG=${IMAGE_TAG} ${BASH_SOURCE[0]} upgrade
    # or: DOCKER='sudo docker' SKIP_PULL=1 IMAGE_TAG=${IMAGE_TAG} ${BASH_SOURCE[0]} upgrade
  Or repair Runner only:
    sudo SKIP_PULL=1 ${BASH_SOURCE[0]} runner"
  fi

  say "Upgrade complete — Manager: http://127.0.0.1:${MANAGER_PORT}"
  echo "  Verify with: ${BASH_SOURCE[0]} status"
}

usage() {
  sed -n '2,26p' "${BASH_SOURCE[0]}"
}

# ── Dispatch ─────────────────────────────────────────────────────────────────
case "${1:-}" in
  login)  cmd_login ;;
  up)     cmd_up ;;
  runner) cmd_runner ;;
  upgrade) cmd_upgrade ;;
  render) cmd_render ;;
  status) cmd_status ;;
  logs)   cmd_logs ;;
  down)   cmd_down ;;
  help|-h|--help) usage ;;
  *) die "unknown command '${1:-}'. Try: ${BASH_SOURCE[0]} help" ;;
esac
