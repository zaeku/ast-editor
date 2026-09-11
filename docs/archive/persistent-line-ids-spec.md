# Spec: Index-Backed Line Identity (`ast-editor`)

> Delivered, all three phases. The phase framing is how it was built, not how it
> works: sections 6 to 8 — the schema, the cache lifecycle, and the invariants —
> describe the store as it stands, and are the part worth reading.

Turns the session store from a copy of the file into an index over it, then
makes the identity in that index survive external edits and restarts.

Guiding principle, unchanged from the original design and held as
`D-01M27K2D7Q5BZ5` in the decision layer since 2026-09-11. What changed when
this was built is that the store takes it literally and holds no file content
at all.

Split out of [the original design](../archive/2026-07-12-persistent-line-ids-and-reconciliation-design.md).
Git-checkpoint compaction, semantic-path targeting, the entity layer, and the
content-addressed storage rewrite are out of scope: what might still become
work is in [the backlog](../backlog.md), and what was reasoned out of existence
is in [discarded](../discarded.md). Do not start this before
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
  was live at an older commit. Dropping both is what removes tombstones and
  compaction from this spec; see [discarded](../discarded.md).
- Cross-machine ID synchronization.
- Real-time collaborative editing. The target is sequential reconciliation, not
  live merge. Two agents editing one file concurrently is out of scope by
  policy, not by mechanism.

## 2. Background

The current model ([session_db.rs](../../src/tools/session_db.rs)) keys a
session by `filepath` and stores each line as `(session_id, sequence_id,
line_hash, content, sort_order REAL, parent_context)`, surfacing IDs as
`{sequence_id:x}#{hash}` (e.g. `1#77cf`). Two weaknesses motivate the work, in
the order this spec addresses them.

1. **The store is a second copy of the codebase.** `lines.content` holds every
   line of every file touched. It buys nothing: `init_edit_session` computes
   `compute_sha256(filepath)` over the whole file on every tool call before it
   can decide whether to reuse a session, so the file is fully read regardless.
2. **Ephemerality.** On an `mtime`/`file_hash` mismatch the safe fallback is to
   rebuild the session, regenerating every `sequence_id`. The agent loses the
   IDs it was tracking exactly when a concurrent change occurred. `smart_resync`
   narrows this to the case where targeted lines survive intact, but the
   fallback is still a full rebuild.
The original design counted `sort_order REAL` precision exhaustion as a third:
midpoint insertion between two adjacent lines exhausts the `f64` mantissa after
roughly 50 insertions between the same pair. Phase 1 removed the midpoints, so
it no longer applies — see §5.3.

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

- **Unchanged** line → keep its ID; refresh cached `mtime`.
- **Deleted** line → delete the row. No tombstone; see §5.2.
- **Inserted** line → allocate a fresh ID, unless it reclaims one below.
- **Moved** line → reads as a delete plus an insert. An inserted line whose
  hash matches a deleted one is that same line relocated, and reclaims its ID.
  Ties go in order of appearance.

`similar`, added for the dry-run diff, provides the algorithm.

### 4.4 Disambiguation

Duplicate lines are separated by position: the diff's anchors fix where each
run of identical lines belongs, so `parent_context` is not consulted. It stays
a display concern, feeding the enclosing-context list in `view_lines`.

### 4.5 Edit-time conflict policy

When reconciliation at edit time finds that a **targeted** line did not
survive, fail with `CONCURRENCY_ERROR` naming it rather than guessing which
line was meant. Non-targeted changes reconcile silently.

The edit path therefore reconciles *before* the read path can, carrying the ids
its batch targets: the gate in §4.2 knows nothing about targets, so were it to
run first it would retire the targeted line quietly and the edit would fail
later with a confusing "line not found".

### 4.6 Whitespace-insensitive fallback matching

A line hash is fragile against formatters: after `prettier`, `black`, or
`cargo fmt` every respaced line hashes differently though nothing about it
changed, and each one would draw a fresh id.

Each row therefore carries a second hash of the line with **every space
removed**. Reconciliation runs it as a third pass, after §4.3's diff and the
exact-match reclaim: a line the diff called new, whose spacing-free hash
matches one it called deleted, is that same line respaced and keeps its id.
Collapsing runs of whitespace instead would not be enough, because formatters
add and remove spaces around operators and delimiters rather than only at the
margin.

The original design specified a structural hash over the enclosing tree-sitter
node, skipping comments and normalising whitespace, used as a per-region gate.
This is the smaller thing that satisfies the invariant the gate existed for
(§6.6): a reformat preserves every id. It also does less. A formatter that
re-wraps lines — splitting one long line into three — changes line identity in
a way no line-level hash can carry, and the AST version would not carry it
either, since the region's line count changed. What the AST version would add
is insensitivity to comment-only edits and a cheaper path for large files;
neither is worth a tree-sitter parse on every reconcile. Recorded in
[discarded](../discarded.md).

