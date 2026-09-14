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
HARDWARE=${HARDWARE-gpu A100}
REPO=${REPO:-https://github.com/zaeku/ast-editor.git}
REF=${REF:-}
JOBS=${JOBS:-8}
EXTRA=${EXTRA:-}
BUILD_JOBS=${BUILD_JOBS:-5}
POLL=${POLL:-60}
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

note "allocating $SESSION (${HARDWARE:-cpu})"
# Registered before the first attempt: an allocation that times out locally may
# have succeeded on the server, and this script ends in more ways than it
# returns from.
trap 'release; rm -rf "$WORK"' EXIT

allocated=
for attempt in $(seq 1 "${ATTEMPTS:-5}"); do
    # shellcheck disable=SC2086
    if colab new -s "$SESSION" ${HARDWARE:+--$HARDWARE} >"$WORK/new.log" 2>&1; then
        allocated=yes
        break
    fi
    note "attempt $attempt: $(grep -E '^[A-Za-z]+(Error|Timeout):' "$WORK/new.log" | tail -1)"
    reap
    sleep 60
done
[ -n "$allocated" ] || fail "could not allocate a session. An attempt that times
  out locally has often allocated anyway, and reap releases it, so the hardware
  is busy rather than the request being wrong. Try again, or set HARDWARE."

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
misses=0
while true; do
    sleep "$POLL"
    if pull /content/heartbeat.txt "$WORK/hb"; then
        misses=0
        note "$(cat "$WORK/hb")"
    else
        misses=$((misses + 1))
        note "no answer from the session ($misses)"
        [ "$misses" -ge 3 ] && fail "the session stopped answering; $OUT holds what had landed"
        continue
    fi
    for name in caught missed timeout unviable; do
        pull "$REMOTE/mutants.out/$name.txt" "$OUT/$name.txt" || true
    done
    pull /content/mutants.done "$OUT/done.txt" && break
done

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
