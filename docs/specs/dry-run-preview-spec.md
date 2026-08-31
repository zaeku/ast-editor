# Spec: Dry-Run Preview Diff (`ast-editor`)

Lets an agent verify that an edit batch parses cleanly before any of it touches
disk, reusing the apply→parse→rollback machinery `edit_lines` already has.

Split out of [the persistent line IDs design](../archive/2026-07-12-persistent-line-ids-and-reconciliation-design.md) (§6). It
depends on nothing else in that document and ships on the current session model.

## 1. Interface

Add `dry_run` (boolean, default `false`) to `edit_lines`. When `true`:

- Apply the edit batch to an **in-memory copy** of the current state.
- Run the same AST / structural validation used for real commits, including the
  hybrid verification split (hard fail for code and config languages, soft
  warnings for Markdown and HTML).
- Return the resulting unified diff plus the validation outcome.
- Mint **no line IDs**, and return no `modified_ids`.
- Write nothing: no disk write, no `mtime` bump, no session-DB mutation, no
  advance of any ID counter.

Preview validates; apply assigns. An agent that needs the IDs of lines it just
created obtains them from a real `edit_lines` response.

## 2. Why preview mints no IDs

Predicting IDs would make the preview lie when the real apply assigns different
ones. Reserving them would make the preview mutate state, which brings counter
burn on abandoned previews and a reservation lifecycle to manage. Preview stays
pure instead.

## 3. Verification Plan

1. `dry_run: true` on a batch that produces valid syntax returns the expected
   unified diff and reports success.
2. `dry_run: true` on a batch that breaks syntax reports the parse failure with
   the same error shape a real rolled-back edit produces.
3. After either of the above: file content, `mtime`, and every stored line ID
   and hash are byte-identical to their pre-call values.
4. A `dry_run: true` call followed by the same batch with `dry_run: false`
   yields the diff the preview predicted.
5. Markdown soft-warning cases surface warnings in the preview response without
   the preview being treated as a failure.
