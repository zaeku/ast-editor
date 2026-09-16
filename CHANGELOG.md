# Changelog

What changed for someone who uses `ast-editor`, newest first. Why it changed is
in the commit that changed it, and what the project holds true is in
`decisions/`.

## 0.3.10 — 2026-09-16

### Fixed

- **`outline` names every kind of definition a file declares.** It listed
  functions and what it called classes, which for Rust meant `struct` alone:
  an `enum`, a `trait`, a `type` and a `const` were each absent from a listing
  whose job is to be complete, and a TypeScript file declaring an interface, a
  type, an enum, a class and a function was answered with two of the five. Java
  `record`s, C `typedef`s and C++ `namespace`s were missing the same way. **An
  entry's `kind` is now the kind** — `enum`, `interface`, `trait`, `record` —
  where anything that was not a function used to come back as `class`.
- **An insert given a span is refused.** `insert_after` and `insert_before`
  act at one line. The script form said so; the JSON form took an `end_id`,
  dropped it without a word and inserted at `start_id`, so the same batch did
  different things depending on which form it arrived in. **A batch that
  carried a pointless `end_id` on an insert and appeared to work now fails**,
  which is what it was doing all along.

## 0.3.9 — 2026-09-16

### Changed

- **The last three machine-readable prefixes are gone**, which the 0.2.0 notes
  said already. `CHECKSUM_ERROR:` and `CONCURRENCY_ERROR:` survived that sweep
  by being written at the `bail!` that printed them rather than stored with
  every other message, and `PREVIEW_STALE:` survived it by being stored with
  the prefix already on it. All three now say what happened and what to do
  next. **A caller matching on any of those three strings will stop
  matching.**
- **`--version` reports the extensions beside each grammar.** It said how many
  grammars were compiled in and never which files they claim, so a reader
  asking whether `.mjs` is handled had nowhere to look. A line is now the
  name, the version pinned for it, and the extensions.
- **A capped read says how a long line is counted.** `Output truncated to 800
  lines max.` now adds that a line counts as one per 2048 characters. A caller
  whose read was cut short could not tell why the count did not match, and the
  rule was in no string the binary printed.
- **The API reference is the table the binary prints.** It rendered every
  schema as pretty-printed JSON, 421 lines where `ast-editor skill api` says
  the same thing in 75, and the install ships neither. It carries what the
  command prints now.
- **The crate builds a binary and no library.** `ast-editor` was never
  published as one — the command and the documents are what this project
  promises, and `src/` is an internal — but the target existed, so `use
  ast_editor::…` compiled. **It no longer does.** Nothing the command offers
  has changed.

### Fixed

- A file reformatted under the store kept its line ids, which is what the
  store is for, but nothing checked that they followed the content rather than
  the position. They do; the test that covers it could not previously tell the
  two apart.

## 0.3.8 — 2026-09-15

### Changed

- **A read that stops at the line cap says so in one wording.** Two messages
  reported the same fact and which one arrived depended on how the range had
  been asked for: a whole-file read was told `Line count limit (800 lines max)
  exceeded. Output capped at 800 lines.` and a range past the cap was told
  `Output truncated to 800 lines max.` Only the second one counted lines that
  were actually withheld, so it is the one that is left. **A caller matching on
  the first string will stop matching.** The number of lines answered has not
  changed.

### Fixed

- Two test processes running at once shared their fixtures and their store, so
  a build that tests in parallel could fail for reasons that had nothing to do
  with the code. This is invisible to anyone using the binary and is why the
  version before it was measured as better than it was.

## 0.3.7 — 2026-09-15

### Changed

- **A hash with no line number in front of it is refused.** `replace 11f6` was
  taken as an address and resolved against whatever line carried that content,
  which is editing by quoted text — the failure the ids exist to remove. A
  caller holding `1#11f6` for a line that has since gone is told the line is
  gone; the same hash without the number edited a different line that had
  picked up the old content. The form was in no document, so nothing was told
  it existed. `ast-editor skill usage` says what an address is, and the
  refusal now says it too.

## 0.3.6 — 2026-09-15

### Changed

