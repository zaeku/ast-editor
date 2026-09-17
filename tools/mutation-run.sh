#!/usr/bin/env bash
# A mutation run on a rented Colab session: allocate, build, run, collect.
#
# The run happens inside the remote kernel and the results are read through the
# Jupyter contents API, which answers while a cell is executing. The local
# `colab exec` is released once the cell is confirmed running, because it holds
# a core at 99 percent for as long as it is attached.
set -euo pipefail

SESSION=${SESSION:-mutants}
# Measured 2026-09-14: a mutant costs about 4.8 seconds at 12 vCPU and about 50
# at 2, so a CPU runtime is for smoke tests and nothing else. A100 has allocated
# on the first attempt every time; tpu v6e1 is the widest on offer at 44 vCPU
# and the one that most often will not allocate at all. Empty means CPU.
#
# HARDWARE names what to ask for, in order. The first that allocates is used,
# and each is tried ATTEMPTS_EACH times, so the widest machine is asked for
# first without the run waiting on it when it is not there.
HARDWARE=${HARDWARE-tpu v6e1|gpu A100}
REPO=${REPO:-https://github.com/zaeku/ast-editor.git}
REF=${REF:-}
# Jobs per machine, because the right number is a fraction of the vCPU it has.
# Measured 2026-09-15: 819 mutants in 2483s at 8 jobs on 12 vCPU. Raising it
# past the machine buys timeouts rather than throughput, and a timeout is not a
# survivor but it is not an answer either.
JOBS_TPU=${JOBS_TPU:-24}
JOBS_GPU=${JOBS_GPU:-8}
JOBS=${JOBS:-}
EXTRA=${EXTRA:-}
BUILD_JOBS=${BUILD_JOBS:-5}
POLL=${POLL:-60}
# The run reports its own elapsed seconds, and that is what this caps: a laptop
# asleep for an hour has not spent any of it, and a kernel wedged for an hour
# has. Zero disables the cap.
CAP=${CAP:-10800}
# Polls whose counts did not move. A read that fails says nothing — the file
# being unreachable is a fact about this machine — so only a heartbeat that
# arrives and stands still says the run stopped working.
STALL=${STALL:-40}
OUT=${OUT:-mutants.out}

REMOTE=/content/ast-editor
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

note() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }
fail() { printf >&2 'mutation-run: %s\n' "$*"; exit 1; }

command -v colab >/dev/null 2>&1 ||
    fail "colab is not on PATH. It is a uv tool: uv tool install google-colab-cli"

CLI_PYTHON=$(dirname "$(readlink -f "$(command -v colab)")")/python
[ -x "$CLI_PYTHON" ] || fail "cannot find the interpreter colab-cli runs under"

# An assignment whose local record is gone is unreachable by name and still
# holds a slot, so the next allocation fails with TooManyAssignments.
reap() {
    # A reap releases every assignment no local record names, which is another
    # shard's session during the seconds between its allocation and its record.
    # A launcher running shards side by side reaps once for all of them and
    # sets REAP=0 here.
    [ "${REAP:-1}" = 1 ] || return 0
    "$CLI_PYTHON" - <<'PY' 2>/dev/null || true
from colab_cli.common import state
sessions, assignments = state.sync_sessions()
named = {s.endpoint for s in sessions.values()}
for a in assignments:
    if a.endpoint not in named:
        state.client.unassign(a.endpoint)
        print("reaped", a.endpoint)
PY
}

release() {
    colab stop -s "$SESSION" >/dev/null 2>&1 || true
    reap
    note "session released"
}

# Pull one remote file. A failure is reported rather than leaving the previous
# copy in place to be read as fresh.
pull() {
    colab download -s "$SESSION" "$1" "$2" >/dev/null 2>&1
}

note "reaping stale assignments"
reap

note "allocating $SESSION, asking for ${HARDWARE:-cpu} in that order"
# Registered before the first attempt: an allocation that times out locally may
# have succeeded on the server, and this script ends in more ways than it
# returns from.
teardown() {
    if [ -n "$ABANDON" ]; then
        note "leaving $SESSION up: $ABANDON"
        note "  watch it:   colab download -s $SESSION /content/heartbeat.txt /dev/stdout"
        note "  collect it: colab download -s $SESSION $REMOTE/mutants.out/missed.txt $OUT/missed.txt"
        note "  stop it:    colab stop -s $SESSION"
    else
        release
    fi
    rm -rf "$WORK"
}
trap teardown EXIT

# The run outlives this script by design, so a release has to mean the script
# decided on one. Set while the kernel is working, it says the session is worth
# more than the tidiness of stopping it, and the trap says how to reach it.
ABANDON=

