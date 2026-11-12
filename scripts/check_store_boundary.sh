#!/usr/bin/env bash
# Store-boundary gate: Manager SQL lives only in crates/manager/src/store.
#
# Handlers (api/) may not *speak SQL*. Files still carrying statement text are
# listed in scripts/store_boundary_allowlist.txt; the list shrinks with every
# migration step and must reach empty unprotected.
#
# What counts as a hit — SQL text, statement construction, or execution:
#   "SELECT … / "INSERT … / "UPDATE … / "DELETE … / "WITH …   (raw statement)
#   sqlx::query( / query_scalar( / query_as(                   (build one)
#   .execute(                                                 (run one)
#   begin_with(                                               (dialect lock)
#
# Not a hit: `sqlx::SqlitePool` / `SqliteRow` in type position, or
# `fetch_all/fetch_one/fetch_optional`. A handler may own a `&SqlitePool` and
# pass it to store operations, and may shape rows the store returns; what it
# may not do is write the SQL itself. Wrapping the dialect token belongs in
# `store/tx.rs`, so `begin_with` is still caught here.
set -euo pipefail
cd "$(dirname "$0")/.."

allowlist="scripts/store_boundary_allowlist.txt"

PATTERN='"(\s*)(SELECT|INSERT|UPDATE|DELETE|WITH)\s|sqlx::(query|query_scalar|query_as)[<(]|\.execute\(|begin_with\('

violators=()
stale=()

for f in crates/manager/src/api/*.rs; do
  if grep -qE "$PATTERN" "$f"; then
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
  if [ ! -f "$f" ] || ! grep -qE "$PATTERN" "$f"; then
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
