# Spec: Index-Backed Line Identity (`ast-editor`)

Turns the session store from a copy of the file into an index over it, then
makes the identity in that index survive external edits and restarts.

Guiding principle, unchanged from the original design: *disk is the ground
truth; the ID store is a durable index over it, reconciled — never blindly
rebuilt.* What changed is that the store now takes that literally and holds no
file content at all.

Split out of [the original design](../archive/2026-07-12-persistent-line-ids-and-reconciliation-design.md).
Git-checkpoint compaction, semantic-path targeting, the entity layer, and the
content-addressed storage rewrite are in [the backlog](../backlog.md) and are
not part of this spec. Do not start this before
[dry-run preview](dry-run-preview-spec.md), which ships on the current model.

## 1. Objectives

- **Bounded storage**: the store holds identity and order, not a second copy of
  every file the tool has touched.
- **Durable identity**: a line's ID stays stable across process restarts and
  external edits, for as long as the file's entry is retained.
- **External-edit resilience**: when a file changes outside the tool — another
  agent, a human, a formatter, a `git checkout` — reconcile via diff instead of
  discarding every ID.
- **Incremental tracking**: only changed lines get new IDs; unchanged regions
  keep theirs by construction.

### Non-Goals

- Reconstructing history the tool never witnessed, or restoring the ID map that
  was live at an older commit. Both are backlog items, and dropping them is what
  removes tombstones and compaction from this spec.
- Cross-machine ID synchronization.
- Real-time collaborative editing. The target is sequential reconciliation, not
  live merge. Two agents editing one file concurrently is out of scope by
  policy, not by mechanism.

## 2. Background

The current model ([session_db.rs](../../src/tools/session_db.rs)) keys a
session by `filepath` and stores each line as `(session_id, sequence_id,
line_hash, content, sort_order REAL, parent_context)`, surfacing IDs as
`{sequence_id:x}#{hash}` (e.g. `1#77cf`). Three weaknesses motivate the work,
in the order this spec addresses them.

1. **The store is a second copy of the codebase.** `lines.content` holds every
   line of every file touched. It buys nothing: `init_edit_session` computes
   `compute_sha256(filepath)` over the whole file on every tool call before it
   can decide whether to reuse a session, so the file is fully read regardless.
2. **Ephemerality.** On an `mtime`/`file_hash` mismatch the safe fallback is to
   rebuild the session, regenerating every `sequence_id`. The agent loses the
   IDs it was tracking exactly when a concurrent change occurred. `smart_resync`
   narrows this to the case where targeted lines survive intact, but the
   fallback is still a full rebuild.
3. **`sort_order REAL` precision exhaustion.** Fractional midpoint insertion
   between two adjacent lines exhausts the `f64` mantissa after roughly 50
   consecutive insertions between the same pair.

## 3. Phase 1 — Index, not copy

A refactor with no new capability, sequenced first because phases 2 and 3
rewrite the same code and would otherwise write it twice against storage that
is about to disappear.

### 3.1 Content moves to disk

Drop `lines.content`. An operation that needs file text reads the file into an
in-memory `Vec<String>`, which the tool already pays for via the whole-file
hash. The edit engine's operations — `replace_substring`, `move`,
`replace_range`, and the rest, roughly twenty sites that currently issue
`SELECT content FROM lines` — become operations on that buffer. The buffer is
written back to disk once, after validation.

### 3.2 Rollback becomes "do not write"

With content out of the store, an edit mutates a buffer, not the database.
Syntax validation runs on the buffer, and a failed strict validation simply
skips the write. `restore_session_lines` and its backup-and-restore path are
deleted, as is the class of defect where a restore silently loses a column.
Dry-run collapses into the same shape: it is an edit that never reaches the
write.

### 3.3 Two hash widths

`compute_line_hash` truncates to 4 hex characters. That stays as the
**surfaced** hash: it rides along in `{sequence_id:x}#{hash}` as a
human-readable change-detector, where the sequence number carries the
identification and a collision is therefore harmless.

Reconciliation needs a second, **stored** hash of at least 64 bits. Aligning
disk lines against stored rows happens before any ID is known, so there the
hash is the only key available and 16 bits collide well within one file. The
two are separate fields with separate jobs; neither is identity.

### 3.4 Scope

