# Implementation Plan: Compact edit_lines Response Formatting

**Goal:**
Format the `modified_ids` array returned by `edit_lines` compactly using a wrap trigger (Format B alignment) to save tokens and prevent vertical bloat.

---

## Proposed Changes

### 1. Formatting Module Extension
#### [MODIFY] [formatter.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/formatter.rs)
- Implement `pub fn format_modified_ids(ids: &[String], wrap_trigger_length: usize) -> String` to:
  - Return `[]` if the slice is empty.
  - Group string elements into single horizontal lines separated by commas, wrapping them only when the line length exceeds the configured `wrap_trigger_length`.
  - Format with proper indentation (2-spaces for brackets, 4-spaces for inner wrapped elements) to align with Format B standards.

### 2. Edit Lines Output Integration
#### [MODIFY] [edit.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/edit.rs)
- Update `edit_lines` response serialization:
  - Retrieve `only_ids_wrap_trigger_length` from config metadata.
  - Call `format_modified_ids(&newly_modified_ids, config.only_ids_wrap_trigger_length)` to serialize the array.
  - Manually construct the final JSON string:
    ```json
    {
      "status": "success",
      "modified_ids": [
        "1#77cf", "2#bcb4", "3#c2b7"
      ]
    }
    ```

---

## Verification Plan

### Automated Tests
- Update unit tests in `src/tools/formatter.rs` verifying the correctness of `format_modified_ids`.
- Run `cargo test` to execute all tests.
- Re-run `cargo test --test readme_generator` to rebuild `README.md`.
