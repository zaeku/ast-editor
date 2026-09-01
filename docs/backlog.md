# Backlog

Work that could plausibly happen. Each entry names the condition that would
make it worth scheduling — an entry without one is an ambition, not a plan.
Nothing here is a commitment; an item earns a spec in `docs/specs/` when its
condition is met.

Designs that were reasoned out of existence are in
[discarded](discarded.md) instead, so that reading this list means reading only
what might still become work.

Active specs: [dry-run preview](specs/dry-run-preview-spec.md), then
[persistent line IDs](specs/persistent-line-ids-spec.md).

## From the persistent line IDs design

Detail in [the archived design](archive/2026-07-12-persistent-line-ids-and-reconciliation-design.md);
section numbers below refer to it. All of these sit on top of the index-backed
store.

- **Follow a file across a rename** (§15 "D") — match by content and structural
  hash, with git rename detection as a corroborating hint. The key is
  path-derived today, so a rename starts a fresh entry and the file's ids reset.
  *Needed when:* renames losing ids becomes a real complaint.
- **Semantic AST-path targeting** (§8) — target nodes by structural path
  (`class:Foo/fn:solve`) resolved down to line ids. The path grammar is
  unresolved. *Needed when:* agents are observably spending `inspect_ast` calls
  just to turn a symbol they already know into line ids.
- **Entity-scoped identity layer** (§9) — a durable `entity_id` above the file
  layer, carried across moves and renames by structural-hash matching, so
  identity survives a cross-file move. *Needed when:* cross-file move
  continuity matters.
- **Entity-qualified handles / path-free global editing** (§9.5) — editing by
  entity handle with no file path. Gated on agent-session ownership management
  rather than on the editing core. *Needed when:* the entity layer above exists
  and something wants to address code without naming a file.
- **Impact / blast-radius analysis** (§13) — use the identity layer to answer
  what a change reaches. *Needed when:* asked for; this is a feature idea, not
  a gap.

## Elsewhere

- **`session_db.rs` is 2,000 lines** — larger than `edit.rs`, and the reconciler,
  the buffer engine, the schema, and the repository all live in it. *Needed
  when:* the next change that touches several of those at once.
