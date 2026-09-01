# ast-editor tasks. Run `just` for the list.
#
# The binary is a build product, so it is installed rather than distributed:
# `just install` puts it on PATH for this machine. The skill content under
# agent_skill/ is separate — that is what a skills installer consumes.

prefix := env('AST_EDITOR_PREFIX', env('HOME') / '.agents')
bin_dir := prefix / 'bin'
share_dir := prefix / 'share' / 'ast-editor'

[private]
default:
    @just --list --unsorted

# Compile the release binary.
build:
    cargo build --release

# Run the whole suite.
test:
    cargo test

# Needs clippy and rustc from one toolchain; the devshell supplies both.

# Lint.
lint:
    cargo clippy --all-targets

# The generator runs as a test, so this is a subset of what `test` does.

# Regenerate README.md and the API reference from the templates.
doc:
    cargo test --test readme_generator

# The binary lands in <prefix>/bin and the grammars in
# <prefix>/share/ast-editor/wasm, which is where the binary looks for them
# relative to itself — so nothing needs to be exported to use it. Set
# AST_EDITOR_WASM_DIR to override that for an unusual layout.

# Install the binary and its grammars for this machine.
install: build
    mkdir -p '{{bin_dir}}' '{{share_dir}}/wasm'
    install -m 755 target/release/ast-editor '{{bin_dir}}/ast-editor'
    cp resources/wasm/*.wasm resources/wasm/languages.json '{{share_dir}}/wasm/'
    @echo 'installed {{bin_dir}}/ast-editor'
    @echo 'grammars  {{share_dir}}/wasm'
    @'{{bin_dir}}/ast-editor' --help > /dev/null && echo 'verified   the installed binary runs'

# Remove what `install` placed.
uninstall:
    rm -f '{{bin_dir}}/ast-editor'
    rm -rf '{{share_dir}}'
    @echo 'removed {{bin_dir}}/ast-editor and {{share_dir}}'

# Separate from `install` on purpose: a client that mounts this over MCP keeps
# every tool schema in its context for the whole conversation, which is the cost
# the command form exists to avoid. Ask for it only if a client needs it.

# Register the installed binary as an MCP server in a client config.
install-mcp config='':
    #!/usr/bin/env bash
    set -euo pipefail
    config='{{config}}'
    if [[ -z "$config" ]]; then
        echo 'Pass the client config to edit, e.g.' >&2
        echo '  just install-mcp ~/.claude.json' >&2
        exit 2
    fi
    python3 - "$config" "{{bin_dir}}/ast-editor" <<'PY'
    import json, sys, pathlib
    config, command = pathlib.Path(sys.argv[1]).expanduser(), sys.argv[2]
    data = json.loads(config.read_text()) if config.exists() else {}
    data.setdefault('mcpServers', {})['ast-editor'] = {'command': command, 'args': ['mcp']}
    config.write_text(json.dumps(data, indent=2) + '\n')
    print(f'registered ast-editor in {config}')
    PY
