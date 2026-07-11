# Implementation Plan: Shell Script inspect_ast & edit_lines Support

**Goal:**
Integrate full Tree-sitter AST validation, parsing, and query support for POSIX shell, Bash, Zsh, and Ksh scripts (`.sh`, `.bash`, `.zsh`, `.ksh`) by compiling and linking `tree-sitter-bash.wasm`.

---

## Proposed Changes

### 1. Compile & Integrate `tree-sitter-bash.wasm`
- Create a temporary working folder `scratch/wasm-builder` in the workspace.
- Run `npm install tree-sitter-bash` and use `npx tree-sitter build-wasm node_modules/tree-sitter-bash` to compile the parser to WASM (using the local Docker environment).
- Move the resulting `tree-sitter-bash.wasm` into `resources/wasm/`.
- Clean up `scratch/wasm-builder`.

### 2. Update Configuration
#### [MODIFY] [languages.json](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/resources/wasm/languages.json)
- Add `"bash"` mapping to handle `.sh`, `.bash`, `.zsh`, and `.ksh` extensions pointing to `tree-sitter-bash.wasm`.

### 3. Update Code Logic
#### [MODIFY] [session_db.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/session_db.rs)
- Add `"sh" | "bash" | "zsh" | "ksh"` to `check_language_supported`.
- Add test assertions for shell scripts in `test_check_language_supported`.

#### [MODIFY] [inspect.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/inspect.rs)
- Map `"sh" | "bash" | "zsh" | "ksh"` extensions to `"bash"` in `run_inspect`.

#### [MODIFY] [mod.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/mod.rs)
- Update `inspect_ast` tool schema template description to note `"functions"` (rust, python, bash) and `"classes"` (rust, python).

### 4. Documentation updates
#### [MODIFY] [SKILL.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/SKILL.md)
- Update documentation to show shell scripts are supported.
#### [MODIFY] [README.tpl.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/README.tpl.md)
- Update language lists to include shell scripts.

---

## Verification Plan

### Automated Tests
- Add unit tests verifying:
  - Valid bash edits succeed.
  - Invalid bash edits (e.g. unclosed `if`, syntax errors) fail and roll back safely.
- Run `cargo test` to execute all tests.
- Re-run `cargo test --test readme_generator` to rebuild `README.md`.
- Compile final release binary `cargo build --release`.
