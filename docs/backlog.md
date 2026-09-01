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
- **Integration tests share the user's real cache** — `get_db_path` picks a
  temporary database under `cfg!(test)`, but that flag is off for the `tests/`
  crate, which links the library built without it. So everything under
  `tests/` reads and writes `~/.cache/line-editor/sessions.db`, the same file
  a real session uses: a test run evicts the user's sessions, and the user's
  sessions can leave rows a test then trips over. Harmless today because the
  store is disposable, but worth closing before phase 2 makes reconciliation
  depend on what the index holds. The fix is a runtime override — an env var
  the test harness sets — rather than a compile-time flag that does not reach
  integration tests.
- **`session_db.rs` is 2,000 lines** — larger than `edit.rs`. Split it during
  phase 1 of the active spec, which rewrites much of it anyway.
