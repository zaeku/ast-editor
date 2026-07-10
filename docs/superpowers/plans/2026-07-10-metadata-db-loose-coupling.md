# Implementation Plan: Metadata Separation & Database Abstraction

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cleanly separate static tip/description text into an external JSON file embedded at compile-time, and abstract the database access layer into a trait to decouple the application logic from rusqlite.

**Architecture:** 
- We will create `resources/tool_metadata.json` and load it via `once_cell::sync::Lazy` and `include_str!`.
- We will define a `SessionRepository` trait in `src/tools/session_db.rs` to hide rusqlite internals, making the codebase ready for future database engines (like Limbo).

**Tech Stack:** Rust, once_cell, serde_json, rusqlite

## Global Constraints
- Target workspace path: `/Users/zaeku/workspace/Tools for Agents/ast-editor`
- Keep line limit capping (800 lines), size capacity capping (45KB), and individual line truncation (2048 chars) intact.
- Decouple all rusqlite usage in `view.rs` and `edit.rs` by routing through `SessionRepository` trait.

---

## Task List

### Phase 1: Metadata Externalization

#### Task 1: Create tool_metadata.json and Metadata Loader Module
**Description:** Separate static text strings (tips, descriptions) into an external JSON file, and load it dynamically using `once_cell::sync::Lazy` and `include_str!` to prevent code pollution and allow easy customization.

**Acceptance criteria:**
- [ ] JSON file `resources/tool_metadata.json` exists with the correct descriptions and tips.
- [ ] `src/tools/metadata.rs` compiles cleanly and exposes helper functions:
  - `pub fn get_tool_description(name: &str) -> String`
  - `pub fn get_tool_tip(name: &str) -> String`
- [ ] Tools registry in `mod.rs` uses descriptions dynamically loaded from the JSON.
- [ ] `dump_ast` footer dynamically appends the tip loaded from the JSON.

**Verification:**
- [ ] Run `cargo check` and verify zero errors.
- [ ] Run `cargo test --lib` and confirm existing tests pass.

**Dependencies:** None

**Files likely touched:**
- `resources/tool_metadata.json` (create)
- `src/tools/metadata.rs` (create)
- `src/tools/mod.rs` (modify)
- `src/tools/dump.rs` (modify)

**Estimated scope:** Small (3 files)

---

### Checkpoint: Metadata Separation
- [ ] `cargo check` compiles with zero warnings or errors.
- [ ] Unit tests pass cleanly.

---

### Phase 2: Database Layer Abstraction

#### Task 2: Define SessionRepository Trait and Implement Sqlite Backend
**Description:** Create a `SessionRepository` trait that defines all database interactions (session initialization, fetching lines, updating lines, querying length) to decouple business logic from rusqlite. Implement this trait for `SqliteSessionRepository` within `src/tools/session_db.rs`.

**Acceptance criteria:**
- [ ] `SessionRepository` trait is defined with clean interfaces.
- [ ] `SqliteSessionRepository` implements the trait.
- [ ] All SQL queries are encapsulated within `src/tools/session_db.rs`, and `get_db_connection` is kept private.

**Verification:**
- [ ] Run `cargo check` and verify zero errors.
- [ ] Run `cargo test --lib` (internal unit tests in `session_db.rs` pass).

**Dependencies:** Task 1

**Files likely touched:**
- `src/tools/session_db.rs` (modify)

**Estimated scope:** Small (1 file)

---

#### Task 3: Refactor view.rs and edit.rs to use SessionRepository Trait
**Description:** Replace direct database connections and queries in `view_lines`, `create_lines`, and `edit_lines` with method calls on `&dyn SessionRepository`. Instantiate `SqliteSessionRepository` in `mod.rs` and pass it to these tools.

**Acceptance criteria:**
- [ ] `view.rs` and `edit.rs` contain zero references to `rusqlite` or direct SQL strings.
- [ ] `mod.rs` instantiates `SqliteSessionRepository` and passes it dynamically.
- [ ] Unit tests in `view.rs` and `edit.rs` are refactored to pass the repository backend.

**Verification:**
- [ ] Run `cargo test` and confirm all 56 tests pass.

**Dependencies:** Task 2

**Files likely touched:**
- `src/tools/view.rs` (modify)
- `src/tools/edit.rs` (modify)
- `src/tools/mod.rs` (modify)

**Estimated scope:** Medium (3 files)

---

### Checkpoint: Decoupling Complete
- [ ] All 56 tests pass.
- [ ] Rebuild release binary and verify live MCP call behavior.

---

## Risks and Mitigations
| Risk | Impact | Mitigation |
|------|--------|------------|
| Trait overhead / Performance degradation | Low | The trait methods will use dynamic/static dispatch which has zero runtime cost relative to SQLite disk/memory operations. |
| DB connection leak / lock issue | Med | Keep SQLite connection lifecycle scoped inside trait method implementations, reusing the existing `get_db_connection` helper. |
