#!/usr/bin/env bash
# Feature-grouped test runner — avoid paying for the full workspace on every
# iteration. `all` mirrors CI (.github/workflows/ci.yml) exactly; anything
# else is a development shortcut and NOT a substitute for `all` before push.
#
# Usage:
#   scripts/test.sh list              show groups and their test targets
#   scripts/test.sh <group>           run a named group (see GROUPS below)
#   scripts/test.sh <regex>           run every test target matching a regex
#   scripts/test.sh all               CI-equivalent: fmt + cargo test --all + dashboard
#   scripts/test.sh <regex> -- <args> trailing args are passed to `cargo test`
#
# Examples:
#   scripts/test.sh runner            # manager runner_* + runner crate core
#   scripts/test.sh '^slot_scheduling_test$'
#   scripts/test.sh clock -- --nocapture
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
# Same flag CI uses, kept constant so the build cache never forks.
export RUSTFLAGS="${RUSTFLAGS:--D warnings}"

# Group → regex over test-target names (matched per crate; a target may land
# in several groups on purpose — groups are a convenience, not a partition).
# Parallel arrays (not `declare -A`) so the script also runs under macOS's
# ancient system bash 3.2.
GROUP_NAMES=(runner clock sandbox pipeline jobs auth plugins github audit)
GROUP_PATTERNS=(
  '^runner_|^instance_identity|^policy_test$|^poll_test$|^token_cache_test$'
  'clock'
  'sandbox|ops_sandbox|resource_profile_request|execution_capabilities|docker_|image_prefetch|env_render'
  'pipeline|step_|yaml|run_number|input_validation|trigger|example_pipelines'
  'jobs_stats|cancel_|log_broadcast|logs_unit|dispatch_log_mask|debug_log_cleanup|log_masking|input_masking|step_log'
  'api_token|rbac|password|^auth_|crypto|signature|registry_auth|secret_|settings|token_cache|instance_test'
  'plugin|image_registry|local_dir|^index_test$'
  'github|webhook|cron_scheduler|git_webhook'
  'audit'
)

CRATES=(shared manager runner plugin-cli)

# Enumerate a crate's test targets: tests/*.rs plus tests/*/main.rs.
targets_for() {
  local crate="$1" dir="crates/$crate/tests" t
  [ -d "$dir" ] || return 0
  for t in "$dir"/*; do
    [ -e "$t" ] || continue
    if [ -d "$t" ]; then
      [ -f "$t/main.rs" ] && basename "$t"
    else
      basename "$t" .rs
    fi
  done
}

select_targets() { # $1=regex → lines of "crate:target"
  local regex="$1" crate t
  for crate in "${CRATES[@]}"; do
    while IFS= read -r t; do
      [ -n "$t" ] || continue
      # No `grep -q` control flow here: a non-match would make the function
      # return non-zero and `set -e` would kill the caller.
      if [[ "$t" =~ $regex ]]; then
        echo "$crate:$t"
      fi
    done < <(targets_for "$crate")
  done
  return 0
}

run_group() { # $1=regex, rest=cargo passthrough args
  local regex="$1"; shift
  local selections items=() crate
  selections="$(select_targets "$regex")"
  [ -n "$selections" ] || { echo "no test targets match /$regex/"; exit 1; }
  echo "$selections" | sed 's/^/  /'
  for crate in "${CRATES[@]}"; do
    items=()
    while IFS= read -r line; do
      # `[[ == ]]` for glob matching — `[ = ]` would be a literal string compare.
      if [[ "$line" == "$crate:"* ]]; then
        items+=("${line#"$crate":}")
      fi
    done <<<"$selections"
    if [ "${#items[@]}" -gt 0 ]; then
      echo "── cargo test -p $crate $(printf '%s ' "${items[@]}")"
      cargo test -p "$crate" $(printf -- '--test %s ' "${items[@]}") "$@"
    fi
  done
}

group_pattern() { # $1=name → prints the regex, or nothing if unknown
  local i
  for i in "${!GROUP_NAMES[@]}"; do
    [ "${GROUP_NAMES[$i]}" = "$1" ] && { echo "${GROUP_PATTERNS[$i]}"; return 0; }
  done
  return 1
}

cmd="${1:-list}"
case "$cmd" in
  list)
    echo "groups:"
    local_i=0
    for name in "${GROUP_NAMES[@]}"; do
      echo "  $name  /${GROUP_PATTERNS[$local_i]}/"
      local_i=$((local_i + 1))
    done
    echo "targets per group:"
    local_i=0
    for name in "${GROUP_NAMES[@]}"; do
      echo "  [$name]"
      select_targets "${GROUP_PATTERNS[$local_i]}" | sed 's/^/    /' | sort
      local_i=$((local_i + 1))
    done
    ;;
  all)
    # CI-equivalent gate (.github/workflows/ci.yml) — required before push.
    cargo fmt --all -- --check
    cargo test --all
    (cd dashboard && npm test && npm run build)
    ;;
  --)
    shift
    cargo test "$@"
    ;;
  *)
    if regex="$(group_pattern "$cmd")"; then
      [ "$#" -gt 0 ] && [ "$1" = "--" ] && shift
      run_group "$regex" "$@"
    else
      regex="$cmd"
      shift
      run_group "$regex" "$@"
    fi
    ;;
esac
