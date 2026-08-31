# Design Spec: Persistent Line IDs, Diff Reconciliation, and Git-Checkpoint Compaction

This design spec proposes a foundational shift in how `ast-editor` maintains line identity. Today, line IDs live inside an ephemeral per-file **session** that is discarded and regenerated whenever the file changes out from under the tool. This document specifies a move to **durable, single-system-persistent line IDs** that survive external edits, are reconciled against disk via a proper line diff, and are checkpointed against git history with bounded storage growth.

It also folds in three agent-requested capabilities that naturally belong to the same layer: **incremental ID tracking**, **semantic AST-path targeting**, and **dry-run preview diffs**.

The guiding principle: *disk is the ground truth; the ID store is a durable index over it, reconciled — never blindly rebuilt.*

---

## 1. Objectives

- **Durable identity**: A line's ID remains stable across sessions, process restarts, and external edits, for the lifetime of the file on a single system.
- **External-edit resilience**: When the file is modified outside the tool (another agent, a human, a formatter, a `git checkout`), reconcile the change into the ID store via diff instead of discarding all IDs.
- **Incremental tracking**: Only changed lines receive new IDs; unchanged regions preserve their existing IDs by construction (subsumes the "Incremental ID Tracking" request).
- **Bounded growth**: Prevent the line store from accumulating unbounded tombstones, using git commits as natural compaction checkpoints, without invalidating any live IDs.
- **History safety**: Survive `git reset` / `git checkout` to an older commit by restoring the exact IDs that were live at that commit.
- **No git hard-dependency**: The identity layer works standalone; git is an *optional* accelerator for checkpointing and history rewind.
- **Simulation before commit**: Let agents preview the resulting diff and syntax-validation outcome of an edit without persisting it.
- **Semantic addressing**: Let agents target nodes by structural path (e.g. `class:Foo/fn:solve`) that resolves down to stable line IDs.

### Non-Goals

- Cross-machine / distributed ID synchronization (single-system scope only).
- Replacing git as the source of truth for file content or history.
- Real-time collaborative editing (CRDT convergence across concurrent writers); we target sequential reconciliation, not live merge.

---

## 2. Background: Why the Session Model Breaks

The current model (`src/tools/session_db.rs`) keys a session by `filepath` and stores each line as `(session_id, sequence_id, line_hash, content, sort_order REAL, parent_context)`. A line ID surfaced to the agent is formatted as `{sequence_id:x}#{hash}` (e.g. `1#77cf`).

Two structural weaknesses motivate this redesign:

1. **Ephemerality**: On a detected `mtime`/`file_hash` mismatch, the safe fallback is to rebuild the session, which regenerates `sequence_id`s. Every line the agent was tracking loses its ID, defeating shift-invariant targeting precisely when a concurrent change occurred.
2. **`sort_order REAL` precision exhaustion**: Fractional midpoint insertion between two adjacent lines (`0.5`, `0.25`, `0.125`, …) exhausts the `f64` mantissa after roughly 50 consecutive insertions between the same pair. Under a durable store where insertions accumulate over a file's lifetime, this is a real corruption risk, not a theoretical one.

---

## 3. Architectural Overview

```
                    +-------------------------------+
                    |         edit_lines /           |
                    |   view_lines / inspect_ast     |
                    +---------------+---------------+
                                    |
                                    v
                    +-------------------------------+
                    |      Reconciliation Gate       |   <-- single entry point
                    |  (open-time & edit-time both)  |       for "sync to disk"
                    +---------------+---------------+
                                    |
                 working_hash mismatch? ---- no --> proceed
                                    | yes
                                    v
              +-------------------------------------------+
              |   JIT Diff Reconciler (patience/histogram) |
              |   disk lines  <->  hot index (id-mapped)   |
              +----------------------+--------------------+
                                     |
             +-----------+-----------+-----------+-----------+
             | unchanged | inserted  |  deleted  |  moved    |
             |  keep id  | new id    | tombstone | keep id   |
             +-----------+-----------+-----------+-----------+
                                     |
                                     v
         +-------------------------------------------------+
         |   HOT store  (live working-tree line index)      |
         +----------------------+--------------------------+
                                | on git commit: snapshot & prune
                                v
         +-------------------------------------------------+
         |   COLD archive  (compressed id-map per commit)   |
         |   keyed by commit SHA; GC by reachability        |
         +-------------------------------------------------+
```

The system has three planes:

1. **Reconciliation Gate** — the *only* path by which the store is brought into agreement with disk. Both "opening" a file and "editing under an `mtime` mismatch" route through it, so there is exactly one reconciliation code path to reason about and test.
2. **Hot store** — the durable index for the current working-tree state. Holds live lines and short-lived tombstones. Aggressively pruned.
3. **Cold archive** — compressed snapshots of the `line_id → content` map at each git commit, keyed by commit SHA. The safety net for history rewind.

---

## 4. Persistent Identity Model

### 4.1 Line ID composition

A durable line ID is `{line_id:x}#{content_hash}` where:

- **`line_id`**: a per-file **monotonic, never-reused** integer. Allocated from a per-file counter (`next_line_id`). Deleting a line does *not* free its `line_id`. This is the identity.
- **`content_hash`**: short hash of the (trimmed) content, as today. Serves as a cheap change-detector and human-readable disambiguator — *not* as identity.

This preserves the existing surfaced format (`1#77cf`) so no agent-facing contract changes.

### 4.2 Ordering: replace `sort_order REAL` with a fractional-index string

Adopt a **lexicographic fractional key** (LexoRank-style, base-62) stored as `TEXT`:

- Ordering is the natural lexicographic sort of the key string.
- Insertion between two keys `A < C` produces a key `B` with `A < B < C` by string midpoint, which **never exhausts** — the string simply grows a character when needed.
- A periodic (or on-compaction) rebalance can renormalize keys to keep them short, and rebalancing changes *only* ordering keys, never `line_id`s.