Behavior is unchanged, so the existing suite is the regression test. Add
coverage for a file whose lines are not unique, since content is no longer
available to disambiguate them.

## 4. Phase 2 — JIT diff reconciliation

Replaces "rebuild on mismatch" and is the heart of external-edit resilience.

### 4.1 Scope of the guarantee

The contract is narrow and sufficient: **open → reconcile-now → stable IDs
onward**. When an agent opens a file the on-disk state is reconciled into the
index at that instant, and IDs stay stable across its edits from then on.

### 4.2 Trigger

On any tool entry that touches a file (`view_lines`, `edit_lines`,
`inspect_ast`), compare the current `working_hash` and `mtime` against the
stored values. On a match, skip reconciliation. On a mismatch, reconcile
**before** serving or mutating. One gate, shared by the open-time and edit-time
paths.

### 4.3 Algorithm

Use **patience diff** (or histogram diff), not naive Myers. Code is dense with
duplicate lines (`}`, blank lines, `return None`) and naive LCS mis-aligns
them. Patience diff anchors on lines unique in both versions, then recurses
into the gaps. It operates on the stored hash sequence against the hashed disk
lines; no content is required on either side.

Reconciling the index against disk:

- **Unchanged** line → keep its ID and fractional key; refresh cached `mtime`.
- **Inserted** line → allocate a fresh ID, assign a fractional key between its
  neighbors.
- **Deleted** line → delete the row. No tombstone; see §5.2.
- **Moved** line → same ID, new fractional key.

`similar`, added for the dry-run diff, provides the algorithm.

### 4.4 Disambiguation with `parent_context`

When two candidate matches are otherwise equal, prefer the alignment whose
surrounding structural context matches. This reduces mis-mapping in repetitive
blocks.

### 4.5 Edit-time conflict policy

When reconciliation at edit time finds that a **targeted** line was changed or
deleted on disk, fail with a `CONCURRENCY_CONFLICT` error naming the offending
IDs rather than guessing. Non-targeted changes reconcile silently. This
supersedes `smart_resync` and gives it a concrete diff algorithm.

### 4.6 Structural-hash cosmetic-change gate

A line hash is fragile against formatters: after `prettier`, `black`, or
`cargo fmt` every line's hash changes though no logic did, forcing a large
low-confidence re-alignment. Compute a **structural hash** of the enclosing AST
node — a hash over the tree-sitter node that skips comment nodes and normalizes
whitespace, covering node kinds plus trimmed leaf text. Formatting-only edits
leave it identical.

- If a region's on-disk structural hash equals the stored one, treat its lines
  as unchanged-cosmetic: preserve their IDs, refresh the line hashes, and skip
  the line diff for that region. A full-file reformat becomes an
  identity-preserving event instead of a mass re-ID.
- Only regions whose structural hash differs fall through to §4.3.
- Responses may carry a `change_kind` of `cosmetic` or `logic` per touched
  region, which is also useful for dry-run reporting.

The gate is an optimization and a confidence signal; correctness still rests on
the line diff for regions that actually changed.

## 5. Phase 3 — Durable identity

### 5.1 Line ID composition

A durable line ID is `{line_id:x}#{surfaced_hash}` where `line_id` is a
per-file **monotonic, never-reused** integer drawn from a per-file counter
(`next_line_id`). Deleting a line does not free its `line_id`. The surfaced
format is unchanged, so no agent-facing contract changes.

### 5.2 No tombstones

The original design retained deleted rows as tombstones. That was required only
to restore the ID map live at an older commit, which is now a backlog item.
Non-reuse is guaranteed by the monotonic counter on the file row alone, so a
deleted line's row is deleted outright and the store tracks live lines only.
This removes the growth that motivated the compaction phase.

### 5.3 Ordering

Replace `sort_order REAL` with a lexicographic fractional key (LexoRank-style,
base-62) stored as `TEXT`:

- Ordering is the natural lexicographic sort of the key.
- Insertion between two keys `A < C` produces `B` with `A < B < C` by string
  midpoint, which never exhausts — the string grows a character when needed.
- A periodic rebalance renormalizes keys and touches only ordering keys.

Identity and order are decoupled: reordering never changes a `line_id`, and
re-IDing never happens on reorder.

## 6. Schema