An edit whose target was respaced under it is still refused, because the hash
in the agent's `target_id` no longer matches the line. Preserving the id keeps
the agent's *other* ids valid, which is the point; it does not promise that a
stale target still applies.

## 5. Phase 3 — Durable identity

### 5.1 Line ID composition

A durable line ID is `{line_id:x}#{surfaced_hash}` where `line_id` is a
per-file **monotonic, never-reused** integer drawn from a per-file counter
(`next_line_id`). Deleting a line does not free its `line_id`. The surfaced
format is unchanged, so no agent-facing contract changes.

### 5.2 No tombstones

The original design retained deleted rows as tombstones. That was required only
to restore the ID map live at an older commit, a goal since dropped.
Non-reuse is guaranteed by the monotonic counter on the file row alone, so a
deleted line's row is deleted outright and the store tracks live lines only.
This removes the growth that motivated the compaction phase.

### 5.3 Ordering

`sort_order REAL` stays. The precision worry that motivated replacing it with a
lexicographic fractional key does not apply to what phase 1 built: an edit
rewrites the whole ordered run as evenly spaced values, so no midpoint is ever
subdivided and the mantissa is never approached. A fractional key would buy
only the ability to renumber one row instead of all of them, which is a
throughput concern and not one this tool has. Recorded in
[discarded](../discarded.md).

## 6. Schema

```sql
sessions (
    filepath         TEXT PRIMARY KEY,
    session_id       TEXT UNIQUE NOT NULL,
    file_hash        TEXT NOT NULL,      -- hash of current disk content
    mtime            INTEGER NOT NULL,
    next_line_id     INTEGER NOT NULL,   -- monotonic, never reused
    last_accessed_at INTEGER NOT NULL,   -- eviction axis, see §7
    line_ending_crlf INTEGER DEFAULT 0
)

-- Index over the file's live lines. No content.
lines (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT NOT NULL,
    sequence_id     INTEGER NOT NULL,   -- durable identity
    line_hash       TEXT,               -- >= 64-bit, for reconciliation (§3.3)
    norm_hash       TEXT,               -- spacing-free, for §4.6
    sort_order      REAL NOT NULL,
    parent_context  TEXT,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
)
```

The original design renamed these to `files` and `file_key`, matching the shift
from a session to a durable entry. Not done, and not planned on its own; see
[discarded](../discarded.md).

Following a file across a rename is a backlog item. Until then the key is
derived from the path, and a rename starts a new entry.

## 7. Cache lifecycle

The store is a cache with a rebuild path. Disk is the ground truth, so evicting
a file's rows loses no data — only ID stability, and only for that file. The
question is never whether eviction is safe but what it costs, and it costs
nothing once no agent is still holding an ID for that file.

- **Eviction axis**: least-recently-accessed. The window is
  `SESSION_TTL_SECONDS`, widened from 30 minutes to 7 days: the old figure
  predated IDs being expected to outlive a restart, and expired an agent's IDs
  over a lunch break.
- **When cleanup runs**: on `init_edit_session`, which every entry point calls,
  so no separate hook is needed. Expired previews are pruned there too rather
  than only when a new one is stored.
- **Reclaiming disk**: deleting rows moves pages to the free list without
  returning them, which had left 342 of 627 pages free in a live store. The
  store now runs in `auto_vacuum = INCREMENTAL`, with a pass on each cleanup;
  an existing database is switched by one full `VACUUM` at open.

## 8. Invariants (test targets)

An invariant adopted as a decision is stated there and not here, so that the
two cannot drift; this list keeps its numbering and names the id.

1. **ID durability**: a `line_id` never changes for the life of the file's
   entry, across restarts, external edits, and moves.
2. **No reuse**: a retired `line_id` is never reassigned while the entry lives.
3. **Order/identity independence**: reordering changes only `sort_order`;
   renumbering it never changes a `sequence_id`.
4. **Reconciliation soundness**: held as `D-01M27K2D7Q5BZ5`, with a fence,
   since 2026-09-11.
5. **Targeted-conflict safety**: held as `D-01M27JNJJDYSWD` in the decision
   layer, with a fence, since 2026-09-11.
6. **Cosmetic invariance**: respacing a whole file preserves every `line_id`.
7. **Insertion endurance**: 1,000 consecutive insertions between the same pair
   of lines keep a correct order and mint no duplicate IDs.
8. **Duplicate-line safety**: a file whose lines are not unique reconciles
   without mis-assigning IDs.
9. **No content at rest**: no table holds file text.