Identity (`line_id`) and order (fractional key) are thus fully decoupled: reordering a line never changes its ID, and re-IDing never happens on reorder.

---

## 5. JIT Diff Reconciliation

This is the heart of external-edit resilience and replaces "rebuild on mismatch."

### 5.0 Scope of the identity guarantee (bounded responsibility)

The identity guarantee is deliberately **scoped to the agent's editing session, from the moment a file is opened onward** — *not* to reconstructing the file's entire past. When an agent opens a file (`view_lines`/`inspect_ast`/`edit_lines`), the current on-disk state is reconciled into the store at that instant; from then on the agent receives consistent, stable line IDs across its edits. The contract to uphold across every scenario in this document is narrow and sufficient: **open → reconcile-now → stable IDs onward.**

Distinguish two kinds of past, however:

- **Git-missed history** (out of scope): changes we never witnessed and git never recorded — e.g. a human editing and reverting a line without committing, or any uncommitted working-tree churn. We do **not** try to reconstruct these. That is over-reach.
- **Git-held history** (available enrichment, not obligation): commits that git *does* hold. There is no reason to discard these. They are not needed for the **core stable-ID guarantee** — an endpoint diff (last snapshot ↔ current content, §5.1–§5.5) suffices for that — but they are a legitimate signal for **blame, history, and attribution backfill**: the checkpoint archive (§7) can be lazily, on-demand backfilled for git-held commit SHAs we did not witness live, improving blame/rewind fidelity. Referencing git-held history is *using what git already recorded*, not reconstructing anything.

So: the core guarantee uses current content only; git-held committed history is an optional, SHA-keyed, cached enrichment for the history/blame dimension (bounded to committed history, with the §7.4 fallback when history is rewritten). We reference git's committed history rather than duplicating it into the overlay.

### 5.1 Trigger

On any tool entry that touches a file (`view_lines`, `edit_lines`, `inspect_ast`), compute the current disk `working_hash` + `mtime`. If they match the stored values, skip reconciliation. If they differ, run the reconciler **before** serving or mutating.

### 5.2 Algorithm

Use **patience diff** (or histogram diff), not naive Myers. The reason is specific to source code: code is dense with duplicate lines (`}`, blank lines, `return None`), and naive LCS mis-aligns them. Patience/histogram diff first anchors on lines that are **unique** in both the old and new versions, then recurses into the gaps between anchors. This yields stable, intuitive alignments on duplicate-heavy text.

Reconcile old (hot index) against new (disk):

- **Unchanged** line → keep its existing `line_id` and fractional key; refresh cached `mtime`.
- **Inserted** line → allocate a fresh `line_id`, assign a fractional key between its neighbors.
- **Deleted** line → mark tombstone (retire `line_id`, retain row until covered by a checkpoint — see §7).
- **Moved** line → same `line_id`, new fractional key (identity survives the move).

### 5.3 Disambiguation with `parent_context`

The existing `parent_context` column is a diff aid: when two candidate matches are otherwise equal (duplicate content), prefer the alignment whose surrounding structural context matches. This reduces mis-mapping in repetitive blocks.

### 5.4 Edit-time conflict policy

When `edit_lines` reconciles at edit time and a **targeted** line ID was itself changed or deleted on disk, the edit cannot be safely applied. Fail with a `CONCURRENCY_CONFLICT` error naming the offending IDs, rather than guessing. Non-targeted changes reconcile silently. (This supersedes the "smart resync" sketch in the 2026-07-12 improvements design and gives it a concrete diff algorithm.)

### 5.5 Structural-hash cosmetic-change gate (borrowed from `sem`)

Line `content_hash` alone is fragile against **formatters**: after `prettier` / `black` / `cargo fmt` runs, every line's `content_hash` changes even though no logic changed, forcing a large, low-confidence re-alignment. To distinguish *cosmetic* churn from *real* churn, compute a **structural hash** of the enclosing AST node — an `xxHash` stream over the tree-sitter node that **skips comment nodes and normalizes whitespace**, hashing node kinds (structure) plus trimmed leaf text (content). Formatting-only edits produce an identical structural hash. (Algorithm adapted from `sem`'s `utils/hash.rs`; the tree-sitter `Node` API is identical under our WASM backend, so the traversal ports almost verbatim.)

Reconciliation uses it as a fast pre-filter and a confidence signal:

- If a region's on-disk **structural hash equals** the stored structural hash, treat all its lines as *unchanged-cosmetic*: **preserve their `line_id`s** and refresh only content/`content_hash`, skipping line-level diff entirely for that region. This makes a full-file reformat an O(1)-per-entity identity-preserving event instead of a mass re-ID.
- Only regions whose structural hash **differs** fall through to the §5.2 patience/histogram line diff.
- The response may surface a `change_kind` of `cosmetic` vs `logic` per touched entity, which is also useful for `dry_run` (§6) and edit reporting.

This gate is an optimization and a confidence booster; correctness still rests on the line diff for regions that actually changed.

---

## 6. Dry-Run Preview Diff

A near-orthogonal, low-risk feature that reuses the existing apply→parse→rollback machinery.

### 6.1 Interface (FROZEN — see §17)

Add `dry_run` (boolean, default `false`) to `edit_lines`. When `true`:

- Apply the edit batch to an **in-memory copy** of the current state.
- Run the same AST / structural validation used for real commits.
- Return the resulting unified diff and the syntax-validation result. **No line IDs are minted or returned.**
- **Do not** write to disk, bump `mtime`, advance the `next_line_id` counter, or mutate the store in any way.

A real (non-dry) `edit_lines` mints the new line IDs and returns them as the `modified_ids` list (as it does today). Thus: **preview validates, apply assigns.**

### 6.2 Why no IDs in preview (resolved)

