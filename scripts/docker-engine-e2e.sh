#!/usr/bin/env bash
# Real Docker daemon e2e for engine: docker (runner engine).
# Does not start Manager/Runner — exercises crates/runner Docker engine only.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if ! command -v docker >/dev/null 2>&1; then
  echo "error: docker CLI not found" >&2
  exit 1
fi

if ! docker info >/dev/null 2>&1; then
  echo "error: docker daemon not reachable (docker info failed)" >&2
  exit 1
fi

echo "==> Pulling alpine:3.20 (if missing)"
docker pull alpine:3.20 >/dev/null

echo "==> Running ignored Docker e2e tests"
export BUZ_DOCKER_E2E=1
cargo test -p runner docker_e2e -- --ignored --nocapture

echo "==> OK — Docker engine e2e passed"
echo "Manual product smoke (optional): import example/pipelines/docker-engine-smoke.yml"
echo "  on a runner with engine.docker_enabled=true and labels including docker."
