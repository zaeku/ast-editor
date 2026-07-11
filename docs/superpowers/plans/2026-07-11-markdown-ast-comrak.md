# Implementation Plan: Markdown AST Integration using Comrak

**Goal:**
1. Add the `comrak` library as a dependency.
2. Enable Markdown (.md, .markdown) syntax validation in `apply_line_edits`.
3. Support Markdown AST dumping in `dump_ast`.
4. Update `SKILL.md` and `README.md` to document Markdown support and how Comrak handles structural safety checks.

---

## Proposed Changes

### 1. Dependency Integration
#### [MODIFY] [Cargo.toml](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/Cargo.toml)
- Add `comrak = "0.28.0"` to the `[dependencies]` section.
- Run `cargo check` to verify successful dependency resolution.

### 2. Markdown Syntax Validation in Edit
#### [MODIFY] [edit.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/edit.rs)
- Update `check_language_supported` to return `true` for `"md"` and `"markdown"`.
- Implement `validate_markdown(content: &str) -> Result<()>`:
  - Parse the markdown content using `comrak::parse_document` with standard + GFM extensions enabled.
  - Traverse the resulting AST nodes recursively to detect structural syntax errors (such as unclosed code fences, malformed blockquotes, or critical parsing failures).
  - Return an error with descriptive diagnostic messages if any violations are found.
- Update `validate_syntax` to route `.md` and `.markdown` extensions to `validate_markdown` instead of calling `ParserManager::parse_code`.

### 3. Markdown AST Dumping in Dump
#### [MODIFY] [dump.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/dump.rs)
- Update `run_dump` to detect `.md` and `.markdown` files.
- For Markdown, bypass Tree-sitter and parse the document using `comrak::parse_document`.
- Implement a recursive helper `format_comrak_node` to traverse the Comrak AST and format the tree structure into a uniform indent-spaced outline matching tree-sitter dump format (including source positions retrieved via `node.data.borrow().sourcepos`).
- Integrate the Markdown S-expression/outline into the final dump response.

### 4. Schema and Extension Registry
#### [MODIFY] [inspect.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/inspect.rs)
- Update `run_inspect` language mapping block to map `.md` / `.markdown` to `"markdown"`.
- If an query is requested, return a descriptive error stating that tree-sitter query matching is not supported for markdown, but AST dumping via `dump_ast` is fully supported.

### 5. Documentation updates
#### [MODIFY] [SKILL.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/SKILL.md)
- Add `.md` and `.markdown` to the supported languages/extensions lists.
- Describe how Markdown parsing runs statically using `comrak` and how it performs structural safety checks on document fences and codeblocks.
#### [MODIFY] [README.tpl.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/README.tpl.md)
- Update the list of supported extensions to include Markdown `.md` and `.markdown`.

---

## Verification Plan

### Automated Tests
- Create unit tests in `src/tools/edit.rs` covering markdown syntax validation (e.g. valid markdown edit passes, malformed/unclosed fence blocks fail and trigger rollbacks).
- Create a test in `src/tools/dump.rs` to verify that `dump_ast` successfully outputs a structured tree for markdown files.
- Run `cargo test` to execute all tests.
- Re-run `cargo test --test readme_generator` to rebuild `README.md`.