Minting or reserving IDs during preview would either make preview lie (predicted IDs that differ from the real apply) or force preview to mutate state (reserving from the counter → counter burn on abandoned previews, a reservation lifecycle with TTL/GC, and concurrency ownership). Both are rejected. Preview stays **pure** (§10 invariant 8) and returns only the diff + syntax outcome; the agent obtains concrete IDs from the apply response. If a follow-up edit must reference the new lines, it sequences after apply. base62 headroom (§16.7) is therefore spent on shorter *production* IDs, not on burnable preview IDs.

### 6.3 Value

Agents can run a "type-check in the loop" — verifying an edit parses cleanly — without spending a real edit-plus-rollback cycle and without any risk of desyncing the store.

---

## 7. Compaction & Git Checkpointing

### 7.1 The problem

Under durable identity, the `lines` table accumulates tombstones (deleted-line rows) indefinitely. We must reclaim space **without** invalidating any live `line_id` and **without** losing the ability to rewind history.

### 7.2 Hot / Cold separation

- **Hot store**: only live working-tree lines plus tombstones not yet captured by a checkpoint. Pruned aggressively.
- **Cold archive**: a compressed snapshot of the full `line_id → content` (+ fractional key) map, one per git commit, keyed by commit SHA.

### 7.3 On `git commit` (checkpoint)

1. Capture the current live id-map as a snapshot; compress it; store keyed by the new commit SHA.
2. **Compaction invariant**: only after a tombstone's line is represented in a persisted checkpoint may its row be pruned from the hot store. *Never prune a tombstone not yet covered by a checkpoint.* This ordering is what makes rewind lossless.
3. Live `line_id`s are untouched by compaction (satisfies the "compaction must not change IDs" requirement).

### 7.4 On `git reset` / `git checkout <sha>` (rewind)

This is exactly why snapshots are keyed by commit SHA. On rewind, the Reconciliation Gate detects a `working_hash` change and:

- If the new HEAD SHA has a stored checkpoint → **restore that id-map directly** (O(1) lookup, exact original IDs — no lossy re-derivation), then diff any working-tree modifications on top.
- If the SHA is unknown (e.g. rebased, detached, or predates caching) → fall back to JIT diff reconciliation against the nearest known snapshot.

Hard-deleting tombstones instead of archiving them would force the lossy fallback on *every* rewind; the compressed per-SHA archive turns the common case into a lookup. This validates the "dump & compress rather than delete" instinct.

### 7.5 Archive retention (GC)

Bound the cold archive by reachability: keep snapshots for commits reachable from `HEAD` up to a retention window (e.g. last `N` commits, configurable). Prune the rest. This caps growth while preserving realistic undo depth.

### 7.6 Standalone (no-git) mode

When the file is not in a git repository, or the agent does not commit, the identity layer still functions on `working_hash` + `mtime` reconciliation. Checkpointing simply degrades to a periodic self-snapshot (e.g. time- or edit-count-triggered) rather than commit-triggered. Git must never be a hard runtime dependency.

---

## 8. Semantic AST-Path Targeting (Resolver, not Storage)

The requested `class:Foo/fn:solve/loop:0` addressing is valuable **as an addressing convenience that compiles down to line IDs**, not as a primary storage key.

### 8.1 Why not store semantic paths as identity

- **Ordinals are not shift-invariant**: `loop:0` slides if another loop is inserted before it — reintroducing the exact problem line IDs solve, one level up.
- **Name ambiguity**: overloaded functions, anonymous closures, duplicate identifiers break naming.
- **Requires a valid parse**: during in-progress edits the AST may be broken — the moment targeting is most needed is the moment a path may fail to resolve.

### 8.2 Design: resolve-to-line-IDs

Expose semantic paths as input sugar that the tool resolves against the *current* AST (reusing `inspect_ast`'s node→line-range mapping) into the underlying stable line IDs, which then drive the actual edit. Effectively this collapses "inspect_ast → pick node → get line IDs" into one step.

Rules:

- **Prefer named anchors** (`fn:solve`, `class:Foo`) over positional ordinals.
- When an **ordinal is unavoidable**, disambiguate with a nearby unique token rather than a bare index.
- If a path is ambiguous or fails to resolve (broken parse), return a resolution error listing candidates — never silently target the wrong node.

### 8.3 Rename continuity via name-excluded structural hash (borrowed from `sem`)

A pure name-based path breaks the instant an entity is renamed (`fn:solve` → `fn:resolve`), even though it is the same body. To carry identity across a rename, additionally index each entity by a **name-excluded structural hash** — the §5.5 structural hash computed with the entity's *name* byte range excluded (`sem`'s `structural_hash_excluding_range`). When a named anchor fails to resolve, the resolver falls back to matching the same-shaped body under a new name and can report `renamed: solve → resolve` while keeping the underlying line IDs stable. This is the entity-level analogue of §5.5's cosmetic gate: *same body, different name* is treated as continuity, not delete+insert.

---

## 9. Entity-Scoped Identity Layer

This section formalizes an optional layer *above* the file-scoped model of §4: tracking identity at the granularity of **code entities** (functions, classes, methods) in addition to lines, so that identity can survive a code block moving between files. It is deliberately scoped conservatively — the durable internal machinery is worthwhile, but the more ambitious agent-facing capabilities are pushed to a far-future phase for the cost reasons in §9.6.

### 9.1 The problem it solves (and doesn't)

File-scoped identity loses the thread when an entity moves across files: a function cut from `a.rs` and pasted into `b.rs` looks like a deletion plus an unrelated insertion, so its history, blame, and any IDs an agent was holding are severed. Entity-scoped identity lets the *same* internal entity — and therefore its lines — carry a stable identity across that move.

What it does **not** try to do (near-term) is preserve the *live, agent-facing line handle* across a cross-file move. That is a separate, expensive capability (§9.6) and is out of scope for the initial entity layer.

### 9.2 Two-layer identity: internal identity vs surfaced handle

The resolution to the "entity UUID is too long for agents to type" tension is the same decoupling used in §4 (identity vs. ordering) and §8 (addressing vs. storage): **never surface the durable identity; surface a short alias and resolve it.**

