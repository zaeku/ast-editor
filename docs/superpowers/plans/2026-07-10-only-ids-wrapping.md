# Chunked multi-line wrapping for only_ids in view_lines

**Goal:**
1. Address the 2,048-character line length limit hazard. If a JSON output file containing a single long line is viewed via `view_lines`, it would trigger truncation.
2. Structure the `only_ids` response layout to align with the user's preferred format:
   ```json
   "lines": [
     ["1#77cf", 1], ["2#bcb4", 2], ["3#c2b7", 3]
   ]
   ```
   by chunking the array into groups of 10 items per line.

---

## Proposed Changes

### 1. view.rs
Modify `src/tools/view.rs` where `lines_formatted` is constructed for `only_ids_bool = true`:
- Use `lines_strs.chunks(10)` to group the generated tuple strings.
- Join each chunk with `, `.
- Join the resulting lines with `,\n    ` and wrap them inside `[\n    {}\n  ]`.

---

## Verification Plan

### Automated Tests
- Run `cargo test` to execute all unit and integration tests (including the readme generator).
- Verify that `README.md` is updated to show the wrapped `only_ids` format.