```sql
-- One row per tracked file (durable, replaces ephemeral "sessions")
files (
    file_key       TEXT PRIMARY KEY,
    rel_path       TEXT,
    working_hash   TEXT NOT NULL,      -- hash of current disk content
    working_mtime  INTEGER NOT NULL,
    next_line_id   INTEGER NOT NULL,   -- monotonic, never reused
    last_accessed_at INTEGER NOT NULL, -- eviction axis, see §7
    line_ending_crlf INTEGER DEFAULT 0
)

-- Index over the file's live lines. No content.
lines (
    file_key        TEXT NOT NULL,
    line_id         INTEGER NOT NULL,
    frac_order      TEXT NOT NULL,     -- LexoRank-style fractional key
    line_hash       INTEGER NOT NULL,  -- >= 64-bit, for reconciliation (§3.3)
    structural_hash TEXT,              -- cosmetic-change gate (§4.6)
    parent_context  TEXT,
    PRIMARY KEY (file_key, line_id),
    FOREIGN KEY (file_key) REFERENCES files(file_key) ON DELETE CASCADE
)
```

Index: `idx_lines_file_order (file_key, frac_order)` for ordered reads.

`file_key` is a durable key rather than a bare path, so a rename need not orphan
a file's IDs. How renames are detected is a backlog item; until then a
path-derived key is acceptable and a rename behaves as it does today.

### Migration

- `sessions.filepath` → `files.file_key` / `rel_path`; `sessions.file_hash` and
  `mtime` → `files.working_hash` / `working_mtime`.
- `lines.sequence_id` → `lines.line_id`; seed
  `files.next_line_id = MAX(sequence_id) + 1`.
- `lines.sort_order REAL` → `lines.frac_order TEXT`: a one-time conversion
  assigns evenly-spaced keys in current `sort_order` order.
- `lines.content` is dropped; `line_hash` is recomputed at the wider width on
  first reconciliation.

`create_tables` already uses idempotent `CREATE TABLE IF NOT EXISTS` plus
additive `ALTER TABLE` guards. Dropping a column is not additive, so this one
migration rebuilds the table.

## 7. Cache lifecycle

The store is a cache with a rebuild path. Disk is the ground truth, so evicting
a file's rows loses no data — only ID stability, and only for that file. The
question is never whether eviction is safe but what it costs, and it costs
nothing once no agent is still holding an ID for that file.

- **Eviction axis**: least-recently-accessed, as today. The current TTL of 30
  minutes is too short once IDs are meant to survive restarts; a window of days
  fits how long an agent may stay on a task. The exact figure is a tuning knob,
  not a design constant.
- **When cleanup runs**: today only `init_edit_session` prunes, so expired rows
  sit until the next session opens. Pruning belongs on every entry point.
- **Reclaiming disk**: deleting rows does not shrink the database. Measured on a
  live store, 342 of 627 pages were free — 55% of a 2.4 MB file holding 445 KB
  of live text — because `auto_vacuum` is off. Enable incremental auto-vacuum
  and reclaim during cleanup. Changing the mode on an existing database
  requires setting the pragma and then running a full `VACUUM` once.
- **Preview TTL**: `PREVIEW_TTL_SECONDS` is an hour, longer than the session
  window. Previews depend on the file hash rather than the session so they
  still work, but the two should be brought into line.

## 8. Invariants (test targets)

1. **ID durability**: a `line_id` never changes for the life of the file's
   entry, across restarts, external edits, and moves.
2. **No reuse**: a retired `line_id` is never reassigned while the entry lives.
3. **Order/identity independence**: reordering changes only `frac_order`;
   rebalancing never changes a `line_id`.
4. **Reconciliation soundness**: after reconciliation the index's lines are
   exactly the disk lines, in the same order.
5. **Targeted-conflict safety**: an edit to a line whose on-disk content changed
   under it fails loudly, never silently mis-targets.
6. **Cosmetic invariance**: running a formatter over a whole file preserves
   every `line_id`.
7. **Insertion endurance**: 1,000 consecutive insertions between the same pair
   of lines keep a correct order and mint no duplicate IDs.
8. **Duplicate-line safety**: a file whose lines are not unique reconciles
   without mis-assigning IDs.
9. **No content at rest**: no table holds file text.
