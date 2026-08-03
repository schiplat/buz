#!/usr/bin/env bash
# Standalone entrypoint: run Manager and/or Runner in one container.
#
# Expects Manager [standalone] enabled + Runner standalone = true so the
# co-located Runner can register over loopback without register_token
# (name must be in standalone.auto_approve or created in the UI).
#
# BUZ_MODE:
#   all      (default) — Manager + Runner (Runner uses http://127.0.0.1:8080)
#   manager  — Manager only
#   runner   — Runner only
set -euo pipefail

MODE="${BUZ_MODE:-all}"
MANAGER_BIN="${BUZ_MANAGER_BIN:-/app/buz/buz-manager}"
RUNNER_BIN="${BUZ_RUNNER_BIN:-/app/buz/buz-runner}"
MANAGER_CONF="${BUZ_MANAGER_CONF:-/app/buz/conf/manager.toml}"
RUNNER_CONF="${BUZ_RUNNER_CONF:-/app/buz/conf/runner.toml}"

manager_pid=""
runner_pid=""

cleanup() {
  local code=$?
  [[ -n "${runner_pid}" ]] && kill "${runner_pid}" 2>/dev/null || true
  [[ -n "${manager_pid}" ]] && kill "${manager_pid}" 2>/dev/null || true
  wait 2>/dev/null || true
  exit "${code}"
}
trap cleanup EXIT INT TERM

wait_manager() {
  local i
  for i in $(seq 1 60); do
    if (echo >/dev/tcp/127.0.0.1/8080) >/dev/null 2>&1; then
      echo "standalone: Manager is accepting connections on :8080"
      return 0
    fi
    sleep 1
  done
  echo "standalone: Manager did not become ready within 60s" >&2
  return 1
}

start_manager() {
  echo "standalone: starting Manager (${MANAGER_CONF})"
  "${MANAGER_BIN}" -c "${MANAGER_CONF}" &
  manager_pid=$!
}

start_runner() {
  echo "standalone: starting Runner (${RUNNER_CONF})"
  "${RUNNER_BIN}" -c "${RUNNER_CONF}" &
  runner_pid=$!
}

case "${MODE}" in
  manager)
    start_manager
    wait "${manager_pid}"
    ;;
  runner)
    start_runner
    wait "${runner_pid}"
    ;;
  all)
    start_manager
    wait_manager
    start_runner
    # Exit if either process dies.
    while true; do
      if ! kill -0 "${manager_pid}" 2>/dev/null; then
        echo "standalone: Manager exited" >&2
        exit 1
      fi
      if ! kill -0 "${runner_pid}" 2>/dev/null; then
        echo "standalone: Runner exited" >&2
        exit 1
      fi
      sleep 2
    done
    ;;
  *)
    echo "standalone: unknown BUZ_MODE='${MODE}' (expected: all|manager|runner)" >&2
    exit 2
    ;;
esac
