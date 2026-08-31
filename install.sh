#!/bin/bash
set -e

echo "=== 1. Building release binary ==="
cargo build --release

echo "=== 2. Creating target deployment directories ==="
TARGET_DIR="$HOME/.gemini/config/plugins/custom-developer-plugin/skills/ast-editor"
mkdir -p "$TARGET_DIR/scripts"

echo "=== 3. Copying binary, SKILL.md, and resources to target plugin directory ==="
cp target/release/ast-editor "$TARGET_DIR/scripts/ast-editor"
cp agent_skill/SKILL.md "$TARGET_DIR/SKILL.md"

# Copy references and resources directories recursively
cp -R agent_skill/references "$TARGET_DIR/"
cp -R resources "$TARGET_DIR/"

chmod +x "$TARGET_DIR/scripts/ast-editor"

echo "=== 4. Updating mcp_config.json ==="
python3 -c "
import json
import os

config_path = os.path.expanduser('~/.gemini/config/mcp_config.json')
if os.path.exists(config_path):
    with open(config_path, 'r') as f:
        try:
            data = json.load(f)
        except Exception:
            data = {}
else:
    data = {}

if 'mcpServers' not in data:
    data['mcpServers'] = {}

data['mcpServers']['ast-editor'] = {
    'command': '$HOME/.gemini/config/plugins/custom-developer-plugin/skills/ast-editor/scripts/ast-editor',
    'args': ['mcp']
}

# Resolve $HOME in python
data['mcpServers']['ast-editor']['command'] = os.path.expandvars(data['mcpServers']['ast-editor']['command'])

with open(config_path, 'w') as f:
    json.dump(data, f, indent=2)
print('mcp_config.json updated successfully.')
"

echo "=== Deploy Finished Successfully! ==="
echo "Ast-editor is now registered as a custom plugin skill at: $TARGET_DIR"
