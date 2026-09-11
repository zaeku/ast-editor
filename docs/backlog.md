# Backlog

Work that could plausibly happen. Each entry names the condition that would
make it worth scheduling — an entry without one is an ambition, not a plan.
Nothing here is a commitment; an item earns a spec of its own when its
condition is met.

Designs that were reasoned out of existence are in
[discarded](discarded.md) instead, so that reading this list means reading only
what might still become work.

Nothing is scheduled: no entry below has met its condition. What the tool holds
true about line identity is in `decisions/live/`, each with a fence; this list
is what it has not decided.

## Above the file layer

Every entry here sits on top of the index-backed store and none of them is
reachable from where the store stands today. They came out of the 2026-07-12
design discussion, which git holds.

- **Follow a file across a rename** — match by content and structural hash,
  with git rename detection as a corroborating hint. The store's key is
  path-derived, so a rename starts a fresh entry and the file's ids reset.
  *Needed when:* renames losing ids becomes a real complaint.
- **Semantic AST-path targeting** — target nodes by structural path
  (`class:Foo/fn:solve`) resolved down to line ids. The path grammar is
  unresolved. *Needed when:* agents are observably spending `inspect` calls
  just to turn a symbol they already know into line ids.
- **Entity-scoped identity layer** — a durable entity id above the file layer,
  carried across moves and renames by structural-hash matching, so identity
  survives a cross-file move. *Needed when:* cross-file move continuity
  matters.
- **Entity-qualified handles / path-free global editing** — editing by entity
  handle with no file path. Gated on agent-session ownership management rather
  than on the editing core. *Needed when:* the entity layer above exists and
  something wants to address code without naming a file.
- **Impact / blast-radius analysis** — use the identity layer to answer what a
  change reaches. *Needed when:* asked for; this is a feature idea, not a gap.

## Elsewhere

- **`session_db.rs` is 2,000 lines** — larger than `edit.rs`, and the reconciler,
  the buffer engine, the schema, and the repository all live in it. *Needed
  when:* the next change that touches several of those at once.
