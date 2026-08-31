# Backlog

Ideas that were designed but are not scheduled. Each entry names where the
detail lives. Nothing here is a commitment; an entry earns a spec in
`docs/specs/` only when a real need for it shows up.

Active specs: [dry-run preview](specs/dry-run-preview-spec.md), then
[persistent line IDs](specs/persistent-line-ids-spec.md).

## From the persistent line IDs design

Detail in [the archived design](archive/2026-07-12-persistent-line-ids-and-reconciliation-design.md),
section numbers below refer to it. All of these sit on top of the persistent
store and are blocked on it.

- **Git-checkpoint compaction** (§7) — hot/cold split, per-SHA id-map snapshots,
  retention GC, and restoring the exact id-map live at a commit on `git reset` /
  `git checkout`. *Needed when:* tombstone growth in a long-lived file measurably
  hurts. Until then tombstones just accumulate.
- **Blame backfill from git-held history** (§5.0) — lazily cache id-maps for
  commit SHAs the tool never witnessed live, to improve blame fidelity.
  *Needed when:* something actually asks the tool for blame.
- **Semantic AST-path targeting** (§8) — target nodes by structural path
  (`class:Foo/fn:solve`) resolved down to line IDs. *Needed when:* agents are
  observably burning calls on `inspect_ast` just to convert a known symbol into
  line IDs. The path grammar is unresolved.
- **Entity-scoped identity layer** (§9) — durable `entity_id` above the file
  layer, carried across moves and renames by structural-hash matching, so
  history survives cross-file moves. The schema hooks are designed to be inert
  until then. *Needed when:* cross-file move continuity matters.
- **Entity-qualified handles / path-free global editing** (§9.5) — editing by
  entity handle with no file path. Gated on agent-session ownership management,
  not on the editing core. Deferred indefinitely.
- **Durable `file_key` with rename tracking** (§15 "D") — follow renames by
  content and structural hash, with git rename detection as a corroborating hint
  only. *Needed when:* renames losing IDs becomes a real complaint.
- **Impact / blast-radius analysis** (§13) — using the identity layer to answer
  what a change reaches.
- **SSOT-as-SQLite: content-addressed versioning with git projection** (§16) —
  a git-shaped object graph over a content-addressed block store, snapshot +
  changeset hybrid, unified local/remote backends, and projection back out to
  git. This is a different product from an AST editor. It also carries an
  unresolved blocker: the parser runs tree-sitter through `wasmtime`, which
  cannot run in a Worker, so an in-Worker deployment would need a JS/WASM
  parser path and possibly a second codebase (§17.3 "C").
- **Merge handling** (§16.4) — the design accepts re-anchor over identity-merge,
  with line-level blame lossy across a merge. Only relevant once versioning
  exists.

## Elsewhere

- **CLI mode** — `argv[1]` dispatches straight to
  [`ToolDispatcher::call_tool`](../src/tools/mod.rs), with the JSON-RPC loop
  kept behind an `mcp` subcommand. No spec needed.
- **`session_db.rs` is 2,000 lines** — larger than `edit.rs`. Split it when the
  persistent store lands, since that rewrites much of it anyway.
