#!/usr/bin/env bash
# A mutation run on a Linux host reached over ssh: send, bootstrap, run, collect.
#
# The run is detached from the connection with setsid, because a dropped ssh is
# not a dropped job and must not become one. What says the run happened is the
# file it writes at its end, not the exit status of any ssh — that status is
# about the connection.
set -euo pipefail

# The host is not written here. It names a machine on a private network and
# this repository is public, so it goes in `tools/mutation-ssh.env`, which is
# not tracked:
#
#     HOST=user@machine
#
# or in the environment for one run.
ENV_FILE=$(dirname "$0")/mutation-ssh.env
# The operator writes that file, so its contents are not knowable from here.
# shellcheck disable=SC1090
[ -f "$ENV_FILE" ] && . "$ENV_FILE"
HOST=${HOST:-}
[ -n "$HOST" ] || {
    printf >&2 'mutation-ssh: no HOST. Write one into %s:\n\n    HOST=user@machine\n\n' "$ENV_FILE"
    exit 1
}
# Measured 2026-09-17 on 64 vCPU: 40 jobs with 16 build threads each timed out
# on half of what it decided in the first eight minutes, because every job runs
# a build and then a test binary that spreads over the machine on its own. The
# product of the three is what oversubscribes it, so the jobs are a third of
# the cores and the builds inside them are small.
JOBS=${JOBS:-20}
BUILD_JOBS=${BUILD_JOBS:-4}
REMOTE_DIR=${REMOTE_DIR:-ast-editor-mutants}
POLL=${POLL:-30}
# The run reports its own elapsed seconds and that is what this caps, so a
# laptop asleep for an hour has spent none of it. Zero disables the cap.
CAP=${CAP:-10800}
# Polls whose counts did not move. A read that fails says nothing — the host
# being unreachable is a fact about this machine — so only a heartbeat that
# arrives and stands still says the run stopped working.
STALL=${STALL:-40}
OUT=${OUT:-mutants.out}
EXTRA=${EXTRA:-}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

note() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }
fail() { printf >&2 'mutation-ssh: %s\n' "$*"; exit 1; }

# -n keeps ssh off this script's stdin. A loop that calls ssh without it hands
# its own input to the first one and runs once.
remote() { ssh -n -o ConnectTimeout=20 -o BatchMode=yes "$HOST" "$@"; }

note "reaching $HOST"
remote true 2>/dev/null ||
    fail "cannot reach $HOST over ssh without a password. Check the host is up
  and that a key is loaded: ssh $HOST true"

# --- The toolchain, installed under the account and reused -------------------

cat > "$WORK/bootstrap.sh" <<'BOOT'
set -eu
export CARGO_HOME=$HOME/.cargo RUSTUP_HOME=$HOME/.rustup
export PATH=$CARGO_HOME/bin:$PATH
if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --default-toolchain stable --profile minimal >/dev/null
fi
. "$CARGO_HOME/env"
if ! command -v cargo-binstall >/dev/null 2>&1; then
    curl -L --proto '=https' --tlsv1.2 -sSf \
        https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh |
        bash >/dev/null 2>&1
fi
# A prebuilt binary is quick and is built against a newer glibc than a stable
# distribution carries — 2.39 against this host's 2.34, measured 2026-09-17. So
# each is asked to run before it is believed, and a build from source is what
# answers when it cannot.
#
# `just` is here because the suite reads it: the test that renders README asks
# it for the install paths, and a run without it fails in the unmutated tree.
for tool in cargo-mutants just; do
    if ! "$tool" --version >/dev/null 2>&1; then
        cargo binstall -y "$tool" >/dev/null 2>&1 || true
    fi
    if ! "$tool" --version >/dev/null 2>&1; then
        echo "the prebuilt $tool does not run here; building it" >&2
        cargo install --locked "$tool" >/dev/null 2>&1
    fi
done
rustc --version
cargo mutants --version
BOOT

note "installing the toolchain if it is not there yet"
scp -q "$WORK/bootstrap.sh" "$HOST:$REMOTE_DIR-bootstrap.sh" 2>/dev/null ||
    { remote "mkdir -p ~" && scp -q "$WORK/bootstrap.sh" "$HOST:$REMOTE_DIR-bootstrap.sh"; }
remote "bash $REMOTE_DIR-bootstrap.sh" > "$WORK/bootstrap.log" 2>&1 ||
    fail "the toolchain would not install:
$(tail -12 "$WORK/bootstrap.log" | sed 's/^/  /')"
note "toolchain: $(tr '\n' ' ' < "$WORK/bootstrap.log")"

# --- The working copy, as it stands -----------------------------------------
#
# rsync sends the tree this repository has rather than a revision a remote
# holds, so what is measured is what is in front of you — committed or not.

# rsync will not create the parent of its destination, and the rsync macOS
# ships has no --mkpath to ask it to.
remote "mkdir -p ~/$REMOTE_DIR/repo"
note "sending the working copy"
rsync -az --delete --delete-excluded \
    --exclude '.git/' --exclude '.jj/' --exclude 'target/' \
    --exclude '.cargo-cache/' --exclude '.direnv/' --exclude 'decisions/' \
    --exclude 'kanban/' --exclude 'scratch/' --exclude 'mutants.*/' \
    --exclude '.codegraph/' \
    ./ "$HOST:$REMOTE_DIR/repo/" ||
    fail "could not send the working copy to $HOST:$REMOTE_DIR/repo/"

