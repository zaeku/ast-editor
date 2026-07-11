# Implementation Plan: README Usage Examples & Multi-Operation Batch Demo

**Goal:**
1. Update `README.tpl.md` to rename 'Live Outputs' sections to 'Usage Examples' (사용 예제) and include paired input argument examples alongside the output JSON blocks.
2. Update `tests/readme_generator.rs` to generate and capture the JSON inputs for all tool examples.
3. Modify the `edit_lines` test block inside `tests/readme_generator.rs` to perform a multi-operation batch (combining `update`, `insert_after`, and `delete`) in a single call, capturing both the multi-op input arguments and output response.

---

## Proposed Changes

### 1. Template Updates
#### [MODIFY] [README.tpl.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/README.tpl.md)
- Replace all "Live Outputs" headings with "Usage Examples".
- Add placeholders for input JSON structures for each tool call:
  - `{{create_lines_input_default}}` & `{{create_lines_output_default}}`
  - `{{create_lines_input_ids}}` & `{{create_lines_output_ids}}`
  - `{{view_lines_input_default}}` & `{{view_lines_output_default}}`
  - `{{view_lines_input_only_ids}}` & `{{view_lines_output_only_ids}}`
  - `{{edit_lines_input_compact}}` & `{{edit_lines_output_compact}}`

### 2. Generator Test Updates
#### [MODIFY] [readme_generator.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/tests/readme_generator.rs)
- Update `generate_readme` to record the input JSON structures for each scenario:
  - `create_lines` input with `return_ids: false`
  - `create_lines` input with `return_ids: true`
  - `view_lines` input with `only_ids: false/None`
  - `view_lines` input with `only_ids: true`
- Refactor the `edit_lines` section to:
  - Prepare a multi-op batch vector (`LineEdit` instances):
    - Update `line 2` content.
    - Insert a new line after `line 2`.
    - Delete `line 3`.
  - Capture the generated JSON input arguments structure.
  - Execute `edit_lines` and capture the resulting response JSON.
- Serialize all input/output JSON pairs and substitute them into the template placeholders.

---

## Verification Plan

### Automated Tests
- Run `cargo test --test readme_generator` to verify that `README.md` is successfully compiled with all matching input/output examples.
- Inspect the generated `README.md` to confirm the formatting is visually appealing and highly readable.
