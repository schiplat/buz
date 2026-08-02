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
#   IMAGE_TAG=v1.3.2 ./dev-deploy.sh upgrade
#                                       # pull + recreate Manager & Runner at a new tag
#                                       # (persists BUZ_JWT_SECRET and data/ workspace)
#
# Environment overrides (all optional):
#   ACR_REGISTRY    registry.cn-hongkong.aliyuncs.com
#   ACR_NAMESPACE   example
#   IMAGE_TAG       latest  — multi-arch tag: Docker auto-selects the manifest
#                   for the VM's architecture (arm64 or amd64). To pin a single
#                   platform instead (e.g. while the merge job is still running,
#                   or to bypass a broken index) use a platform tag such as
#                   `latest-arm64` / `latest-amd64` or `1.3.0-arm64`.
#   MANAGER_PORT    8080              (host bind port -> container 8080)
#   DEPLOY_DIR      /opt/buz
#   DOCKER          docker invocation (auto-detected: `docker` when the user is
#                   in the docker group, else `sudo docker`).
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
#
# Notes:
#   * ACR_USERNAME / ACR_PASSWORD may be set for non-interactive docker login.
#   * BUZ_JWT_SECRET encrypts stored secrets/hosts credentials. It is generated
#     once and reused on later runs; losing it means re-entering stored secrets.
#   * To let Runner drive host Docker (`uses: docker` steps), uncomment the
#     docker.sock mounts in the generated compose file.
set -euo pipefail

# ── Configuration ─────────────────────────────────────────────────────────────
ACR_REGISTRY="${ACR_REGISTRY:-registry.cn-hongkong.aliyuncs.com}"
ACR_NAMESPACE="${ACR_NAMESPACE:-example}"
IMAGE_TAG="${IMAGE_TAG:-latest}"
MANAGER_PORT="${MANAGER_PORT:-8080}"
DEPLOY_DIR="${DEPLOY_DIR:-/opt/buz}"
RUNNER_NAME="${RUNNER_NAME:-linux-vm-runner}"
RUNNER_TOKEN="${RUNNER_TOKEN:-}"
DOCKER="${DOCKER:-}"
BUZ_CONTAINER_UID="${BUZ_CONTAINER_UID:-10001}"

