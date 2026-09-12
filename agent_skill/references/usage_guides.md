# Usage Guides - `ast-editor`

How the id model behaves in the situations that come up while editing.

## Ids do not shift

`edit` targets a line by an id like `1#dfca`: a sequence number that names the
line and a hash that guards its content.

- **Inserting or deleting elsewhere changes nothing.** The ids you hold for
  other lines stay valid, so an id read before an edit still names its line
  after it.
- **Duplicate lines are still different lines.** Two identical lines have
  different ids, so there is no way to edit the wrong one by matching content.
- **An unchanged line keeps its id.** Ids from an earlier `view` can be used in
  a later edit without reading the file again.

## The store is a cache, not a lock

The ids live in a SQLite database under the user's cache directory. Files on
disk are read, written and closed; nothing in a workspace is held open, and
nothing is written beside the file being edited.

Losing the store costs the ids of the files it covered and nothing else. The
next call reads those files and mints new ones.

## When a file changes underneath

A file changed outside the tool has its index reconciled against what is now on
disk rather than rebuilt. A patience diff over the stored hashes decides what
survived: unchanged lines keep their ids, a line that only moved keeps its id,
and a line a formatter merely respaced keeps its id too. Only genuinely new
lines draw a new one, and a retired id is never handed out again.

That happens silently for lines you are not editing. If a line **your edit
targets** did not survive, the edit is refused and names that line — read it
again for its current id. The ids you hold for other lines are unaffected, so
only the refused one needs re-reading.

## What a syntax check does

An edit is parsed after it is applied, and the verdict is reported rather than
enforced: the answer carries `syntax_valid` and the diagnostics, and the file is
written. Prose is checked too — an unclosed code fence in Markdown comes back as
a warning — and a warning never blocks a write.

`--strict` refuses instead, leaves the file alone, and hands back a `preview_id`
that `--apply` commits if you judge the parser wrong.

A file no grammar covers is written with `syntax_valid: null`.

## Replacing a block

An address is one line id, or two separated by a comma for a span:
`replace 1a#b029,22#f8c3`. It is the comma `view src/main.rs 40,80` already
reads, and it is why there is no separate range operation — a span of one is
one line.

Replacing a block in one directive is better than a loop of single-line
`replace` and `delete` operations:

- The whole span is one batch, so it is parsed once and either reported on or,
  under `--strict`, refused as a unit.
- One call rather than several.
- Nothing in between can move, since the batch is applied against the ids it
  was given.

`delete 1a#b029,22#f8c3` removes a span the same way.

The first line of a replaced span keeps its **number**. An id is
`<number>#<hash>` and the hash is of the content, so writing new content there
mints a new id with the old number — `2#e9d7` becomes `2#c032`, and the id you
were holding is refused like any other stale one. What the number buys is that
the line is the same line: nothing else in the file has to move, and the
sequence number is not retired. The lines after the first are new lines with
new numbers.

Through `--json` the same edit is:

```json
{
  "filepath": "/path/to/project/src/main.rs",
  "edits": [
    {
      "op": "replace",
      "start_id": "1a#b029",
      "end_id": "22#f8c3",
      "content": "pub fn execute() -> Result<()> {\n    println!(\"Updated content!\");\n    Ok(())\n}"
    }
  ]
}
```

`start_id` and `end_id` are the keys `inspect` answers with, so a match it
found pastes into an edit without being renamed.

## Editing inside a long line

`replace_substring` edits within one line, given a `pattern` and a
`replacement`, which is what minified JavaScript, a long string or a single-line
JSON array needs. `view` soft-wraps such a line for display onto `│:` rows; the
id names the whole line however many rows it was shown on.

## Ids for a file you just wrote

`create` with `return_ids: false` skips serialising and hashing the lines back
to you, which is worth doing for a large file. The ids are still derivable,
because the file was written a moment ago and nothing has edited it yet:

- Line numbers are the 1-indexed positions of the content split on `\n`.
- The id of line N is N in lowercase hexadecimal, then `#`, then the first four
  characters of the SHA-1 hex digest of that line's content.

That holds for a file in the state `create` left it, and stops holding after the
first edit, which mints new ids. After that, ask: `view` with `only_ids` over
the range you care about answers with the pairs and no text.
