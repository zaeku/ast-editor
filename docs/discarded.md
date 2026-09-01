# Discarded

Designs that were reasoned out of existence rather than postponed. The
distinction from [the backlog](backlog.md) is what a reader should do with an
entry: a backlog item may become work when its condition is met, whereas
nothing here is waiting for a condition. Each entry records what the design
was and what removed the need for it.

Kept because a decision is worth more than a blank space: the same ideas
resurface, and the reasoning is what stops them being re-litigated. Reviving
one means arguing with the reason given, not merely finding it interesting.

Section numbers refer to [the archived design](archive/2026-07-12-persistent-line-ids-and-reconciliation-design.md).

## The premise stopped holding

- **Lexicographic fractional ordering** (§4.2) — replace `sort_order REAL` with
  a LexoRank-style `TEXT` key, because midpoint insertion exhausts the `f64`
  mantissa after roughly 50 insertions between the same pair. Phase 1 removed
  the midpoints: an edit rewrites the whole ordered run as evenly spaced
  values, so nothing subdivides and the mantissa is never approached. What a
  fractional key would still buy is renumbering one row instead of all of them,
  which is throughput, not correctness, and no measurement asks for it. An
  endurance test drives 200 insertions at one point and checks the ordering
  holds.

- **Structural-hash cosmetic gate** (§5.5) — hash the enclosing tree-sitter
  node, skipping comments, and gate reconciliation per region on it, so a
  reformat preserves ids. Phase 2b satisfies that invariant with a
  whitespace-free line hash and no parse. The AST version's residual benefits
  are insensitivity to comment-only edits and skipping the line diff for
  untouched regions; both cost a tree-sitter parse on every reconcile. Note
  that neither version survives a formatter that re-wraps lines, since the
  region's line count changes either way.

## The goal was dropped

Dropping the ability to reconstruct an older state is what let tombstones and
the compaction phase go with it, and it is why the store stays bounded without
a git dependency. These return together, or not at all.

- **Git-checkpoint rewind** (§7) — per-SHA id-map snapshots, so `git reset` /
  `git checkout` restores the ids that were live at that commit. Reviving it
  means reintroducing tombstones and recording enough content to rebuild an old
  state, so price it as reversing §3 and §5.2 of the active spec rather than
  adding to them. A checkout today resets that file's ids and loses no data.

- **Blame backfill from git-held history** (§5.0) — lazily cache id-maps for
  commit SHAs the tool never witnessed, improving blame fidelity. Blame is not
  a thing this tool offers, and git already answers the question.

- **Merge handling** (§16.4) — accept re-anchoring over identity-merge, with
  line-level blame lossy across a merge. Only meaningful once the tool holds
  versions, which it does not.

## Belongs in a different project

- **SSOT-as-SQLite: content-addressed versioning with git projection** (§16) —
  a git-shaped object graph over a content-addressed block store, snapshot and
  changeset hybrid, unified local and remote backends, and projection back out
  to git, motivated by running the editor on Cloudflare's infrastructure. It
  would present the same tool interface, which is exactly why it is a separate
  product rather than a phase here. It also carries an unresolved blocker: the
  parser runs tree-sitter through `wasmtime`, which cannot run inside a Worker,
  so an in-Worker deployment needs a JS/WASM parser path and probably a second
  codebase (§17.3 "C").

## Not worth doing on its own

- **Rename `sessions`/`session_id` to `files`/`file_key`** — the names still
  describe an ephemeral session that the entry has outlived. Every query would
  be touched and no behaviour would change, and `session_id` already is the
  durable key. Worth folding into some later change that rewrites those queries
  anyway; not worth a commit of its own.
