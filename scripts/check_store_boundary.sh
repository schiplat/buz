#!/usr/bin/env bash
# Store-boundary gate: Manager SQL lives only in crates/manager/src/store.
#
# Handlers (api/) may not build SQL. Files still carrying SQL are listed in
# scripts/store_boundary_allowlist.txt; the list shrinks with every migration
# step and must reach empty. A file that appears in api/ SQL hits but is NOT in
# the allowlist fails this gate.
set -euo pipefail
cd "$(dirname "$0")/.."

allowlist="scripts/store_boundary_allowlist.txt"
violators=()
stale=()

for f in crates/manager/src/api/*.rs; do
  if grep -qE 'sqlx::(query|query_scalar|query_as)|SqlitePool|SqliteRow|fetch_all|fetch_one|fetch_optional|fetch_one|begin_with|\.execute\(' "$f"; then
    base="$(basename "$f")"
    if grep -qxF "$base" "$allowlist"; then
      : # known, still to migrate
    else
      violators+=("$f")
    fi
  fi
done

# Allowlist entries whose file no longer matches are removed on sight.
while IFS= read -r base; do
  [ -z "$base" ] && continue
  f="crates/manager/src/api/$base"
  if [ ! -f "$f" ] || ! grep -qE 'sqlx::(query|query_scalar|query_as)|SqlitePool|SqliteRow|fetch_all|fetch_one|fetch_optional|begin_with|\.execute\(' "$f"; then
    stale+=("$base")
  fi
done < "$allowlist"

if [ "${#violators[@]}" -gt 0 ]; then
  echo "ERROR: SQL outside the store boundary (add to $allowlist only while migrating):"
  printf '  %s\n' "${violators[@]}"
  exit 1
fi

if [ "${#stale[@]}" -gt 0 ]; then
  echo "ERROR: allowlist is stale — remove these entries (files are already clean):"
  printf '  %s\n' "${stale[@]}"
  exit 1
fi

echo "store boundary ok ($(grep -c . "$allowlist" || true) api file(s) still migrating)"