# --- The run, detached from this connection ---------------------------------

cat > "$WORK/run.sh" <<RUN
set -eu
R=\$HOME/$REMOTE_DIR
export CARGO_HOME=\$HOME/.cargo RUSTUP_HOME=\$HOME/.rustup
export PATH=\$CARGO_HOME/bin:\$PATH
export CARGO_BUILD_JOBS=$BUILD_JOBS
rm -f "\$R/done" "\$R/heartbeat"
cd "\$R/repo"

start=\$(date +%s)
(
    while [ ! -f "\$R/done" ]; do
        counts=
        for name in caught missed timeout unviable; do
            n=0
            [ -f "\$R/repo/mutants.out/\$name.txt" ] &&
                n=\$(wc -l < "\$R/repo/mutants.out/\$name.txt")
            counts="\$counts\"\$name\": \$(echo \$n),"
        done
        printf '%s up=%ss {%s}\n' "\$(date -u +%Y-%m-%dT%H:%M:%SZ)" \\
            "\$(( \$(date +%s) - start ))" "\${counts%,}" > "\$R/heartbeat"
        sleep 15
    done
) &
beat=\$!

# A run that finds survivors exits non-zero and so does one refused in the
# unmutated tree, and either way the marker below is what the watcher waits
# for. With errexit on neither would ever be written, which is a run that
# finished and a watcher that waits for it forever.
set +e
cargo mutants -j $JOBS --timeout-multiplier 5 $EXTRA > "\$R/mutants.log" 2>&1
code=\$?
set -e
kill \$beat 2>/dev/null || true
printf 'rc=%s wall=%ss\n' "\$code" "\$(( \$(date +%s) - start ))" > "\$R/done"
RUN

scp -q "$WORK/run.sh" "$HOST:$REMOTE_DIR/run.sh"
note "starting the run with $JOBS jobs"
# setsid detaches it from this ssh, so the run survives the connection the way
# it has to survive a laptop closing.
remote "cd ~/$REMOTE_DIR && setsid nohup bash run.sh > launch.log 2>&1 < /dev/null & echo started"

# The cell has to be seen alive before this is left to the heartbeat.
for _ in $(seq 1 20); do
    remote "cat ~/$REMOTE_DIR/heartbeat 2>/dev/null" > "$WORK/hb" 2>/dev/null
    [ -s "$WORK/hb" ] && break
    sleep 15
done
[ -s "$WORK/hb" ] || fail "no heartbeat after five minutes, so the run never
  started. The host said:
$(remote "cat ~/$REMOTE_DIR/launch.log 2>/dev/null" | tail -8 | sed 's/^/  /')"
note "running: $(cat "$WORK/hb")"

# --- Watching ---------------------------------------------------------------

mkdir -p "$OUT"
misses=0
still=0
last_counts=
while true; do
    sleep "$POLL"
    if remote "cat ~/$REMOTE_DIR/heartbeat 2>/dev/null" > "$WORK/hb" 2>/dev/null && [ -s "$WORK/hb" ]; then
        misses=0
        beat=$(cat "$WORK/hb")
        note "$beat"

        counts=${beat#*\{}
        if [ "$counts" = "$last_counts" ]; then
            still=$((still + 1))
            [ "$still" -lt "$STALL" ] || fail "nothing has been decided in
  $still polls. The run is wedged rather than slow, and it is left running so it
  can be looked at: ssh $HOST 'tail ~/$REMOTE_DIR/mutants.log'"
        else
            still=0
            last_counts=$counts
        fi

        up=${beat#*up=}
        up=${up%%s *}
        if [ "${CAP:-0}" -gt 0 ] && [ "${up:-0}" -ge "$CAP" ]; then
            fail "the run reports ${up}s, past the ${CAP}s cap. $OUT holds what
  had landed; it is still running on $HOST."
        fi
    else
        misses=$((misses + 1))
        note "could not read the heartbeat ($misses); that is this machine's
  network and not the run"
        continue
    fi
    remote "cat ~/$REMOTE_DIR/done 2>/dev/null" > "$WORK/done" 2>/dev/null
    [ -s "$WORK/done" ] && break
done

note "collecting"
rsync -az "$HOST:$REMOTE_DIR/repo/mutants.out/" "$OUT/" || true
rsync -az "$HOST:$REMOTE_DIR/mutants.log" "$OUT/mutants.log" || true
cp "$WORK/done" "$OUT/done.txt"

note "finished: $(cat "$OUT/done.txt")"
for name in caught missed timeout unviable; do
    printf '  %-9s %s\n' "$name" "$(wc -l < "$OUT/$name.txt" 2>/dev/null | tr -d ' ')"
done

if [ -s "$OUT/missed.txt" ]; then
    printf '\nSurvived, and each needs a test or a written reason:\n'
    sed 's/^/  /' "$OUT/missed.txt"
    exit 1
fi