allocated=
chosen=
IFS='|' read -r -a wanted <<<"$HARDWARE"
for want in "${wanted[@]}"; do
    for attempt in $(seq 1 "${ATTEMPTS_EACH:-2}"); do
        # shellcheck disable=SC2086
        if colab new -s "$SESSION" ${want:+--$want} >"$WORK/new.log" 2>&1; then
            allocated=yes
            chosen=$want
            break 2
        fi
        note "${want:-cpu} attempt $attempt: $(grep -E '^[A-Za-z]+(Error|Timeout):' "$WORK/new.log" | tail -1)"
        reap
        sleep 30
    done
    note "${want:-cpu} did not allocate; asking for the next"
done
[ -n "$allocated" ] || fail "none of ${HARDWARE:-cpu} allocated. An attempt that
  times out locally has often allocated anyway, and reap releases it, so the
  hardware is busy rather than the request being wrong. Try again, or set
  HARDWARE."
note "allocated ${chosen:-cpu}"

# The number of jobs follows the machine that answered rather than the machine
# that was asked for first.
if [ -z "$JOBS" ]; then
    case $chosen in
    tpu*) JOBS=$JOBS_TPU ;;
    *) JOBS=$JOBS_GPU ;;
    esac
fi
note "running with $JOBS jobs"

cat > "$WORK/setup.py" <<PY
import subprocess
script = r'''
set -eu
export CARGO_HOME=/content/.cargo RUSTUP_HOME=/content/.rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
    sh -s -- -y --default-toolchain stable --profile minimal --component clippy,rustfmt >/dev/null
. \$CARGO_HOME/env
curl -L --proto '=https' --tlsv1.2 -sSf \
    https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh |
    bash >/dev/null 2>&1
cargo binstall -y just cargo-mutants >/dev/null 2>&1
rm -rf $REMOTE
git clone --depth 1 $REPO $REMOTE
cd $REMOTE
${REF:+git fetch --depth 1 origin $REF && git checkout FETCH_HEAD}
git rev-parse HEAD > /content/commit.txt
rustc --version; cargo mutants --version; cat /content/commit.txt
# Warm the dependency build once, so each mutant rebuilds this crate alone.
cargo build --tests 2>&1 | tail -1
# Written last, and it is what says the setup finished. Nothing before it does.
echo ok > /content/setup.done
'''
done = subprocess.run(["bash", "-lc", script], capture_output=True, text=True)
print(done.stdout[-1200:])
if done.returncode:
    print(done.stderr[-2000:])
    raise SystemExit(done.returncode)
PY

note "installing the toolchain and warming the build"
# The exec's exit status reports on its websocket, not on the work: it times out
# waiting for a reply while the kernel is still going. The file the setup writes
# at its end is what says the setup happened.
colab exec -s "$SESSION" -f "$WORK/setup.py" >"$WORK/setup.log" 2>&1 || true
for _ in $(seq 1 60); do
    pull /content/setup.done "$WORK/setup.done" && break
    sleep 30
done
[ -f "$WORK/setup.done" ] || fail "the setup left no marker after 30 minutes:
$(tail -5 "$WORK/setup.log" | sed 's/^/  /')"

cat > "$WORK/run.py" <<PY
# Runs in the kernel and keeps running when the websocket goes away, so the
# heartbeat is what says the cell is alive rather than the local process.
import subprocess, threading, time, os, datetime, json

WORK = "$REMOTE"
stop = threading.Event()
start = time.time()

def beat():
    while not stop.is_set():
        counts = {}
        for name in ("caught", "missed", "timeout", "unviable"):
            try:
                counts[name] = sum(1 for _ in open(f"{WORK}/mutants.out/{name}.txt"))
            except OSError:
                counts[name] = 0
        stamp = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
        with open("/content/heartbeat.txt", "w") as out:
            out.write(f"{stamp} up={int(time.time()-start)}s {json.dumps(counts)}\n")
        stop.wait(15)

threading.Thread(target=beat, daemon=True).start()
env = dict(os.environ, CARGO_HOME="/content/.cargo", RUSTUP_HOME="/content/.rustup",
           CARGO_BUILD_JOBS="$BUILD_JOBS", PATH="/content/.cargo/bin:" + os.environ["PATH"])
with open("/content/mutants.log", "w") as log:
    code = subprocess.call(["cargo", "mutants", "-j", "$JOBS",
                            "--timeout-multiplier", "5", *"$EXTRA".split()],
                           cwd=WORK, env=env, stdout=log, stderr=subprocess.STDOUT)
stop.set()
with open("/content/mutants.done", "w") as out:
    out.write(f"rc={code} wall={int(time.time()-start)}s\n")
PY

