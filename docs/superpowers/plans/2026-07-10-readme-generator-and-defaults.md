# README Template Generator & Default Return IDs Update Plan

**Goal:** 
1. Modify the default value of `return_ids` in `create_lines` to be `false` (defaulting to the most token-efficient, lightweight option). Update all tests to pass `Some(true)` when they assert on returned IDs.
2. Implement a dynamic template-based README rendering pipeline where `README.tpl.md` contains placeholders for tool schemas and live-executed output JSONs. Integrate this generator as a test in `tests/readme_generator.rs` so that running `cargo test` automatically rebuilds `README.md`.
3. Document the design philosophy: Why we do not use the name `write_lines` (it implies destructive overwrite, which is intentionally blocked by `FILE_ALREADY_EXISTS` to prevent catastrophic truncation data loss).

---

## Task List

### Phase 1: Default Parameter Change & Test Refactoring

- [ ] **Task 1: Change `return_ids` default to `false`**
  - [ ] Modify `src/tools/mod.rs` to change `return_ids` default to `false` in the JSON schema.
  - [ ] Modify `src/tools/view.rs` to use `return_ids.unwrap_or(false)`.
  - [ ] Refactor unit and integration tests that assert on the `ids` output field of `create_lines` to explicitly pass `Some(true)`.
  - [ ] Run `cargo test` to ensure all existing tests compile and pass.

### Phase 2: Create Template and Generator

- [ ] **Task 2: Create `README.tpl.md`**
  - [ ] Copy the current `README.md` to `README.tpl.md`.
  - [ ] Add the "Catastrophic Truncation Prevention / Why not write_lines" section.
  - [ ] Insert placeholders for dynamic schemas and execution outputs:
    - `{{view_lines_schema}}`
    - `{{create_lines_schema}}`
    - `{{edit_lines_schema}}`
    - `{{create_lines_output_default}}` (return_ids = false)
    - `{{create_lines_output_ids}}` (return_ids = true)
    - `{{view_lines_output_default}}`
    - `{{view_lines_output_only_ids}}`
    - `{{edit_lines_output_compact}}`

- [ ] **Task 3: Implement `tests/readme_generator.rs`**
  - [ ] Add a new integration test file `tests/readme_generator.rs`.
  - [ ] Implement code to read `README.tpl.md`, run view/create/edit tools on temporary files to extract real output JSON, parse MCP schemas from `src/tools/mod.rs`, replace placeholders, and overwrite `README.md`.
  - [ ] Ensure the test cleans up all generated temporary files.

### Phase 3: Final Verification

- [ ] **Task 4: Run build, tests, and commit**
  - [ ] Run `cargo test` to trigger the README generator and run all 61 tests.
  - [ ] Verify that `README.md` is successfully generated and matches the template structure.
  - [ ] Commit all changes including `README.md` and `README.tpl.md`.
