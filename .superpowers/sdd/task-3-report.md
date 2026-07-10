# Task 3 Report: Refactor view.rs and edit.rs to use SessionRepository Trait

## Status
- **Status:** DONE
- **Commits:** `3ba9962` (refactor: view.rs and edit.rs to use SessionRepository trait)

## Summary of Changes
1. **`src/tools/view.rs`**: Refactored `view_lines`, `create_lines`, and `fetch_and_format_lines` to take `repository: &impl SessionRepository` as their first parameter. Removed direct instantiation of `SqliteSessionRepository`.
2. **`src/tools/edit.rs`**: Refactored `edit_lines` and the deprecated `apply_line_edits` to receive `repository: &impl SessionRepository` and use it for database queries and updates.
3. **`src/tools/mod.rs`**: Instantiated `let repository = SqliteSessionRepository;` and passed its reference `&repository` in all dispatcher calls.
4. **Tests**: Updated all test functions in `view.rs`, `edit.rs`, `session_db.rs`, and the integration test file `tests/line_edit_tests.rs` to instantiate and pass the repository reference.

## Verification
- Ran `cargo check --tests` and `cargo test`.
- All 57 tests passed cleanly with 0 failures and 0 compile warnings.
