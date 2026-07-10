# Compact JSON Line Formatting & SKILL.md Corrections Plan

**Goal:**
1. Customize `view_lines` JSON serialization in `src/tools/view.rs` to format each line tuple array on a single line (no internal vertical spacing).
2. If `only_ids` is true, format the entire `lines` array block on a single line (e.g. `[["id1", 1], ["id2", 2]]`).
3. Correct documentation inaccuracies in `SKILL.md` lines 43 to 72:
   - Change `return_ids` default in `create_lines` to `false`.
   - Update `create_lines` output description (omit list of IDs when `return_ids` is false).
   - Update `edit_lines` output description to reflect the new compact `modified_ids` response instead of the old textual context preview.

---

## Proposed Changes

### 1. view.rs
Modify the final serialization block in `view_lines` in `src/tools/view.rs`:
- Manually serialize each line item to a single-line JSON string.
- If `only_ids` is true, join all item strings with `, ` and wrap them in a single-line `[{}]` representation.
- If `only_ids` is false, join all item strings with `,\n    ` and wrap them in a pretty-printed `[\n    {}\n  ]` representation.
- Format the response JSON string manually by joining all fields pretty-printed.

### 2. SKILL.md
- Correct default values and descriptions in the `create_lines`, `view_lines`, and `edit_lines` reference tables.

---

## Verification Plan

### Automated Tests
- Run `cargo test` to verify that all 61 tests still pass cleanly and that `README.md` is dynamically generated using the new formatting.