- **Internal identity (never surfaced).** Each entity gets a durable `entity_id`. Lines are stored as `(entity_id, intra-entity position)`. This is what survives cross-file moves. It may be a UUID or a monotonic integer; it is never shown to the agent.
- **Surfaced handle (what the agent types).** Unchanged from today: a short, per-file line handle `{seq:x}#{hash}` (e.g. `1#77cf`). It is an **alias** the tool maps onto the internal `(entity_id, line)` on every call. The agent never sees `entity_id`.

Because the surfaced contract is unchanged, adopting the entity layer is transparent to existing agents; it enriches the internal model without widening the API surface.

### 9.3 Stable entity naming: recognition-order ordinal, not structural hash

An entity needs a *stable* internal name. The structural hash (§5.5) must **not** be used for this: it is dynamic and changes on every edit to the entity's body, so it would be an unstable name. The structural hash is a **matching** signal only — used during reconciliation to recognize "this is the same entity, possibly moved or renamed" and thereby carry the existing `entity_id` forward — exactly as `sem` uses it in its matching phases (§14), never as an identity.

The stable name is instead a **monotonic entity ordinal**: entities are numbered in the order they are first recognized within their scope, and that number is retained for the entity's lifetime (the same never-reused monotonic-counter discipline as `line_id` in §4.1). Matching (structural hash) determines *which existing `entity_id` a parsed entity maps to*; the ordinal is *what that identity is called* durably.

### 9.4 The entity layer sits on top of the file layer (graceful degradation)

The file/module bucket remains the robust floor; entities are an enrichment layered above it, never a replacement. This preserves the tool's reliability on invalid or in-progress code.

- **Orphan lines.** Imports, top-of-file comments, blank lines, module-level statements, and most config-file lines belong to no named entity. They live in a **file/module-level bucket**, which is simply the file-scoped model of §4.
- **Broken-parse degradation.** When an edit leaves the AST unparseable and entities cannot be identified, the layer falls back to file-level buckets — i.e. today's behavior. Entity tracking is best-effort enrichment; correctness never depends on it.
- **Re-bucketing.** Entity spans shift as the file is edited, so line→entity assignment is recomputed on each reconcile. The **innermost** enclosing entity owns a line (nested method inside class inside module → the method).
- **Split / merge policy.** When one entity splits into two (or two merge), identity forks/joins by **largest structural overlap**: the largest-overlapping successor keeps the `entity_id`; others are minted as new entities. This is messier than rename and should be treated as a documented heuristic, not a guarantee.

### 9.5 Optional entity-qualified handle (deferred)

For the case where an agent wants to edit an entity it already holds in context *without* first re-viewing a file, an **entity-qualified handle** could be introduced: e.g. `@k7/3#77cf` = intra-entity line `3` of entity `k7`. The intended ergonomics:

- **Default stays bare.** Ordinary file-scoped editing continues to use plain line handles; nothing changes for the common path.
- **Opt-in, path-free entity edits.** When the agent supplies a full entity-qualified handle and *omits* the filepath, `edit_lines` resolves the entity globally and edits it immediately — no `view_lines`/path round-trip.

This is powerful but explicitly **deferred** (see §9.6).

### 9.6 What ships with the entity layer vs. what is deferred to the far future

- **Ships (when the entity layer lands):** internal `entity_id` carried across moves and renames via structural-hash matching, so **history, blame, and impact survive cross-file moves**; the cosmetic gate (§5.5) evaluated at entity granularity.
- **Deferred to the far future:** entity-qualified *surfaced* handles (§9.5), path-free global entity editing, and cross-file survival of the *live* agent handle. The blocker is scope, not design: resolving an entity *globally* — without a filepath — across many large projects at once requires **agent-session management** and a mapping of which project each agent session owns. That is a substantial systems undertaking orthogonal to the editing core, so it is intentionally postponed. The internal identity work in this section is designed so that adding these later is additive, not a rewrite.

---

## 10. Data Model & Migration

### 10.1 Proposed schema (conceptual)

```sql
-- One row per tracked file (durable, replaces ephemeral "sessions")
files (
    file_key       TEXT PRIMARY KEY,   -- stable key for the file on this system
    git_root       TEXT,               -- nullable (standalone mode)
    rel_path       TEXT,
    head_sha       TEXT,               -- last-known HEAD at reconcile time
    working_hash   TEXT NOT NULL,      -- hash of current disk content
    working_mtime  INTEGER NOT NULL,
    next_line_id   INTEGER NOT NULL,   -- monotonic counter, never reused
    line_ending_crlf INTEGER DEFAULT 0
)

-- Optional entity layer (§9): durable entity identity above the file layer.
-- entity_id survives cross-file moves; structural_hash is a MATCHING signal only.
entities (
    entity_id       INTEGER PRIMARY KEY,  -- durable, never reused (recognition-order ordinal / UUID)
    file_key        TEXT NOT NULL,        -- current owning file (mutable across moves)
    parent_entity_id INTEGER,             -- nesting: method < class < module (nullable)
    name            TEXT,                 -- may be NULL for orphan/file bucket
    structural_hash TEXT,                 -- dynamic: matching only, NOT identity
    FOREIGN KEY (file_key) REFERENCES files(file_key) ON DELETE CASCADE
)

-- Hot store: live working-tree lines + uncaptured tombstones
lines (
    file_key       TEXT NOT NULL,
    line_id        INTEGER NOT NULL,   -- durable identity (never reused)
    entity_id      INTEGER,            -- owning entity (§9); NULL = file-level bucket
    frac_order     TEXT NOT NULL,      -- LexoRank-style fractional key
    content_hash   TEXT,
    content        TEXT NOT NULL,
    parent_context TEXT,
    status         TEXT NOT NULL,      -- 'live' | 'tombstone'
    PRIMARY KEY (file_key, line_id),
    FOREIGN KEY (file_key) REFERENCES files(file_key) ON DELETE CASCADE
)

-- Cold archive: compressed id-map snapshot per commit
checkpoints (
    file_key       TEXT NOT NULL,
    commit_sha     TEXT NOT NULL,
    snapshot_blob  BLOB NOT NULL,      -- compressed line_id -> (content, frac_order)
    created_at     INTEGER NOT NULL,
    PRIMARY KEY (file_key, commit_sha)
)
```