# The clone is of what the remote holds, so anything committed only here would
# not be measured — which reads as a test that killed nothing. BASE names the
# revision the clone lands on, and everything this working copy has that the
# clone does not is sent over it. `jj diff --summary` alone would send the
# uncommitted changes and nothing else, which is empty whenever the work has
# just been committed and not yet pushed — the state a measurement is usually
# asked for in.
if [ "${LOCAL:-1}" = 1 ]; then
    BASE=${BASE:-master@origin}
    changed=$(jj diff --from "$BASE" --summary 2>/dev/null | sed -n 's/^[AM] //p')
    gone=$(jj diff --from "$BASE" --summary 2>/dev/null | sed -n 's/^D //p')
    if [ -n "$gone" ]; then
        listed=$(printf '%s\n' "$gone" | sed 's/^/  /')
        fail "these files are gone since $BASE and the clone would still hold
  them, which measures code that does not ship:
$listed"
    fi
    for path in $changed; do
        [ -f "$path" ] || continue
        colab upload -s "$SESSION" "$path" "$REMOTE/$path" >/dev/null 2>&1 ||
            fail "could not send $path to the session"
        note "sent $path from the working copy"
    done
    [ -n "$changed" ] || note "the working copy matches the clone"
fi

note "starting the run in the kernel"
colab exec -s "$SESSION" -f "$WORK/run.py" >"$WORK/exec.log" 2>&1 &
EXEC_PID=$!

# The cell has to be confirmed executing before the local process is released.
# A queued request that never reached the kernel leaves nothing running at all.
for _ in $(seq 1 40); do
    pull /content/heartbeat.txt "$WORK/hb" && break
    # The exec dying is not the run dying: it times out on its own websocket
    # while the cell keeps going. Only the heartbeat answers this question.
    sleep 15
done
[ -f "$WORK/hb" ] || fail "no heartbeat after 10 minutes, so the cell never
  started. The exec said:
$(tail -5 "$WORK/exec.log" | sed 's/^/  /')"

note "cell is running: $(cat "$WORK/hb")"
kill -9 "$EXEC_PID" 2>/dev/null || true
note "local exec released; watching through the contents API"

mkdir -p "$OUT"
mkdir -p "$OUT"
# From here the kernel is doing the work and this loop only watches. Anything
# that goes wrong locally from now on leaves the session up rather than taking
# the run down with it.
ABANDON="the run was still going when the watch stopped"
misses=0
still=0
last_counts=
while true; do
    sleep "$POLL"
    if pull /content/heartbeat.txt "$WORK/hb"; then
        misses=0
        beat=$(cat "$WORK/hb")
        note "$beat"

        # The counts are what moving looks like. `up=` is a clock and rises
        # whether or not anything is happening, so it answers the cap and not
        # this.
        counts=${beat#*\{}
        if [ "$counts" = "$last_counts" ]; then
            still=$((still + 1))
            [ "$still" -lt "$STALL" ] || fail "nothing has been decided in
  $still polls and the run is still up. It is wedged rather than slow, and the
  session is left running so it can be looked at: colab stop -s $SESSION"
        else
            still=0
            last_counts=$counts
        fi

        up=${beat#*up=}
        up=${up%%s *}
        if [ "${CAP:-0}" -gt 0 ] && [ "${up:-0}" -ge "$CAP" ]; then
            fail "the run reports ${up}s, past the ${CAP}s cap. $OUT holds what
  had landed; the session is left up: colab stop -s $SESSION"
        fi
    else
        # A file that will not come is a fact about this machine's network, not
        # about the session. It costs patience and never a verdict.
        misses=$((misses + 1))
        note "could not read the heartbeat ($misses); the session is not the
  thing this proves anything about"
        continue
    fi
    for name in caught missed timeout unviable; do
        pull "$REMOTE/mutants.out/$name.txt" "$OUT/$name.txt" || true
    done
    pull /content/mutants.done "$OUT/done.txt" && break
done
# The run is over, so the session is this script's to release again.
ABANDON=

pull /content/mutants.log "$OUT/mutants.log" || true
pull /content/commit.txt "$OUT/commit.txt" || true
# Which phase decided each mutant, and which test spoke. The four lists say what
# the verdict was; this is the only thing that says why.
pull "$REMOTE/mutants.out/outcomes.json" "$OUT/outcomes.json" || true
for name in caught missed timeout unviable; do
    pull "$REMOTE/mutants.out/$name.txt" "$OUT/$name.txt" || true
done

note "finished: $(cat "$OUT/done.txt")"
for name in caught missed timeout unviable; do
    printf '  %-9s %s\n' "$name" "$(wc -l < "$OUT/$name.txt" | tr -d ' ')"
done

if [ -s "$OUT/missed.txt" ]; then
    printf '\nSurvived, and each needs a test or a written reason:\n'
    sed 's/^/  /' "$OUT/missed.txt"
    exit 1
fi
