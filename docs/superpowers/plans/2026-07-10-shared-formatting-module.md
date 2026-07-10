# Implementation Plan: Shared Formatting Module & Wrap Trigger Configuration

**Goal:**
1. Create `src/tools/formatter.rs` to centralize all JSON output formatting logic (Format B, 2048-char truncation, 45KB capacity caps) and prevent duplication between `view_lines` and `inspect_ast`.
2. Load configuration values (`only_ids_wrap_trigger_length`, `view_lines_response_tip`) from `resources/tool_config.json` embedded at compile-time.
3. Update `src/tools/view.rs` and `src/tools/inspect.rs` to use the new shared formatter.
4. Enforce compile-time embedding in `AGENTS.md` (completed).

---

## Proposed Changes

### 1. Configuration Module Updates
#### [MODIFY] [metadata.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/metadata.rs)
- Add `ToolConfig` struct mapping `only_ids_wrap_trigger_length: usize` and `view_lines_response_tip: String`.
- Load and parse `resources/tool_config.json` using `once_cell::sync::Lazy`.
- Expose `pub fn get_config() -> &'static ToolConfig`.

### 2. Introduce Shared Formatter
#### [NEW] [formatter.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/formatter.rs)
- Implement `retrieve_and_format_lines` which:
  - Fetches line records from `SessionRepository`.
  - Performs 2,048-character length limit check & `#TRUNC` suffixing.
  - Enforces the 45,000-byte cumulative capacity cap.
  - Builds compact tuples (`[id, n, content]` or `[id, n]`).
  - Implements **Format B**: wraps outer array brackets with newlines but keeps inner line tuples compactly on single lines.
  - Implements **Wrap Trigger**: for `only_ids = true`, adds a newline if the current line exceeds `only_ids_wrap_trigger_length` characters, wrapping to the next line.

### 3. Integrate Shared Formatter
#### [MODIFY] [mod.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/mod.rs)
- Register `formatter` module: `pub mod formatter;`.

#### [MODIFY] [view.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/view.rs)
- Remove `fetch_and_format_lines`.
- Update `view_lines` to call `formatter::retrieve_and_format_lines` and serialize the final JSON response using the dynamically loaded response tip.

#### [MODIFY] [inspect.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/inspect.rs)
- Update `format_definition_table` to call `formatter::retrieve_and_format_lines` with `only_ids = false`, giving `inspect_ast` full capacity capping and truncation safety.

---

## Verification Plan

### Automated Tests
- Run `cargo test` to verify that all 61 tests pass cleanly.
- Verify `tests/readme_generator.rs` automatically compiles and outputs a compliant `README.md` using the new formatting.