Indices: `idx_lines_file_order (file_key, frac_order)` for ordered reads; `idx_lines_status (file_key, status)` for pruning; `idx_entities_struct (structural_hash)` for move/rename matching.

The `entities` table (and `lines.entity_id`) is inert until the §9 entity layer is implemented — `entity_id` stays `NULL` (file-level bucket) and every earlier phase behaves as if the column did not exist.

### 10.2 Migration from the current schema

- `sessions.filepath` → `files.file_key` / `rel_path`; `sessions.file_hash/mtime` → `files.working_hash/working_mtime`.
- `sessions.session_id` is retired as an external concept but may remain internally if convenient during transition.
- `lines.sequence_id` → `lines.line_id`; seed `files.next_line_id = MAX(sequence_id)+1`.
- `lines.sort_order REAL` → `lines.frac_order TEXT`: one-time conversion assigns evenly-spaced LexoRank keys in current `sort_order` order.
- New `lines.status` defaults to `'live'`; `checkpoints` and `entities` are new tables; `lines.entity_id` is added nullable and defaults to `NULL`.

Because `create_tables` already uses idempotent `CREATE TABLE IF NOT EXISTS` plus additive `ALTER TABLE` guards, the migration follows the same additive, backward-tolerant pattern.

---

## 11. Invariants (test targets)

These are the properties the implementation must uphold, and the basis for the test suite:

1. **ID durability**: a line's `line_id` never changes for the life of the file, across sessions, restarts, external edits, moves, and compaction.
2. **No reuse**: a retired `line_id` is never reassigned to a different line.
3. **Order/identity independence**: reordering changes only `frac_order`; re-IDing never happens on reorder, and rebalancing `frac_order` never changes any `line_id`.
4. **Reconciliation soundness**: after reconciliation, the hot store's live lines are exactly the disk lines in the same order.
5. **Targeted-conflict safety**: an edit to a line whose on-disk content changed under it fails loudly (`CONCURRENCY_CONFLICT`), never silently mis-targets.
6. **Compaction safety**: no tombstone is pruned before it is covered by a persisted checkpoint; live IDs are invariant under compaction.
7. **Rewind fidelity**: checkout to a checkpointed SHA restores the exact id-map that was live at that commit.
8. **Dry-run purity**: a dry-run mutates no persistent state (no disk write, no `mtime` bump, no counter advance).
9. **No-git parity**: all of the above hold in standalone mode, with self-snapshots substituting for commit checkpoints.

---

## 12. Suggested Phasing

Sequenced by correctness-per-unit-effort; each phase is independently shippable.

1. **JIT diff reconciliation** (§5) — biggest correctness/concurrency win; needed regardless of persistence. Introduce patience/histogram diff and route open-time + edit-time through one gate.
2. **Persistent line IDs** (§4) — monotonic counter + LexoRank `frac_order`, replacing session discard. Delivers incremental ID tracking for free.
3. **Dry-run preview** (§6) — mostly orthogonal, cheap; land early to help agents self-verify.
4. **Git-checkpoint compaction** (§7) — durability/history; hot/cold split, per-SHA snapshots, retention GC.
5. **Semantic-path resolver** (§8) — ergonomic layer over `inspect_ast`, resolving to line IDs.
6. **Entity-scoped identity — internal layer** (§9.1–§9.4, §9.6 "ships") — durable `entity_id` carried across moves/renames via structural-hash matching, so history/blame survive cross-file moves. Surfaced handles stay unchanged.
7. **Entity-qualified handles & path-free global editing** (§9.5, §9.6 "deferred") — far future; gated on agent-session ↔ project ownership management, not on the editing core.

Note: doing Phase 1 well shrinks the gap between "session" and "persistent," because a well-reconciling store can rebuild a lost session from disk + last snapshot — de-risking Phase 2. Phases 6–7 are strictly additive over the file-scoped floor and can be deferred indefinitely without blocking 1–5.

---

## 13. Future Vision: Impact & Blast-Radius Analysis

This is explicitly **out of scope for the phases above** and noted only to reserve the direction. `sem` builds a cross-file dependency graph (`parser/graph.rs`, `import_resolution.rs`, `scope_resolve.rs`) that answers "if this entity changes, what breaks?" — dependents, dependencies, and affected tests. That is an *analysis* surface, distinct from `ast-editor`'s *editing* surface, and importing it wholesale would dilute focus.

A focused, editing-adjacent slice is worthwhile, however: a **post-edit / dry-run blast-radius hint**. When `edit_lines` (or a `dry_run`) modifies an entity, the response can optionally list the entities and tests that reference it, e.g. `"blast_radius": { "dependents": [...], "tests": [...] }`. This pairs naturally with §6 (preview before commit) and reuses the same tree-sitter parses. Treat it as a later, optional tool (`inspect_impact`) rather than a near-term phase.

Philosophical note: `ast-editor` (safe *write* with durable identity) and `sem` (semantic *read* over git history) are complementary, not competing — both rest on the same "code is not text" premise. A natural integration is an edit pipeline that shows a `sem`-style entity diff after applying changes.

---

## 14. Prior Art / Borrowable Algorithms

This is not novel machinery; each piece maps to a proven technique:

