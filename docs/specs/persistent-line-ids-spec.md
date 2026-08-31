# Spec: Persistent Line IDs and Diff Reconciliation (`ast-editor`)

Moves line identity from an ephemeral per-file session to a durable store that
is reconciled against disk by a real line diff. Guiding principle: *disk is the
ground truth; the ID store is a durable index over it, reconciled — never
blindly rebuilt.*

Split out of [the original design](../archive/2026-07-12-persistent-line-ids-and-reconciliation-design.md), which also
carried git-checkpoint compaction, semantic-path targeting, an entity identity
layer, and a content-addressed storage redesign. Those live in
[the backlog](../backlog.md) and are not part of this spec. Do not implement
this one before [dry-run preview](dry-run-preview-spec.md).

## 1. Objectives

- **Durable identity**: a line's ID stays stable across sessions, process
  restarts, and external edits, for the lifetime of the file on this system.
- **External-edit resilience**: when a file changes outside the tool — another
  agent, a human, a formatter, a `git checkout` — reconcile via diff instead of
  discarding every ID.
- **Incremental tracking**: only changed lines get new IDs; unchanged regions
  keep theirs by construction.

### Non-Goals

- Cross-machine ID synchronization.
- Replacing git as the source of truth for content or history.
- Real-time collaborative editing. The target is sequential reconciliation, not
  live merge.
- Bounded store growth. Tombstones accumulate under this spec; compaction is a
  backlog item.

## 2. Background: why the session model breaks

The current model ([session_db.rs](../../src/tools/session_db.rs)) keys a
session by `filepath` and stores each line as `(session_id, sequence_id,
line_hash, content, sort_order REAL, parent_context)`, surfacing IDs as
`{sequence_id:x}#{hash}` (e.g. `1#77cf`). Two structural weaknesses motivate the
change:

1. **Ephemerality**: on an `mtime`/`file_hash` mismatch the safe fallback is to
   rebuild the session, regenerating every `sequence_id`. The agent loses the
   IDs it was tracking exactly when a concurrent change occurred. `smart_resync`
   narrows this to the case where targeted lines survive intact, but the
   fallback is still a full rebuild.
2. **`sort_order REAL` precision exhaustion**: fractional midpoint insertion
   between two adjacent lines exhausts the `f64` mantissa after roughly 50
   consecutive insertions between the same pair. Under a durable store where
   insertions accumulate over a file's lifetime this is a real corruption risk.

## 3. Persistent Identity Model

### 3.1 Line ID composition

A durable line ID is `{line_id:x}#{content_hash}` where:

- **`line_id`** is a per-file **monotonic, never-reused** integer allocated from
  a per-file counter (`next_line_id`). Deleting a line does not free its
  `line_id`. This is the identity.
- **`content_hash`** is a short hash of the trimmed content, as today. It is a
  cheap change-detector and a human-readable disambiguator, not identity.

The surfaced format is unchanged, so no agent-facing contract changes.

### 3.2 Ordering: a fractional-index string replaces `sort_order REAL`

Adopt a lexicographic fractional key (LexoRank-style, base-62) stored as `TEXT`:

- Ordering is the natural lexicographic sort of the key.
- Insertion between two keys `A < C` produces `B` with `A < B < C` by string
  midpoint, which never exhausts — the string grows a character when needed.
- A periodic rebalance renormalizes keys to keep them short, and touches only
  ordering keys.

Identity and order are fully decoupled: reordering never changes a `line_id`,
and re-IDing never happens on reorder.

## 4. JIT Diff Reconciliation

This replaces "rebuild on mismatch" and is the heart of external-edit
resilience.

### 4.1 Scope of the guarantee

The contract is narrow and sufficient: **open → reconcile-now → stable IDs
onward**. When an agent opens a file the current on-disk state is reconciled
into the store at that instant, and from then on IDs stay stable across its
edits. Reconstructing history the tool never witnessed — a human editing and
reverting a line without committing — is out of scope, and an endpoint diff
between the stored state and current content is enough for the guarantee above.
Using git's committed history to enrich blame and attribution is a backlog item,
not a dependency.

### 4.2 Trigger

On any tool entry that touches a file (`view_lines`, `edit_lines`,
`inspect_ast`), compute the current disk `working_hash` and `mtime`. If they
match the stored values, skip reconciliation. If they differ, run the reconciler
**before** serving or mutating. This is a single gate, shared by open-time and
edit-time paths.

### 4.3 Algorithm

Use **patience diff** (or histogram diff), not naive Myers. Code is dense with
duplicate lines (`}`, blank lines, `return None`) and naive LCS mis-aligns them.
Patience diff anchors on lines unique in both versions, then recurses into the
gaps, which yields stable alignments on duplicate-heavy text.

Reconciling the stored index against disk:

- **Unchanged** line → keep its `line_id` and fractional key; refresh cached
  `mtime`.
- **Inserted** line → allocate a fresh `line_id`, assign a fractional key
  between its neighbors.
- **Deleted** line → mark tombstone. The `line_id` is retired and the row is
  retained.