MANAGER_IMAGE="${ACR_REGISTRY}/${ACR_NAMESPACE}/buz-manager:${IMAGE_TAG}"
RUNNER_IMAGE="${ACR_REGISTRY}/${ACR_NAMESPACE}/buz-runner:${IMAGE_TAG}"
COMPOSE_FILE="${DEPLOY_DIR}/docker-compose.yml"
ENV_FILE="${DEPLOY_DIR}/.env"
RUNNER_CONF="${DEPLOY_DIR}/conf/runner.toml"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# ── Helpers ───────────────────────────────────────────────────────────────────
# Detect a working `docker` invocation: plain docker when the current user can
# reach the daemon, otherwise fall back to `sudo docker`. Override with DOCKER.
require_docker() {
  command -v docker >/dev/null 2>&1 || die "docker is not installed on this VM"
  if [[ -z "${DOCKER}" ]]; then
    if docker info >/dev/null 2>&1; then
      DOCKER="docker"
    elif sudo docker info >/dev/null 2>&1; then
      DOCKER="sudo docker"
    else
      die "docker daemon is not reachable as 'docker' or 'sudo docker' — add your user to the docker group, or set DOCKER='sudo docker'"
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

ensure_dir() {
  if [[ ! -d "${DEPLOY_DIR}" ]]; then
    sudomkdir "${DEPLOY_DIR}"
  fi
  if [[ "$(id -u)" != "$(stat -c %u "${DEPLOY_DIR}")" ]]; then
    sudo chown "$(id -u):$(id -g)" "${DEPLOY_DIR}"
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
  cat > "${COMPOSE_FILE}" <<COMPOSE
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
    depends_on:
      - manager
    volumes:
      - ${RUNNER_CONF}:/app/buz/conf/runner.toml:ro
      - ${DEPLOY_DIR}/workspace:/app/buz/workspace
      - ${DEPLOY_DIR}/logs:/app/buz/logs
    # Optional: give Runner access to host Docker for \`uses: docker\` steps.
    # user: root
    # volumes:
    #   - /var/run/docker.sock:/var/run/docker.sock
COMPOSE
}

write_runner_conf() {
  [[ -z "${RUNNER_TOKEN}" ]] && die "RUNNER_TOKEN is required (Manager UI -> Runners -> New Runner). Re-run with RUNNER_TOKEN=..."
  say "Writing ${RUNNER_CONF} (name=${RUNNER_NAME})"
  cat > "${RUNNER_CONF}" <<TOML
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

# Obtain a platform-admin credential for `gen-runner-token`: reuse a provided
# BUZ_ADMIN_TOKEN (JWT or API token), else log in with password to get a JWT.
admin_login() {
  if [[ -n "${BUZ_ADMIN_TOKEN:-}" ]]; then
    echo "${BUZ_ADMIN_TOKEN}"
    return 0
  fi
  [[ -n "${ADMIN_USERNAME:-}" && -n "${ADMIN_PASSWORD:-}" ]] || die \
    "ADMIN_USERNAME/ADMIN_PASSWORD are required to auto-create the runner"
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

# Create the runner inside the Manager container via `gen-runner-token` and
# print name/register_token. The command only runs after the caller proves they
# hold a platform-admin credential (JWT or API token).
ensure_runner_token() {
  if [[ -n "${RUNNER_TOKEN}" ]]; then
    return
  fi
  say "No RUNNER_TOKEN set — generating one via buz-manager gen-runner-token (name=${RUNNER_NAME})"
  local admin_token
  admin_token="$(admin_login)"
  local out
  # shellcheck disable=SC2086
  out="$($DOCKER exec -e BUZ_ADMIN_TOKEN="${admin_token}" buz-manager \
    /app/buz/buz-manager \
    -c /app/buz/conf/manager.toml \
    gen-runner-token "${RUNNER_NAME}" 2>&1)" || die "failed to create runner in Manager: ${out}"
  RUNNER_TOKEN="$(printf '%s\n' "${out}" | sed -n 's/^register_token: //p')"
  [[ -n "${RUNNER_TOKEN}" ]] || die "could not parse register_token from: ${out}"
  say "Runner '${RUNNER_NAME}' created — register_token captured"
}

cmd_login() {
  require_docker
  if [[ -n "${ACR_USERNAME:-}" && -n "${ACR_PASSWORD:-}" ]]; then
    # shellcheck disable=SC2086
    echo "${ACR_PASSWORD}" | $DOCKER login "${ACR_REGISTRY}" -u "${ACR_USERNAME}" --password-stdin
  else
    # shellcheck disable=SC2086
    $DOCKER login "${ACR_REGISTRY}"
  fi
}

cmd_up() {
  require_docker
  ensure_dir
  ensure_jwt_secret
  write_compose

  say "Pulling images…"
  say "  ${MANAGER_IMAGE}"
  say "  ${RUNNER_IMAGE}"
  # shellcheck disable=SC2086
  $DOCKER pull "${MANAGER_IMAGE}"
  # shellcheck disable=SC2086
  $DOCKER pull "${RUNNER_IMAGE}"

  say "Starting Manager on :${MANAGER_PORT}"
  dc up -d manager

  say "Waiting for /health…"
  local ok=0
  if command -v curl >/dev/null 2>&1; then
    for _ in $(seq 1 30); do
      if curl -fsS "http://127.0.0.1:${MANAGER_PORT}/health" >/dev/null 2>&1; then ok=1; break; fi
      sleep 2
    done
  else
    say "curl not found on host — skipping health check; use \`${BASH_SOURCE[0]} logs\` to watch startup"
  fi
  if [[ ${ok} -eq 1 ]]; then
    say "Manager is healthy → http://127.0.0.1:${MANAGER_PORT}"
  else
    say "Manager did not respond yet — check \`${DOCKER} compose -f ${COMPOSE_FILE} logs manager\`"
  fi

  echo
  say "Next: bootstrap the Runner on this VM"
  echo "  Just run: ${BASH_SOURCE[0]} runner"
  echo "  It auto-creates runner '${RUNNER_NAME}' (requires ADMIN_USERNAME/ADMIN_PASSWORD"
  echo "  of a Manager platform admin, or pre-set RUNNER_TOKEN) and writes ${RUNNER_CONF}."
}

cmd_runner() {
  require_docker
  ensure_dir
  ensure_runner_token
  write_runner_conf

  say "Pulling ${RUNNER_IMAGE}…"
  # shellcheck disable=SC2086
  $DOCKER pull "${RUNNER_IMAGE}"

  say "Starting Runner (${RUNNER_NAME})"
  # --force-recreate: docker compose `up` may reuse a running container whose
  # embedded-DNS alias cache (manager -> buz-manager) went stale, leaving the
  # runner stuck in a restart loop ("Cannot reach Buz Manager"). Recreating the
  # container re-registers the aliases and reliably fixes it.
  dc up -d --force-recreate runner
  say "Runner started — check Manager UI → Runners for status 'online'"
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
# images and recreates the containers. Persists BUZ_JWT_SECRET (.env) and the
# bind-mounted data/workspace dirs — only the images change.
cmd_upgrade() {
  require_docker
  [[ -f "${COMPOSE_FILE}" ]] || die "not deployed yet (${COMPOSE_FILE} missing) — run 'up' first"

  say "Upgrading to ${IMAGE_TAG}"
  say "  ${MANAGER_IMAGE}"
  say "  ${RUNNER_IMAGE}"

  # Rewrite image lines; use '|' as sed delimiter so registry slashes are literal.
  sed -i "s|image: ${ACR_REGISTRY}/${ACR_NAMESPACE}/buz-manager:.*|image: ${MANAGER_IMAGE}|" "${COMPOSE_FILE}"
  sed -i "s|image: ${ACR_REGISTRY}/${ACR_NAMESPACE}/buz-runner:.*|image: ${RUNNER_IMAGE}|" "${COMPOSE_FILE}"

  # Fail loudly if the expected image lines were not found (e.g. the user
  # pointed the compose file at a different registry/namespace).
  if ! grep -q "image: ${MANAGER_IMAGE}" "${COMPOSE_FILE}" \
    || ! grep -q "image: ${RUNNER_IMAGE}" "${COMPOSE_FILE}"; then
    die "could not update the image lines in ${COMPOSE_FILE} — expected 'buz-manager:${IMAGE_TAG}' / 'buz-runner:${IMAGE_TAG}'"
  fi
  say "Compose file now references ${IMAGE_TAG}:"
  grep -E '^\s+image:' "${COMPOSE_FILE}"

  # shellcheck disable=SC2086
  $DOCKER pull "${MANAGER_IMAGE}"
  # shellcheck disable=SC2086
  $DOCKER pull "${RUNNER_IMAGE}"

  say "Recreating containers with ${IMAGE_TAG}…"
  dc up -d

  say "Upgrade complete — Manager: http://127.0.0.1:${MANAGER_PORT}"
  echo "  Verify with: ${BASH_SOURCE[0]} status"
  echo "  If the Runner stays offline, re-run: ${BASH_SOURCE[0]} runner"
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
