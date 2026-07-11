# Walkthrough: README Usage Examples & Multi-Operation Batch Demo

We have successfully refined the codebase documentation to present simple input arguments alongside their live outputs. We also redesigned the README generator's `edit_lines` test to demonstrate a real-world multi-operation batch transactional change.

## Changes Accomplished

### 1. Unified 'Usage Examples' (사용 예제) in README
- Reorganized `README.tpl.md` to rename 'Live Outputs' sections to 'Usage Examples'.
- For each tool call example (`create_lines` default & IDs, `view_lines` default & only_ids, `edit_lines` compact), we now present:
  - **Input arguments example JSON** (the simple parameter payload to send).
  - **Output response example JSON** (the structure returned by the tool).

### 2. Multi-Operation Batch Demo for `edit_lines`
- Refactored `tests/readme_generator.rs` to prepare and execute an atomic batch of edits combining:
  - `insert_after` to inject a new line (`let y = 200;`) after line 2.
  - `update` to modify line 2 content to (`let x = 100;`).
  - `delete` to erase line 3 (`}`).
- Captured and serialized both the exact JSON arguments input and the resulting output response into `README.md`.
- Verified that execution order in the mock JSON template exactly matches code execution order to guarantee error-free verification.

## Verification Results
- All 66 tests passed cleanly under `cargo test`.
- Visual layout of `README.md` now clearly shows both input and output structures for easy developer onboarding.
