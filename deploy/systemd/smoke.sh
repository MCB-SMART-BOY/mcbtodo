#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
build_dir=$(realpath -- "${1:-$repo_root/target/debug}")
cli="$build_dir/mcbtodo"
daemon="$build_dir/mcbtodo-daemon"
fixture="$repo_root/examples/vault"
unit_id=30000000-0000-4000-8000-000000000002

if [[ ! -x "$cli" || ! -x "$daemon" ]]; then
    echo "Build both binaries first with cargo build --locked -p mcbtodo -p mcbtodo-daemon." >&2
    exit 1
fi
for dependency in jq bwrap; do
    if ! command -v "$dependency" >/dev/null; then
        printf 'The smoke test requires %s.\n' "$dependency" >&2
        exit 1
    fi
done
if ! systemctl --user show-environment >/dev/null; then
    echo "Run this from a login session with a reachable systemd user manager." >&2
    exit 1
fi

smoke_dir=$(mktemp -d /tmp/mcbtodo-smoke.XXXXXXXX)
service="mcbtodo-smoke-${smoke_dir##*.}.service"

cleanup() {
    local exit_code=$?
    if (( exit_code != 0 )); then
        systemctl --user status "$service" --no-pager >&2 || true
        journalctl --user-unit "$service" -n 30 --no-pager >&2 || true
    fi
    systemctl --user stop "$service" >/dev/null 2>&1 || true
    find "$smoke_dir" -depth -delete
}
trap cleanup EXIT

# Mount a temporary home at the normal path inside each process's namespace.
# Stage the inputs outside that mount, since the repository may be under it.
mkdir -- "$smoke_dir/workspace"
cp --reflink=auto -- "$cli" "$daemon" "$smoke_dir/"
cp -R -- "$fixture" "$smoke_dir/vault"
isolated=( "$(command -v bwrap)" --ro-bind / / --bind "$smoke_dir/workspace" "$HOME" --chdir / -- )

run_cli() {
    "${isolated[@]}" "$smoke_dir/mcbtodo" "$@"
}

wait_for_timer() {
    local predicate=$1
    local output
    local attempt
    for ((attempt = 0; attempt < 60; attempt++)); do
        if output=$(run_cli timer status --json 2>"$smoke_dir/status.err") &&
            jq -e "$predicate" <<<"$output" >/dev/null; then
            printf '%s\n' "$output"
            return 0
        fi
        sleep 0.25
    done
    printf 'Timer did not reach expected state (%s).\n%s\n' "$predicate" "$output" >&2
    cat -- "$smoke_dir/status.err" >&2
    return 1
}

run_cli init
cp -R -- "$smoke_dir/vault/." "$smoke_dir/workspace/.mcbtodo/vault/"
# Mirror the packaged unit's service settings in a transient unit.
systemd-run --user --collect --unit="$service" \
    --property=Type=exec --property=Restart=on-failure --property=RestartSec=3s \
    --property=NoNewPrivileges=yes --property=UMask=0077 \
    "${isolated[@]}" "$smoke_dir/mcbtodo-daemon"

wait_for_timer '.state == "idle" and .session == null' >/dev/null
started=$(run_cli timer start --unit "$unit_id" --json)
session_id=$(jq -er '.session.id' <<<"$started")
report_date=$(jq -er '.session.started_at[:10]' <<<"$started")
wait_for_timer '.state == "running" and .session.elapsed_seconds >= 6' >/dev/null

systemctl --user kill --signal=KILL --kill-whom=all "$service"
recovered=$(wait_for_timer '.state == "needs_review"')
jq -e --arg id "$session_id" '.session.id == $id and .session.elapsed_seconds >= 5' \
    <<<"$recovered" >/dev/null

report=$(run_cli --timezone UTC timer report --date "$report_date" --json)
jq -e --arg unit "$unit_id" --arg date "$report_date" \
    --argjson seconds "$(jq -er '.session.elapsed_seconds' <<<"$recovered")" \
    '.date == $date and .session_count == 1 and .needs_review_count == 1
    and .total_elapsed_seconds == $seconds
    and (.by_work_unit | length == 1 and .[0].work_unit_id == $unit)' \
    <<<"$report" >/dev/null
markdown=$(run_cli --timezone UTC timer report --date "$report_date")
[[ "$markdown" == *"$report_date"* && "$markdown" == *"$unit_id"* ]]
run_cli timer stop >/dev/null

echo "systemd user service smoke passed: timer checkpoint, restart, and focus report."