- **`view <path> 40` answers with line 40, where it answered with 40 to the end
  of the file.** It said what `40,` says, so the four spellings had three
  meanings. The address is the one `sed -n` takes — which is why it is spelled
  this way at all, since a caller reaching for part of a file writes a sed
  address without being told to — and `sed -n '40p'` is one line. **A call
  passing a bare number gets one line now rather than the rest of the file.**
- The `--help` line for a range says the four forms are read as sed reads them,
  where it showed `40,80` alone.

## 0.3.5 — 2026-09-14

### Fixed

- **The heading-hierarchy warning names the levels it is about.** It said
  `found H3 after H3 without H3` for a document going from `#` to `###`: the
  message has four places to fill and each was filled with the same value,
  because `str::replace` replaces every occurrence and the code called it four
  times rather than filling one place at a time. It now reads `found H3 after
  H1 without H2`. Every other message in the tool already filled one at a time.

## 0.3.4 — 2026-09-14

### Fixed

- **`inspect --template classes` works on a `.c` file.** It answered `Invalid
  Tree-sitter query`, because the template it sent named `class_specifier`,
  which the C grammar does not have — the query was written for C++ and both
  languages were served the same one. C now gets `(struct_specifier)` and C++
  keeps both. Every other template pair the tool serves is now covered by a
  test that runs it against a file in that language.

## 0.3.3 — 2026-09-14

### Fixed

- **`create` with `return_ids` answers with every line it wrote.** It stopped at
  800, so the ids for a longer file had to be read back out of a file the caller
  had just written — the re-read the ids exist to remove. The 800-line cap is
  for a read that was given no bounds; a call that wrote the content stated its
  own amount. `create` no longer reports a line-count warning either, because
  nothing is withheld. The cap on `view` is unchanged.

## 0.3.2 — 2026-09-12

### Changed

- **An edit that is missing a field says which op, which field, and what the
  edit did carry.** `Missing start_id for replace op` is now `The 'replace' op
  needs start_id, and this one carries end_id, content. \`ast-editor skill api\`
  lists what each op takes.` The mistake is usually a field in the wrong place,
  so naming what arrived is what locates it.
- **A refused line id says which field it came in under**, since a batch
  carries several: `No line here is end_id 9#dead`.
- **An unknown directive is named as unknown**, where a script that opened a
  block after one was told the operation takes no content — which reads as
  though the operation were real and the block were the mistake.

## 0.3.1 — 2026-09-12

### Fixed

- **The skill document said a replaced line keeps its id.** It keeps its
  number; the id changes with the content, because the hash in it is of the
  content. `ast-editor skill usage` said otherwise, and a reader who believed
  it would hold an id that the next edit refuses — which is the failure the
  ids exist to prevent. The 0.3.0 note above is corrected in the same way.

## 0.3.0 — 2026-09-12

An address is a span now, which removes an operation and renames two fields.
**A script or a JSON batch written against 0.2 has to change.**

### Changed

- **`replace_range` is gone. `replace` takes the span.** An address is one id
  or two separated by a comma, the way `view src/main.rs 40,80` already reads a
  range: `replace 1a#b029,22#f8c3` is what `replace_range 1a#b029 22#f8c3` was.
  A span of one line is one line, so `replace 1a#b029` is unchanged.
- **`delete` takes a span too**, so `delete 1a#b029,22#f8c3` removes a block.
  It used to take one line, and removing several meant replacing the range with
  an empty payload.
- **`move` spells its range with the comma**: `move a,b before c`, where it
  took `move a b before c`.
- **`target_id` is `start_id`, `end_target_id` is `end_id`, and
  `dest_target_id` is `dest_id`.** These are the keys `inspect` and `outline`
  already answer with, so a match they found pastes into an edit without being
  renamed — which is what prompted the change. There are no aliases: the old
  names are refused.
- **An op that acts at one line says so when given a span.** `insert_after a,b`
  is refused by name rather than silently taking the first id.
- The first line of a replaced span keeps its line **number**, where a replaced
  range used to retire every number in it. The id still changes, because the
  hash in it is of the content: `2#e9d7` becomes `2#c032`, and an id held from
  before the edit is refused either way. The lines after the first are new.

## 0.2.1 — 2026-09-12

### Fixed

