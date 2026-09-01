# Backlog

Ideas that were designed but are not scheduled. Each entry names where the
detail lives. Nothing here is a commitment; an entry earns a spec in
`docs/specs/` only when a real need for it shows up.

Active specs: [dry-run preview](specs/dry-run-preview-spec.md), then
[persistent line IDs](specs/persistent-line-ids-spec.md).

## From the persistent line IDs design

Detail in [the archived design](archive/2026-07-12-persistent-line-ids-and-reconciliation-design.md),
section numbers below refer to it. All of these sit on top of the index-backed
store and are blocked on it.

- **Git-checkpoint rewind** (§7) — per-SHA id-map snapshots, so `git reset` /
  `git checkout` restores the exact IDs that were live at that commit. This is
  the goal the active spec dropped, and dropping it is what let tombstones and
  the compaction phase go with it. Picking it back up means reintroducing both,
  plus enough recorded content to reconstruct an old state — so price it as
  reversing §3 and §5.2 of the active spec, not as an addition to them.
  *Needed when:* an agent is observed losing work to a checkout.
- **Structural-hash cosmetic gate** (§5.5) — hash the enclosing tree-sitter
  node, skipping comments, and use it as a per-region gate during
  reconciliation. Phase 2b shipped a whitespace-insensitive line hash instead,
  which satisfies the same invariant; the AST version would add insensitivity
  to comment-only edits and skip the line diff for untouched regions.
  *Needed when:* comment churn is observed costing ids, or reconciliation shows
  up as slow on large files. Costs a parse on every reconcile.
- **Lexicographic fractional ordering** (§4.2) — replace `sort_order REAL` with
  a LexoRank-style `TEXT` key so an insertion renumbers one row instead of the
  whole file. Purely throughput: the precision exhaustion the original design
  worried about cannot occur now that an edit rewrites the ordered run as
  evenly spaced values. *Needed when:* rewriting every row per edit shows up on
  a large file.
- **Rename `sessions`/`session_id` to `files`/`file_key`** — the names still
  describe an ephemeral session that the entry outlived. No behaviour change,
  every query touched, so fold it into the next change that rewrites them.
- **Follow a file across a rename** (§15 "D") — match by content and structural
  hash, with git rename detection as a corroborating hint. Today the key is
  path-derived, so a rename starts a fresh entry and loses the file's ids.
  *Needed when:* renames losing ids becomes a real complaint.
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
- **Impact / blast-radius analysis** (§13) — using the identity layer to answer
  what a change reaches.
- **SSOT-as-SQLite: content-addressed versioning with git projection** (§16) —
  a git-shaped object graph over a content-addressed block store, snapshot +
  changeset hybrid, unified local/remote backends, and projection back out to
  git, motivated by running the editor on Cloudflare's infrastructure. Not an
  item for this repository: it is a separate product that would present the
  same tool interface, so it belongs in its own project rather than as a phase
  here. It also carries an unresolved blocker: the parser runs tree-sitter
  through `wasmtime`, which cannot run in a Worker, so an in-Worker deployment
  needs a JS/WASM parser path and probably a second codebase (§17.3 "C").
- **Merge handling** (§16.4) — the design accepts re-anchor over identity-merge,
  with line-level blame lossy across a merge. Only relevant once versioning
  exists.

## Elsewhere

- **CLI mode** — `argv[1]` dispatches straight to
  [`ToolDispatcher::call_tool`](../src/tools/mod.rs), with the JSON-RPC loop
  kept behind an `mcp` subcommand. No spec needed.
- **`session_db.rs` is 2,000 lines** — larger than `edit.rs`. Split it during
  phase 1 of the active spec, which rewrites much of it anyway.
