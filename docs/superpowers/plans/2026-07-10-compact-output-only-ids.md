# Compact Tool Outputs & view_lines only_ids Option Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clean up and compact the output of `create_lines` and `edit_lines` by removing text content payloads, returning only stable line IDs. Add an optional `only_ids` parameter to `view_lines` to retrieve only Line IDs and line numbers, preventing platform truncation to file.

**Architecture:**
- `create_lines`: Returns `"ids": [String]` list instead of `"lines": [[String, u64, String]]`.
- `edit_lines`: Returns `"modified_ids": [String]` list containing the updated/inserted Line IDs instead of rendering a textual context preview.
- `view_lines`: Exposes optional boolean parameter `only_ids`. When true, returns `columns = ["id", "n"]` and drops the content field in `lines`.

**Tech Stack:** Rust, serde_json

## Global Constraints
- Target workspace path: `/Users/zaeku/workspace/Tools for Agents/ast-editor`
- Decouple all output structures cleanly while maintaining existing core line editing and validation safety.

---

## Task List

### Phase 1: Tool Registry & Schema Updates

#### Task 1: Update Tool Schemas and Descriptions
**Description:** Update `src/tools/mod.rs` to register the new `only_ids` parameter for `view_lines`, and update descriptions in `resources/tool_metadata.json` to reflect the new output formats.

**Acceptance criteria:**
- [ ] `resources/tool_metadata.json` updated with correct schemas and descriptions of compact output behaviors.
- [ ] `src/tools/mod.rs` has the `only_ids` property (type: `boolean`, default: `false`) in the `view_lines` inputSchema.

**Verification:**
- [ ] Run `cargo check` and verify compilation succeeds.

**Dependencies:** None

**Files likely touched:**
- `resources/tool_metadata.json` (modify)
- `src/tools/mod.rs` (modify)

**Estimated scope:** Small (2 files)

---

### Phase 2: Logic Implementation

#### Task 2: Implement Compact Output and only_ids Option in view.rs
**Description:** Refactor `src/tools/view.rs` to support `only_ids` in `view_lines` and return a clean `ids` array in `create_lines`.

**Acceptance criteria:**
- [ ] `view_lines` signature accepts `only_ids: Option<bool>`.
- [ ] If `only_ids` is true, the returned JSON contains `columns: ["id", "n"]` and `lines` contains items structured as `[id, n]`.
- [ ] `create_lines` returns a JSON response containing `status`, `message`, `ids` (as a flat array of String IDs), `total_lines`, and `total_bytes`. It no longer includes the `content` column.
- [ ] Unit tests in `src/tools/view.rs` are refactored to assert the new formats.

**Verification:**
- [ ] Run `cargo test --lib tools::view` and confirm all view unit tests pass.

**Dependencies:** Task 1

**Files likely touched:**
- `src/tools/view.rs` (modify)

**Estimated scope:** Medium (1 file)

---

#### Task 3: Implement Compact Output in edit.rs
**Description:** Refactor `src/tools/edit.rs` to return only a flat list of `modified_ids` in the JSON response of `edit_lines` instead of formatting a textual line preview.

**Acceptance criteria:**
- [ ] `edit_lines` returns a JSON string representing `{ "status": "success", "modified_ids": [String] }`.
- [ ] Unit tests in `src/tools/edit.rs` are updated to match the new return format.

**Verification:**
- [ ] Run `cargo test --lib tools::edit` and confirm all edit unit tests pass.

**Dependencies:** Task 2

**Files likely touched:**
- `src/tools/edit.rs` (modify)

**Estimated scope:** Medium (1 file)

---

### Phase 3: Integration & Documentation

#### Task 4: Refactor Integration Tests and Update SKILL.md
**Description:** Update `tests/line_edit_tests.rs` to align with the new compact JSON structures, add test coverage for `only_ids`, and document the changes in `SKILL.md`.

**Acceptance criteria:**
- [ ] All integration tests in `tests/line_edit_tests.rs` pass successfully.
- [ ] Integrated test `test_integration_view_lines_only_ids` is added.
- [ ] `SKILL.md` documents `only_ids` parameter, the new compact formats, and the position-based matching workflow for agents.

**Verification:**
- [ ] Run `cargo test` and confirm all 57 tests pass with zero warnings.

**Dependencies:** Task 3

**Files likely touched:**
- `tests/line_edit_tests.rs` (modify)
- `SKILL.md` (modify)

**Estimated scope:** Medium (2 files)

---

### Checkpoint: Complete Validation
- [ ] `cargo test` compiles with zero warnings or errors.
- [ ] All 57 tests pass cleanly.
- [ ] Release binary rebuilt successfully.
