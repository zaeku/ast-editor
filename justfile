# ast-editor tasks. Run `just` for the list.
#
# The binary is a build product, so it is installed rather than distributed:
# `just install` puts it on PATH for this machine. The skill content under
# agent_skill/ is separate — that is what a skills installer consumes.

prefix := env('AST_EDITOR_PREFIX', env('HOME') / '.agents')
bin_dir := prefix / 'bin'
share_dir := prefix / 'share' / 'ast-editor'
skill_dir := prefix / 'skills' / 'ast-editor'

[private]
default:
    @just --list --unsorted

# Compile the release binary.
build:
    cargo build --release

# Run the whole suite.
test:
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

# Install the binary, its grammars, and the skill.
install: install-bin install-skill

# The binary lands in <prefix>/bin and the grammars in
# <prefix>/share/ast-editor/wasm, which is where the binary looks for them
# relative to itself — so nothing needs to be exported to use it. Set
# AST_EDITOR_WASM_DIR to override that for an unusual layout.

# Install just the binary and its grammars.
install-bin: build
    mkdir -p '{{bin_dir}}' '{{share_dir}}/wasm'
    install -m 755 target/release/ast-editor '{{bin_dir}}/ast-editor'
    cp resources/wasm/*.wasm resources/wasm/languages.json '{{share_dir}}/wasm/'
    @echo 'installed {{bin_dir}}/ast-editor'
    @echo 'grammars  {{share_dir}}/wasm'
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
    rm -rf '{{share_dir}}' '{{skill_dir}}'
    @echo 'removed {{bin_dir}}/ast-editor, {{share_dir}} and {{skill_dir}}'
