#!/usr/bin/env bash
# Assemble a deployable archive: binaries + config templates + dashboard dist.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="${VERSION:-$(
  awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^version[[:space:]]*=/{gsub(/"/,"",$3); print $3; exit}' Cargo.toml
)}"
VERSION="${VERSION:-0.0.0}"
TARGET="${TARGET:-}"
ARCHIVE_NAME="${ARCHIVE_NAME:-}"
BIN_DIR="${BIN_DIR:-}"

if [[ -n "$TARGET" ]]; then
  BIN_DIR="${BIN_DIR:-$ROOT/target/${TARGET}/release}"
  ARCHIVE_NAME="${ARCHIVE_NAME:-buz-${VERSION}-${TARGET}}"
else
  BIN_DIR="${BIN_DIR:-$ROOT/target/release}"
  ARCHIVE_NAME="${ARCHIVE_NAME:-buz-${VERSION}-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)}"
fi

OUT_ROOT="${OUT_ROOT:-$ROOT/dist/release}"
STAGE="${OUT_ROOT}/${ARCHIVE_NAME}"

need() {
  local p="$1"
  [[ -f "$p" ]] || { echo "missing: $p" >&2; exit 1; }
}

need "$BIN_DIR/buz-manager"
need "$BIN_DIR/buz-runner"
need "$BIN_DIR/buz-sandboxd"
need "$ROOT/deploy/conf/manager.toml"
need "$ROOT/deploy/conf/runner.toml"
need "$ROOT/deploy/conf/sandboxd.toml"

rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/conf" "$STAGE/data" "$STAGE/pipelines"

cp -a "$BIN_DIR/buz-manager" "$BIN_DIR/buz-runner" "$BIN_DIR/buz-sandboxd" "$STAGE/bin/"
chmod +x "$STAGE/bin/buz-manager" "$STAGE/bin/buz-runner" "$STAGE/bin/buz-sandboxd"
# Best-effort strip (may be a no-op for foreign targets)
strip "$STAGE/bin/buz-manager" "$STAGE/bin/buz-runner" "$STAGE/bin/buz-sandboxd" 2>/dev/null || true

cp -a "$ROOT/deploy/conf/manager.toml" "$ROOT/deploy/conf/runner.toml" "$ROOT/deploy/conf/sandboxd.toml" "$STAGE/conf/"
cp -a "$ROOT/deploy/README.md" "$STAGE/README.md"
# UI is embedded in buz-manager — no dashboard/ directory in the archive.

# Never ship a usable default JWT secret — require BUZ_JWT_SECRET (or edit) at deploy time.
STAGE_MANAGER_TOML="$STAGE/conf/manager.toml" python3 - <<'PY'
from pathlib import Path
import os, re, sys

p = Path(os.environ["STAGE_MANAGER_TOML"])
text = p.read_text(encoding="utf-8")
text2, n = re.subn(
    r"(?m)^jwt_secret\s*=\s*.*$",
    'jwt_secret = ""  # set via BUZ_JWT_SECRET or edit before start',
    text,
)
if n != 1:
    sys.exit(f"package-release: expected exactly one jwt_secret line, found {n}")
p.write_text(text2, encoding="utf-8")

m = re.search(r'(?m)^jwt_secret\s*=\s*"(.*)"', text2)
if not m or m.group(1) != "":
    sys.exit("package-release: staged jwt_secret must be empty string")
print("package-release: jwt_secret staging OK")
PY

# Refuse packaging if known demo hex appears as a TOML value (not comments)
if grep -qE '^\s*[a-zA-Z0-9_]+\s*=\s*".*b5c8fb3a' "$STAGE/conf"/*.toml 2>/dev/null; then
  echo "refusing to package: staged conf still contains demo secret material" >&2
  exit 1
fi

mkdir -p "$OUT_ROOT"
ARCHIVE_PATH="${OUT_ROOT}/${ARCHIVE_NAME}.tar.gz"
tar -C "$OUT_ROOT" -czf "$ARCHIVE_PATH" "$ARCHIVE_NAME"

# Checksum next to archive (Linux: sha256sum, macOS: shasum)
(
  cd "$OUT_ROOT"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${ARCHIVE_NAME}.tar.gz" > "${ARCHIVE_NAME}.tar.gz.sha256"
  else
    shasum -a 256 "${ARCHIVE_NAME}.tar.gz" > "${ARCHIVE_NAME}.tar.gz.sha256"
  fi
)

echo "Packed: $ARCHIVE_PATH"
ls -lh "$ARCHIVE_PATH" "${ARCHIVE_PATH}.sha256"
