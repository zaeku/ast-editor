# Discarded

Designs that were reasoned out of existence rather than postponed. The
distinction from [the backlog](backlog.md) is what a reader should do with an
entry: a backlog item may become work when its condition is met, whereas
nothing here is waiting for a condition. Each entry records what the design
was and what removed the need for it.

Kept because a decision is worth more than a blank space: the same ideas
resurface, and the reasoning is what stops them being re-litigated. Reviving
one means arguing with the reason given, not merely finding it interesting.

Every entry below came out of the 2026-07-12 design discussion, which git
holds.

## The premise stopped holding

- **Lexicographic fractional ordering** — replace `sort_order REAL` with
  a LexoRank-style `TEXT` key, because midpoint insertion exhausts the `f64`
  mantissa after roughly 50 insertions between the same pair. Why the premise
  no longer holds is `D-01M280K4V8RX4F`, which has a fence driving insertions
  at one point.

- **Structural-hash cosmetic gate** — hash the enclosing tree-sitter
  node, skipping comments, and gate reconciliation per region on it, so a
  reformat preserves ids. Phase 2b satisfies that invariant with a
  whitespace-free line hash and no parse. The AST version's residual benefits
  are insensitivity to comment-only edits and skipping the line diff for
  untouched regions; both cost a tree-sitter parse on every reconcile. Note
  that neither version survives a formatter that re-wraps lines, since the
  region's line count changes either way.

## The goal was dropped

Held as `D-01M27W003J69KH` in the decision layer since 2026-09-11, with a
fence: the store holds only the present, and why the three designs below come
back together or not at all is stated there rather than here.

- **Git-checkpoint rewind** — per-SHA id-map snapshots, so `git reset` /
  `git checkout` restores the ids that were live at that commit.
- **Blame backfill from git-held history** — lazily cache id-maps for
  commit SHAs the tool never witnessed, improving blame fidelity.
- **Merge handling** — accept re-anchoring over identity-merge, with
  line-level blame lossy across a merge.

## Belongs in a different project

- **SSOT-as-SQLite: content-addressed versioning with git projection** —
  a git-shaped object graph over a content-addressed block store, snapshot and
  changeset hybrid, unified local and remote backends, and projection back out
  to git, motivated by running the editor on Cloudflare's infrastructure. It
  would present the same tool interface, which is exactly why it is a separate
  product rather than a phase here. It also carries an unresolved blocker: the
  parser runs tree-sitter through `wasmtime`, which cannot run inside a Worker,
  so an in-Worker deployment needs a JS/WASM parser path and probably a second
  codebase.

## Not worth doing on its own

- **Rename `sessions`/`session_id` to `files`/`file_key`** — the names still
  describe an ephemeral session that the entry has outlived. Every query would
  be touched and no behaviour would change, and `session_id` already is the
  durable key. Worth folding into some later change that rewrites those queries
  anyway; not worth a commit of its own.