- **LSP incremental document sync** — the shape of "keep a durable in-memory model reconciled against an authoritative buffer."
- **Patience / histogram diff** (git's own diff strategies) — duplicate-tolerant line alignment.
- **LexoRank / fractional indexing** — non-exhausting insertion ordering.
- **Content-addressed snapshots keyed by commit** — mirrors git's own snapshot-per-commit model for the cold archive.
- **`sem` structural hashing** (`Ataraxy-Labs/sem`, `utils/hash.rs`) — comment-stripped, whitespace-normalized `xxHash` over the AST for cosmetic-vs-logic change detection (§5.5) and name-excluded rename continuity (§8.3). MIT/Apache-licensed; algorithm ports directly since the tree-sitter `Node` API is backend-agnostic.
- **`sem` 6-phase entity matching** (`model/identity.rs`: exact ID → content-hash → same-signature-in-file → same-signature-across-rename → fuzzy token-Jaccard >80% → intra-file reorder) — an entity-granularity reference for our line-granularity reconciler's rename/move classification. Notably, `sem` needs six phases *because* `file::type::name` identity is unstable — direct evidence for our "semantic path as resolver, not storage key" decision (§8).

---

## 15. Open Questions

Consolidated design-discussion status (Confirmed / Acceptable / Needs review / Open) lives in §17. The genuinely-unresolved items are:

- **Retention window default**: what `N` balances undo depth against archive size for typical repos?
- **Multi-writer within one system**: remote is serialized by the single-threaded Durable Object (§16.6); the local case (two concurrent `edit_lines` on the same file, SQLite WAL + reconciliation gate) still needs an explicit contention test.
- **Semantic-path grammar**: exact syntax, escaping for identifiers with special characters, and how to express "the body of" vs. "the whole node."
- **Entity split/merge heuristic** (§9.4): largest-structural-overlap forking needs validation on real refactors.
- **Big-merge threshold** (§16.4, §17): at what merge size do we drop fine changesets and rely on the pre-merge checkpoint snapshot for rollback?
- Plus the §16.9 storage-format sub-questions (checkpoint cadence, schema-migration replay, overlay merge, page-1 blank-vs-synthesis, round-trip validation cost).

---

## 16. SSOT-as-SQLite: Content-Addressed Versioning & Git Projection

### 16.1 Position and premise

Everything above (§4–§9) treats the SQLite store as a *durable index over files*. This section specifies the inverse, more ambitious framing that the project is targeting: **the SQLite store is the single source of truth (SSOT), and the on-disk text files are projections ("shadows") of it.** The design goal is a *middle ground* — the DB is the SSOT, but it is version-controlled with Git-compatible semantics (branch, commit, checkout, diff, time-travel), and the exact same logic runs whether the backend is a local filesystem or Cloudflare (D1/R2).

The mechanism is to **reimplement Git's object model over the DB** rather than to store the DB as an opaque blob in Git. This is the same inversion Cloudflare Artifacts uses (logical objects in Durable-Object SQLite, a Git interface *projected* on top), applied to our identity store.

### 16.2 Data model: Git's object graph over a content-addressed block store

The DB (or the relevant slice of it) is chunked into fixed-size **content-addressed blocks**; identity and history are expressed as a Git-shaped graph over those blocks:

- **Block** — a fixed-size unit (SQLite's native page size, e.g. 4 KiB) identified by the SHA-256 of its bytes. Immutable and deduplicated: identical blocks across commits/branches share one stored object.
- **Manifest** — the ordered set of block hashes that reconstitutes a versioned artifact at a point in time (analogous to a Git tree). Scoped per file (see §16.5) so a single file can be materialized without the whole DB.
- **Commit** — references a manifest (plus parent commit(s), author, message, timestamp).
- **Ref** — a branch/tag name pointing at a commit.

Opening a file at a given `(branch|commit)` is then: ref → commit → manifest → fetch the referenced blocks → materialize the overlay → project the text file for reading/editing. Because blocks are content-addressed, unchanged data is never re-stored or re-fetched.

### 16.3 The SQLite physical-layout trap (primary risk)

Fixed-page content addressing is **correct for SQLite specifically** — SQLite files are page-aligned, so fixed 4 KiB chunking has no content-defined-chunking boundary-shift problem. **But** SQLite's *physical* page layout is not a deterministic function of its *logical* content:

- `VACUUM`, auto-vacuum, freelist reuse, `rowid` allocation order, and B-tree rebalancing mean logically-identical data can produce different page bytes, and a single-row insert can dirty many pages.
- Consequently "pages that changed between commits" ≠ "what changed logically." Naive page-diff dedup is therefore *loose*, and can churn far more blocks than the logical change implies.

Mitigations are mandatory: pin `page_size`; disable auto-vacuum and avoid `VACUUM` except as a deliberate, separately-versioned event; checkpoint WAL deterministically; keep schema stable. Even then, do **not** rely on page-diff to mean "logical diff" (see §16.4).

#### 16.3.1 Page-1 header normalization (co-designed with append-only writes)

The single largest churn source is **page 1**: its first 100 bytes are the DB header, and several fields there change on *every* transaction even when data is merely appended elsewhere — so page 1's block hash is always "new" and never dedups. The offending fields are at fixed offsets, which makes them blankable:

- `24–27` file change counter, `28–31` database size in pages, `32–35`/`36–39` freelist trunk head / freelist page count, `40–43` schema cookie, `92–95` version-valid-for, `96–99` library version number.

**Normalization scheme.** Before hashing/storing the page-1 block, overwrite those volatile ranges with a canonical constant (zero); persist their real values as a small per-commit **header patch** (~24–40 bytes) inside the `RefIndex` commit metadata. On materialize, re-inject the patch to reconstruct a byte-exact valid SQLite file. The stored page-1 block is then stable across commits that don't change the schema, so it dedups.

**Co-design with append-only writes (required, not optional).** Normalization only pays off when combined with a **logically append-only overlay** (append new `line_id`/tombstone rows; never update-in-place; never delete):

- No deletes → no freelist → offsets `32–39` stay zero (nothing to normalize there).
- Appends land on the rightmost b-tree leaf → physical page churn tracks logical append.
- With those two, page 1 is the *last* always-churning block, and normalization removes it — turning §16.3 from a *mitigation* into a near-structural resolution. Neither half suffices alone.

**Synergy with §16.5.** In an identity-only overlay the DB is only a few pages, so page 1 is a large fraction of the total; normalizing it has outsized benefit.

**Scope limit.** This fixes only page 1 (and fixed-offset header metadata). It does **not** address b-tree rebalancing churn on data pages — a rebalanced interior page has no "blank region" to normalize — which is precisely why the append-only write pattern (localizing rebalancing to the rightmost path) is a prerequisite rather than a nicety.

**Implementation notes.** The change counter and version-valid-for need only be *mutually consistent* for SQLite to accept the file, and the database-size field is *derivable* from the manifest block count; storing the real values is nonetheless recommended so that re-normalizing a materialized file yields the identical block hash (byte-exact round-trip). Snapshot from a quiescent state via `PRAGMA wal_checkpoint(TRUNCATE)`. Freeze the overlay schema early so the `sqlite_schema` b-tree on page 1 stays stable; treat schema migrations as separately-versioned events (§16.9). A round-trip `PRAGMA integrity_check` after reinjection is a required invariant/test (see §16.9).

### 16.4 Storage format: snapshot + changeset hybrid

To get both random-access ("open any commit instantly") and tight logical history ("what actually changed"), use two complementary representations, mirroring Git packfiles+deltas and Litestream WAL-shipping+snapshots:

- **Snapshot (page-blocks)** — at *checkpoint commits*, store the content-addressed page-block set. This is the random-access anchor for jumping to an arbitrary commit/branch.
- **Changeset (delta)** — between checkpoints, capture logical mutations via the **SQLite session extension** (changeset/patchset). These are deterministic, compact, and — critically — **invertible**, which directly powers the rollback/rewind semantics of §6/§7.

Checkpoint cadence trades storage (more snapshots) against replay cost (longer changeset chains).

### 16.5 Identity-only versioning (churn reduction)

The decisive optimization, consistent with §5.5/§9: **version the identity overlay, not the content.** The `lines.content` column duplicates bytes the projected file already holds, so the *versioned* DB slice should drop it and retain only `line_id, frac_order, content_hash, entity_id` (plus tombstones/checkpoint metadata). Content is recovered on open from the file materialized at that commit and validated against `content_hash`.

This keeps the versioned DB tiny → few pages → block dedup actually works well and the §16.3 trap is largely defused. (A fully self-contained SSOT that *includes* content in the DB remains possible, but must consciously accept the page-churn cost of §16.3; identity-only is the recommended default.) The small page count also amplifies §16.3.1: with only a few pages, normalizing page 1 removes a proportionally large share of churn.

### 16.6 Unified local/remote backends

The same logic runs on any backend by depending on two narrow interfaces:

- **`BlockStore`** — `hash → bytes`, put/get/exists over immutable content-addressed blocks. Local: files under `.ast-editor/blocks/<hash>`. Remote: R2 objects keyed by hash (HTTP Range + prefetch as caching details).
- **`RefIndex`** — `branch → commit → manifest`, and `manifest → block hashes`. Local: SQLite. Remote: D1 or Durable-Object SQLite.

| Concern | Local backend | Remote (Cloudflare) backend |
| --- | --- | --- |
| Block bytes (`BlockStore`) | `.ast-editor/blocks/<hash>` files | R2 objects (key = hash) |
| Ref/commit/manifest index (`RefIndex`) | SQLite | D1 or DO-SQLite |
| Concurrency serialization | file lock + WAL | single-threaded Durable Object |
| Content projection | write file to working tree | stream via Workers binding / REST |

Notes:

- **D1's 10 GB cap applies only to the index**, which is small (hashes + refs); blocks live in R2. This retracts the earlier blanket objection to D1 — D1 is unsuitable for blocks but appropriate for the `RefIndex`.
- **File-scoped manifests** (`(commit, filepath) → blocks`) enable partial hydration: opening one file fetches a handful of blocks, not the entire DB.
- **Garbage collection** must reclaim blocks unreachable from any live ref (equivalent to `git gc`); include it in the `BlockStore`/`RefIndex` contract.

### 16.7 Line-ID encoding: base62 monotonic integer

For very long-lived projects, encode the monotonic `line_id` (§4.1) as **base62 of a never-reused increasing integer** instead of hex (~5.95 bits/char vs 4), e.g. `1_000_000_000` → hex `3b9aca00` (8 chars) vs base62 `15ftgG` (6). Constraints:

- **Do not conflate identity and order encodings.** `line_id` is pure identity, so any base62 alphabet is fine (lexical sort is irrelevant). `frac_order` (LexoRank, §4.2) is the opposite: its alphabet **must** be sort-consistent because lexical order *is* logical order. Keep the two encodings separate.
- The dominant length contributor is often the `content_hash` suffix in `{seq}#{hash}`; once monotonic integer identity is in place, consider **shortening or making the hash suffix optional**, since its disambiguator role shrinks.
- A monotonic integer is **collision-free** (unlike a truncated hash), provided the never-reuse rule holds.

### 16.8 Git projection

Git compatibility is achieved by **projecting** Git semantics over the `RefIndex`, not by storing files in Git:

- `git log` / `git diff` / `git checkout` become views/operations over refs → commits → manifests; the working-tree files are materialized projections of the DB at the selected commit.
- Coherence on `checkout`/`reset` reuses the §7.4 rewind path, now driven by ref changes.
- Optionally expose a Git-speaking remote (as Cloudflare Artifacts does) so external Git clients interoperate — but this is an interface on top of §16.2, not a new storage substrate.

### 16.9 Open sub-questions (specific to §16)

- **Checkpoint cadence policy**: fixed commit-count, size-threshold, or adaptive by changeset-chain length?
- **Changeset across schema migrations**: session-extension changesets assume a stable schema; how are overlay-schema migrations versioned and replayed?
- **Merge semantics**: branch merges over identity overlays — three-way merge of `line_id`/`frac_order` maps, and conflict presentation when the same line diverges.
- **Total-SSOT variant**: if content *is* kept in the DB (§16.5 alternative), what compaction keeps page-churn tolerable for large binary-ish overlays?
- **Page-1: blank-and-reinject vs. full synthesis** (§16.3.1): store a header-normalized page-1 block and reinject, or omit page 1 entirely and synthesize the header at materialize time (storing only the schema portion as a block)? The latter is cleaner when the `sqlite_schema` region is managed separately, at the cost of a page-1 builder.
- **Round-trip validation cost** (§16.3.1): `PRAGMA integrity_check` on every materialize may be too expensive at scale — is a cheaper structural check (open + targeted query, or checking only the reinjected header/consistency fields) sufficient for the common path, reserving full `integrity_check` for tests and checkpoints?

---

## 17. Decision Log (Design-Discussion Consolidation)

Status of the decisions taken during design discussion, bucketed by confidence. This is the authoritative index; the cited sections carry the detail.

### 17.1 Confirmed (frozen)

- **Dry-run = validate-only, apply = assign** (§6, "L"): preview runs syntax + structural validation and returns the resulting diff, **mints no line IDs**; the real `edit_lines` mints IDs and returns them as `modified_ids`. No reservation, no counter burn; dry-run purity (§10 invariant 8) is preserved. base62 headroom funds shorter production IDs, not preview IDs.
- **Bounded identity guarantee + git-held enrichment** (§5.0, "F"): the contract is *open → reconcile-now → stable IDs onward*, using **current content only** for the core guarantee. We take no responsibility for **git-missed** history (un-witnessed / uncommitted churn). But **git-held** committed history is *not* discarded — it is an optional, SHA-keyed, cached enrichment for blame/history/checkpoint backfill (§7). We reference git's committed history rather than duplicate or reconstruct it.
- **base62 monotonic line handle** (§16.7): surfaced line handle is a short, per-(branch, file) base62 sequential integer; identity vs. order encodings kept separate (`frac_order` must stay sort-consistent).
- **No bit-packing of identifiers** (§16.7, "H"): do not embed branch/seq bits into a UUID's entropy or interleave them into a handle. Identifiers are separate logical fields, not physically interleaved.

### 17.2 Acceptable (adopted for v1, with a known cost)

- **Merge = re-anchor, not identity-merge** (§16.4, "B"): on merge, the disappearing branch's line IDs are dropped; the surviving branch reconciles the merged content via JIT diff (survivor lines keep IDs, incoming content gets fresh IDs). **Known cost:** line-level blame/history is lossy across a merge for incoming lines. This is acceptable because the loss is largely theoretical (the merged-in branch's identity space is deleted, so nothing live references its line IDs) and is backstopped: cross-merge continuity is preserved at the **entity** level by entity UUIDs (below), and **orphan lines** (imports, blank lines — not in any entity) fall back to **git blame**, since git holds their committed content history (§5.0). No line-level preservation mechanism is therefore needed.
- **Per-branch UUID + contextual line handle** (§16.6/§16.8, "H" clarified): each branch (a ref) carries a stable UUID in the `RefIndex`. Line handles do **not** need to embed branch bits and are **not** globally unique across branches — storage qualifies each line row by branch/commit, and the agent always operates within one branch's context, so handles are contextual and stay short. Cross-branch line coexistence is never required because identity spaces are not merged (see "B").
- **`file_key` = UUID + rename tracking** (§15, "D"): the durable file key is a UUID, not a path. Rename/move is followed primarily by content/structural-hash matching (§9), with git rename detection as a *corroborating hint only* — because git infers renames heuristically at diff time and cannot see uncommitted working-tree moves.
- **Rollback of large merges via pre-merge snapshot** (§16.4, "F"): fine invertible changesets power line-level rollback for ordinary edits; a large merge instead takes a checkpoint snapshot immediately before, and rollback restores that snapshot (O(1)). Avoids eager per-line changeset cost on big merges.
- **Entity identity = UUID; line identity = seq** ("H" resolution): put durable UUID identity at the **entity** level (§9), where blame/history naturally lives (as in `sem`), and keep lines as cheap per-branch seq. This is the resolution to the original worry behind "H" (that dropping dead-branch line IDs loses identity): the fix belongs at the entity level, not the line level. Per-line UUIDs — whether as a separate alias table or by packing an index into the handle's bits — are therefore *not* adopted; they would carry per-line UUID weight (doubling the largest table) for marginal gain, and bit-packing adds fixed-width-partition fragility on top. Revisit only if line-granular cross-merge blame proves necessary, in which case the clean form is per-line UUID + alias, never bit-packing.

### 17.3 Needs further review (direction set, details unresolved)

- **Additive-only overlay schema evolution** (§16.9, "E"): destructive schema migrations force a full rebuild (a defined hard-checkpoint boundary); the mitigation is to evolve the overlay schema **additively only** (nullable columns, side tables) so most changes avoid rebuild. Needs a concrete migration/versioning policy and a rule for what counts as "destructive."
- **Checkpoint cadence** (§16.9, "G"): decouple checkpoint *timing* (durability, a logical trigger: commit / op-count / WAL-size) from block *emission* (the page-aligned chunker already yields stable per-page blocks). Tying cadence to "emit exactly one fixed page block" is unnecessary. Needs the concrete trigger thresholds.
- **Worker portability of the parser** (§16, "C", deferred): the current parser runs tree-sitter grammars via `wasmtime`, which cannot run inside a Worker (V8 isolate). The in-Worker deployment must route tree-sitter through JS/WASM bindings instead. Also implies a codebase fork decision (dual Rust+TS vs. TS-only); deferred until the design settles.

### 17.4 Still open (see §15 and §16.9)

Retention-window default; local multi-writer contention test; semantic-path grammar; entity split/merge heuristic; big-merge threshold; and the §16.9 storage-format questions (schema-migration replay, overlay merge semantics, page-1 blank-vs-synthesis, round-trip validation cost).
