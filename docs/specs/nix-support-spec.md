# Nix AST Support Specification (ast-editor)

This specification defines the integration of the Nix programming language parser (`tree-sitter-nix`) into `ast-editor` to enable AST analysis on `.nix` files.

## Proposed Changes

### 1. Parser Mapping Configuration
Modify [languages.json](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/resources/wasm/languages.json) to add:
```json
  "nix": {
    "extensions": [".nix"],
    "wasm_file": "tree-sitter-nix.wasm"
  }
```

### 2. WASM Module Acquisition Strategy
To compile Nix files, a `tree-sitter-nix.wasm` module is required in [resources/wasm/](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/resources/wasm/).
We will attempt to acquire it in two phases:
- **Phase 1 (Pre-compiled)**: Download `tree-sitter-nix.wasm` from verified sources (e.g. npm package or GitHub release).
- **Phase 2 (Fallback Compilation)**: If loading fails or compatibility issues arise, compile it locally:
  - Clone `tree-sitter-nix` source.
  - Run `tree-sitter build-wasm` to output `tree-sitter-nix.wasm`.

## Verification Plan
1. Copy/Download `tree-sitter-nix.wasm` into `resources/wasm/`.
2. Verify that calling the `inspect_ast` tool on a `.nix` file returns the correct AST tree structures without failure.
