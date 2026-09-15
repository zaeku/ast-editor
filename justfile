# ast-editor tasks. Run `just` for the list.
#
# The binary is a build product, so it is installed rather than distributed:
# `just install` puts it on PATH for this machine. The skill content under
# agent_skill/ is separate — that is what a skills installer consumes.

# Every recipe runs inside the devshell rather than whatever shell invoked it.
# A caller that reaches cargo through some other PATH gets a different rustc,
# and the artifacts the two leave in target/ do not link against each other.
set shell := ['nix', 'develop', '-c', 'bash', '-c']

prefix := env('AST_EDITOR_PREFIX', env('HOME') / '.agents')
bin_dir := prefix / 'bin'
skill_dir := prefix / 'skills' / 'ast-editor'

[private]
default:
    @just --list --unsorted

# Compile the release binary.
build:
    cargo build --release

# Lint first, because the command someone runs while working is this one and a
# check nobody chooses to run is a check that does not happen. It costs about
# four seconds against a suite that takes eight.

# Lint, then run the whole suite.
test: lint
    cargo test

# Format with rustfmt's defaults.
fmt:
    cargo fmt

# Needs clippy and rustc from one toolchain; the devshell supplies both.

# Lint, and check the formatting.
lint:
    cargo fmt --check
    cargo clippy --all-targets

# The generator runs as a test, so this is a subset of what `test` does.

# Regenerate README.md and the API reference from the templates.
doc:
    cargo test --test readme_generator

# The binary is no use to an agent that has not been told the command exists,
# so this installs both halves.

# Install the binary and the skill.
install: install-bin install-skill

# `cargo release` bumps the version, writes the tag and publishes. Between the
# bump and the publish the installed binary is the tree's again: the fences ask
# the command on PATH, and the decision layer refuses to run them when the
# version it answers is not the version the manifest names. That refusal is why
# the install belongs in the middle rather than at the end.

# Cut a release: LEVEL is patch, minor, major or a version; EXECUTE=1 to carry it out.
release LEVEL='patch' EXECUTE='':
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -z '{{EXECUTE}}' ]; then
        echo 'A release, in order:'
        echo '  1. cargo release {{LEVEL}}     the version, the changelog heading and the tag'
        echo '  2. jj sign && jj git push      master requires a signature'
        echo '  3. just install                so the fences ask this build'
        echo '  4. cargo run --bin check       in decisions/, which refuses a stale binary'
        echo '  5. cargo publish'
        echo
        echo 'Rehearsing step 1 without writing anything:'
        # A dirty tree is what cargo-release objects to, and seeing the order is
        # most wanted while the tree is dirty, so its refusal is printed rather
        # than made this recipe's.
        cargo release {{LEVEL}} --no-confirm || true
        exit 0
    fi
    cargo release {{LEVEL}} --no-confirm --execute --no-push --no-publish --no-tag
    just install
    echo 'Now: sign and push, run the decision layer'"'"'s check, then `cargo publish`.'

# The grammars are compiled into the binary (D-01M28RAGW19ZZC), so the install
# is one file and there is nothing beside it to find.

# Install just the binary.
install-bin: build
    mkdir -p '{{bin_dir}}'
    install -m 755 target/release/ast-editor '{{bin_dir}}/ast-editor'
    @echo 'installed {{bin_dir}}/ast-editor'
    @'{{bin_dir}}/ast-editor' --help > /dev/null && echo 'verified   the installed binary runs'

# Only the hub is installed, and what lands there is what the binary prints:
# D-01M27W3KCKF69G in decisions/, with a fence.

# Install just the skill document.
install-skill: build
    mkdir -p '{{skill_dir}}'
    rm -rf '{{skill_dir}}/references'
    ./target/release/ast-editor skill > '{{skill_dir}}/SKILL.md'
    @echo 'installed {{skill_dir}}/SKILL.md'

# Remove what `install` placed.
uninstall:
    rm -f '{{bin_dir}}/ast-editor'
    rm -rf '{{skill_dir}}'
    @echo 'removed {{bin_dir}}/ast-editor and {{skill_dir}}'

# The suite runs on a rented Colab session rather than here, because a full run
# is 873 mutants. Needs the `colab` CLI; SESSION, HARDWARE and JOBS override it.

# Measure what the tests would catch, and print what survived.
mutants:
    ./tools/mutation-run.sh

# Sharding buys wall clock; the total cost is the same either way. SHARDS,
# HARDWARE and JOBS override it.

# The same measurement, split across rented sessions.
mutants-split:
    ./tools/mutation-shards.sh
