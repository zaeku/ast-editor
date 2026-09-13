#!/usr/bin/env bash
# A full mutation run split across rented sessions, one job on each.
#
# One job is what makes the verdicts trustworthy: a run whose jobs execute at
# the same time reports mutants as killed that the suite does not kill, measured
# at 44 and at 12 vCPU and absent at 2. One job at a time is right and slow, so
# the machines are what buy the time back rather than the job count.
set -euo pipefail

SHARDS=${SHARDS:-3}
HARDWARE=${HARDWARE:-gpu A100}
JOBS=${JOBS:-1}
BUILD_JOBS=${BUILD_JOBS:-12}
OUT=${OUT:-mutants.out}
HERE=$(cd "$(dirname "$0")" && pwd)

note() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }
fail() { printf >&2 'mutation-shards: %s\n' "$*"; exit 1; }

command -v colab >/dev/null 2>&1 || fail "colab is not on PATH"
CLI_PYTHON=$(dirname "$(readlink -f "$(command -v colab)")")/python

# Once, here, for all of them: a shard reaping on its own would release a
# sibling's session in the seconds between its allocation and its local record.
note "reaping stale assignments for all $SHARDS shards"
"$CLI_PYTHON" - <<'PY' 2>/dev/null || true
from colab_cli.common import state
sessions, assignments = state.sync_sessions()
named = {s.endpoint for s in sessions.values()}
for a in assignments:
    if a.endpoint not in named:
        state.client.unassign(a.endpoint)
        print("reaped", a.endpoint)
PY

rm -rf "$OUT"
mkdir -p "$OUT"
pids=""
for k in $(seq 0 $((SHARDS - 1))); do
    # Every shard runs with identical arguments and the same total, or the
    # partitions do not line up and the union means nothing.
    REAP=0 \
    SESSION="shard-$k-of-$SHARDS" \
    OUT="$OUT/shard-$k" \
    EXTRA="--shard $k/$SHARDS ${ALSO:-}" \
    HARDWARE="$HARDWARE" JOBS="$JOBS" BUILD_JOBS="$BUILD_JOBS" \
        "$HERE/mutation-run.sh" >"$OUT/shard-$k.log" 2>&1 &
    pids="$pids $!:$k"
    note "shard $k of $SHARDS started"
    sleep 20   # stagger the allocations rather than racing for the same hardware
done

landed=""
missing=""
for entry in $pids; do
    pid=${entry%%:*}
    k=${entry##*:}
    if wait "$pid"; then landed="$landed $k"; else
        # A shard exits non-zero when it found survivors as well as when it
        # failed, so the outcome files decide, not the status.
        if [ -s "$OUT/shard-$k/done.txt" ]; then landed="$landed $k"
        else missing="$missing $k"; fi
    fi
done

commits=$(for k in $landed; do cat "$OUT/shard-$k/commit.txt" 2>/dev/null; done | sort -u | wc -l)
[ "$(echo "$commits" | tr -d ' ')" = 1 ] ||
    note "WARNING: the shards did not all run the same commit, so their union is not one measurement"

for name in caught missed timeout unviable; do
    : > "$OUT/$name.txt"
    for k in $landed; do
        cat "$OUT/shard-$k/$name.txt" >> "$OUT/$name.txt" 2>/dev/null || true
    done
done

note "shards landed:${landed:- none}${missing:+, missing:$missing}"
for name in caught missed timeout unviable; do
    printf '  %-9s %s\n' "$name" "$(wc -l < "$OUT/$name.txt" | tr -d ' ')"
done

[ -z "$missing" ] || fail "shard(s)$missing did not land, so the union is partial"

if [ -s "$OUT/missed.txt" ]; then
    printf '\nSurvived, and each needs a test or a written reason:\n'
    sort "$OUT/missed.txt" | sed 's/^/  /'
    exit 1
fi
