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
- Leave the file alone: no disk write, no `mtime` bump, no change to any line
  or to the ID counter.
- When the result validated, return a short single-use `preview_id`.

Preview validates; apply assigns. An agent that needs the IDs of lines it just
created obtains them from a real `edit_lines` response.

## 2. Applying a preview

`edit_lines` takes `apply` (string). Given a `preview_id` and the `filepath` it
was taken on, it commits exactly the batch that preview validated and returns
its `modified_ids`. `edits` is not required in that call, so an agent confirms
a batch without re-emitting it.

A preview id is refused when:

- it is unknown, already applied, or older than an hour;
- it is addressed at a different file than the one it was taken on;
- the file changed since the preview was taken. The stored diff and syntax
  result no longer describe the outcome, so the id is dropped and the agent is
  told to preview again. Applying it anyway is still possible through an
  ordinary `edits` call.

Storing the batch is the only state a preview keeps, and it lives beside the
session rather than in it: the previewed lines themselves are never held.

## 3. Why preview mints no line IDs

Predicting line IDs would make the preview lie when the real apply assigns
different ones. Reserving them would burn the counter on abandoned previews and
bring a reservation lifecycle to manage. A `preview_id` avoids both: it names
the pending batch, not the lines it will produce.

## 4. Verification Plan

1. `dry_run: true` on a batch that produces valid syntax returns the expected
   unified diff and reports success.
2. `dry_run: true` on a batch that breaks syntax reports the parse failure with
   the same error shape a real rolled-back edit produces.
3. After either of the above: file content, `mtime`, and every stored line ID
   and hash are byte-identical to their pre-call values.
4. A `dry_run: true` call followed by the same batch with `dry_run: false`
   yields the diff the preview predicted.
5. Markdown soft-warning cases surface warnings in the preview response without
   the preview being treated as a failure, and still carry a `preview_id`.
6. A failed preview carries no `preview_id`.
7. `apply` commits the previewed batch and returns `modified_ids`; a second
   apply of the same id, an apply against another file, and an apply after the
   file changed are each refused, and none of them writes.