- **A `replace` given several lines answers with all of them.** It wrote them
  all and reported one, and the id it reported named no line: the sequence
  number the edit had just retired, carrying the hash of the whole payload. A
  following edit addressed with it was refused — "That line is gone" — about a
  line that was there under a different id. `replace_range` and the insert
  operations were unaffected.

## 0.2.0 — 2026-09-12

A minor release rather than a patch because several answers changed shape, and
one of them changes what an identical script does.

### Changed

- **An empty payload writes nothing.** `replace_range 1#a 2#b ``` ``` ` removes
  the span, where it used to leave one blank line; `replace` with an empty
  payload removes the line. Content is line-terminated text now: `""` is no
  lines, `"\n"` is one empty line, and a trailing newline ends the last line
  rather than starting another. The JSON form reads it the same way. **This is
  the one change that alters what an existing script does without saying so** —
  a script that relied on an empty fence to leave a blank line now deletes.
- **`modified_ids` is `modified_lines`, and `view --only-ids` answers with
  `lines` rather than `ids`.** Each entry is a line as `[id, line number]`:
  `[["2#9639",2], ["4#b7a3",3]]` where `edit` used to answer `["2#9639",
  "4#b7a3"]`. The name says the shape, so nothing has to be read elsewhere to
  know it. The list is in file order, and an id a batch touched twice appears
  once.
- **A refused edit says what to do next**, and the two refusals differ because
  the remedies do: an id naming no line says the line is gone and there is
  nothing to re-read for it; a line that changed says to read that line again.
  Both add that the ids held for other lines are unaffected. The
  `CHECKSUM_ERROR` and `CONCURRENCY_ERROR` prefixes are gone.
- **A `--strict` refusal answers in the shape of a dry run** — `syntax_valid`,
  the diagnostics, and a `preview_id` — on stderr, still exiting non-zero. The
  batch it refused is kept, so `--apply <id>` commits it if you judge the parser
  wrong. A dry run whose result does not parse now returns an id too.
- **An `inspect` whose query does not compile is an error**, where it used to
  report zero matches with a hint and exit 0.
- **An edit to a file no grammar covers says so**, with `syntax_valid: null` and
  a message. An edit that parsed still answers with its ids alone.
- **`--version` reports the grammars compiled in**, each with its version, in
  place of the grammar directory it used to name.
- **`AST_EDITOR_WASM_DIR` does nothing**, and the install places no
  `share/ast-editor/wasm`. The install is the binary and the skill.
- **`.nix` files are syntax-checked.** A `--strict` edit that breaks one is now
  refused, where it used to be written.
- The preview refusals no longer tell you to run `edit_lines` with `dry_run`,
  which have not existed since the MCP server was removed.

### Added

- **`view` takes several paths**, answering with one block per file and a `json`
  block whose `files` array carries each file's metadata. One path answers
  exactly as before.
- **A line range can follow the path**, the way `sed -n '40,80p'` takes one:
  `40,80`, `40`, `40,`, `,40`. The options still work.
- **`<tool> --help`** prints that tool's options with their types and
  descriptions, instead of refusing the flag.

### Fixed

- **The first `edit` on a machine worked.** It failed with `no such table:
  sessions` unless something had read a file first, which the skill happens to
  tell you to do.
- **Grammars are found by where the binary is.** `CARGO_MANIFEST_DIR` was
  consulted first, so an `ast-editor` called from any other crate's `cargo run`
  or `cargo test` lost every language.
- **Valid Rust is no longer reported as broken.** The bundled grammar read
  `&raw` as the start of a raw borrow, so an ordinary borrow of a variable named
  `raw` failed to parse and `--strict` refused valid edits to such files.
- **`--help | head -30` no longer panics** on the closed pipe.
- **Reading a file with no grammar is quiet.** Every such read logged an error
  twice.
- `inspect` stops advertising `output_file`, which it accepted and ignored.

### Inside

- The grammars are Cargo dependencies compiled into the binary, at versions
  `Cargo.lock` pins, in place of WebAssembly modules loaded at run time.
  wasmtime is gone with them. The test suite runs in about a third of the time
  and the release build in half.
- The first thirty lines of `--help` carry what a first reader needs: the
  invocation, the tools, the id model, the directives, and the response shape.