- **Moved** line → same `line_id`, new fractional key.

### 4.4 Disambiguation with `parent_context`

When two candidate matches are otherwise equal, prefer the alignment whose
surrounding structural context matches. This reduces mis-mapping in repetitive
blocks.

### 4.5 Edit-time conflict policy

When reconciliation at edit time finds that a **targeted** line was itself
changed or deleted on disk, fail with a `CONCURRENCY_CONFLICT` error naming the
offending IDs rather than guessing. Non-targeted changes reconcile silently.
This supersedes `smart_resync` and gives it a concrete diff algorithm.

### 4.6 Structural-hash cosmetic-change gate

`content_hash` alone is fragile against formatters: after `prettier`, `black`,
or `cargo fmt` every line's hash changes though no logic did, forcing a large
low-confidence re-alignment. Compute a **structural hash** of the enclosing AST
node — a hash over the tree-sitter node that skips comment nodes and normalizes
whitespace, covering node kinds plus trimmed leaf text. Formatting-only edits
leave it identical.

- If a region's on-disk structural hash equals the stored one, treat its lines
  as unchanged-cosmetic: preserve their `line_id`s, refresh content and
  `content_hash`, and skip the line diff for that region entirely. A full-file
  reformat becomes an identity-preserving event instead of a mass re-ID.
- Only regions whose structural hash differs fall through to §4.3.
- Responses may carry a `change_kind` of `cosmetic` or `logic` per touched
  region, which is also useful for dry-run and edit reporting.

The gate is an optimization and a confidence signal; correctness still rests on
the line diff for regions that actually changed.

## 5. Data Model & Migration

### 5.1 Schema

```sql
-- One row per tracked file (durable, replaces ephemeral "sessions")
files (
    file_key       TEXT PRIMARY KEY,   -- stable key for the file on this system
    rel_path       TEXT,
    working_hash   TEXT NOT NULL,      -- hash of current disk content
    working_mtime  INTEGER NOT NULL,
    next_line_id   INTEGER NOT NULL,   -- monotonic counter, never reused
    line_ending_crlf INTEGER DEFAULT 0
)

lines (
    file_key       TEXT NOT NULL,
    line_id        INTEGER NOT NULL,   -- durable identity (never reused)
    frac_order     TEXT NOT NULL,      -- LexoRank-style fractional key
    content_hash   TEXT,
    content        TEXT NOT NULL,
    parent_context TEXT,
    structural_hash TEXT,              -- cosmetic-change gate (§4.6)
    status         TEXT NOT NULL,      -- 'live' | 'tombstone'
    PRIMARY KEY (file_key, line_id),
    FOREIGN KEY (file_key) REFERENCES files(file_key) ON DELETE CASCADE
)
```

Indices: `idx_lines_file_order (file_key, frac_order)` for ordered reads,
`idx_lines_status (file_key, status)` for pruning.

`file_key` is a durable key rather than a bare path, so a rename does not orphan
the file's IDs. Deciding how renames are detected is a backlog item; until then
a path-derived key is acceptable and a rename behaves as it does today.

### 5.2 Migration

- `sessions.filepath` → `files.file_key` / `rel_path`; `sessions.file_hash` and
  `mtime` → `files.working_hash` / `working_mtime`.
- `sessions.session_id` is retired as an external concept, and may remain
  internally during the transition.
- `lines.sequence_id` → `lines.line_id`; seed
  `files.next_line_id = MAX(sequence_id) + 1`.
- `lines.sort_order REAL` → `lines.frac_order TEXT`: a one-time conversion
  assigns evenly-spaced keys in current `sort_order` order.
- `lines.status` defaults to `'live'`; `structural_hash` is added nullable.

`create_tables` already uses idempotent `CREATE TABLE IF NOT EXISTS` plus
additive `ALTER TABLE` guards, so the migration follows the existing pattern.

## 6. Invariants (test targets)

1. **ID durability**: a `line_id` never changes for the life of the file, across
   sessions, restarts, external edits, and moves.
2. **No reuse**: a retired `line_id` is never reassigned to a different line.
3. **Order/identity independence**: reordering changes only `frac_order`;
   rebalancing `frac_order` never changes a `line_id`.
4. **Reconciliation soundness**: after reconciliation the store's live lines are
   exactly the disk lines, in the same order.
5. **Targeted-conflict safety**: an edit to a line whose on-disk content changed
   under it fails loudly, never silently mis-targets.
6. **Cosmetic invariance**: running a formatter over a whole file preserves
   every `line_id`.
7. **Insertion endurance**: 1,000 consecutive insertions between the same pair
   of lines keep a correct order and mint no duplicate IDs.

## 7. Phasing

1. **Reconciliation gate + patience diff** (§4) on the current session storage.
   This is the correctness win and is worth having on its own; it also de-risks
   step 2, because a store that reconciles well can rebuild a lost session from
   disk.
2. **Durable store** (§3, §5): monotonic counter, `frac_order`, tombstones,
   migration.
