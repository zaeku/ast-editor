# Agent Guidelines

Rules for an agent working on ast-editor. Read `README.md` first for what the
product does.

**Terms.** *Line id* is the `n#hash` pair a tool prints beside a line. *Session*
is the cached database that maps a file's lines to their ids. *Grammar* is a
tree-sitter parser compiled into the binary. *Script* is the edit-directive
format `ast-editor edit` reads from stdin. *Tool* is one of the five the binary
serves: `inspect`, `outline`, `view`, `edit`, `create`.

## Three layers

Ask which layer a change belongs in before you make it.

| Layer | Holds | Changes |
|---|---|---|
| This one | The implementation and the documents it publishes | Per work item |
| `decisions/` | The rules that stand, and the fences that fail when the code breaks one | When a rule does |
| `kanban/` | What is next | Hourly |

The other two are ignored here, carry their own history, and are not published
with this repository. A clone is the code layer alone, so a `D-…` id in a
comment names a rule the reader cannot look up. Write the sentence around such
an id to carry its reason by itself.

## Where code goes

New work joins the file whose subject it already is. Three files under
`src/tools/` are shared: `session_db.rs` owns the line-id store, `formatter.rs`
owns what output looks like, and `metadata.rs` owns every string the binary
prints.

## Decision layer

Read `decisions/AGENTS.md`, and `decisions/SPEC.md` §7 and §12, before you add a
decision or a fence.

**Move a rule there when a check can reach it.** A rule in this file is a rule
someone has to remember. A decision in `live/` with a fence in `fences/` fails a
check when it is broken. This file should end up holding only what no check
reaches.

**Ask also whether the code layer could be rebuilt without the rule.** A rule
that would have to be rediscovered belongs there whether or not anything can
execute it. §4 provides `fence: none` with `verified:` for that case.

**Write the fence in the same change that adopts the rule** (§8 Adopt), and
delete the rule from this file in that same change. A rule kept in both places
drifts, and the copy without the fence is the one that goes stale.

**Identify a rule by its opening sentence, not by a number.** Rules leave this
file one at a time, so a position shifts under every adoption.

**Keep no list of decisions here.** `live/` is the list, and
`cargo run --bin check` derives the rest. §2 allows references in one direction
only: the faster layer points at the slower one. To find which rule a decision
came from, run `jj log -r 'diff_lines(substring:"D-...")' -p` in whichever layer
you are standing in.

**Ask which surface a rule lives on before deciding it is out of reach.** §5
lets a fence read what the code layer exposes to anyone else: the `ast-editor`
command on `PATH` — its subcommands, its output, its exit codes, and the files
it rewrites — and the documents it publishes, which are `README.md`,
`agent_skill/SKILL.md`, `CHANGELOG.md`, this file, and the strings under
`resources/` that the binary prints back to a caller. §5 forbids implementation
internals: `src/`, `tests/`, a Cargo target, a `just` recipe.

**Adopt a decision about where this tool should end up before the code satisfies
it.** §5 says a fence that cannot find its subject exits 2, and that this is the
intended direction of work rather than a defect. The fence states the standard,
and `cargo run --bin check` answers how far off the code is.

**Write the numbers a decision relies on into its own body, and name no file
here.** A path into this layer dangles the moment this layer is what was lost.
Give the figure, how it was measured, and when.

**Run `just install` before `cargo run --bin check` in `decisions/`.** `check`
tests the `ast-editor` on `PATH`, and `target/release/` is not on it.

## Board

Run `kanban-md board` from the repository root. `kanban/` holds what is next
rather than what is true.

**Stand a rule's fence up before working a card that changes that rule.** The
card is done when the fence passes, not when the code compiles.

**Keep a finding off the board.** What was measured goes in a commit, and what
the project now holds true goes in `decisions/`. The board holds only what has
not been done yet.

**Give a waiting card its condition.** Work that could plausibly happen sits in
`backlog` with `--block` carrying the condition that would make it worth
scheduling, so `kanban-md list --blocked` is the list of things nobody has
committed to. Unblocking one says its condition is met. Keep no second list of
ideas anywhere.

**Send an idea you refuse outright to `decisions/` as a proposal, not to the
board.** Reject it there in the next change, which leaves a tripwire watching
for the code drifting into it.

## Generated documents

`just doc` renders `README.md` and `agent_skill/references/api_specification.md`
from `agent_skill/doc_templates/*.tpl.md`. Edit the template.

## Toolchain

Do not install a program globally to make a task work. Add it to the flake that
needs it, so the next reader gets it. Run `direnv allow` once: `.envrc` enters
the shell `flake.nix` declares, and points `CARGO_HOME` at `.cargo-cache/`
inside the tree.

`just` is the task list: `just build`, `just test`, `just lint`, `just doc`,
`just install`. Read `justfile` before adding a step elsewhere.

## Version control

This layer is Git, on `master`. `decisions/` and `kanban/` are Jujutsu colocated
with Git, so `jj` and `git` both work inside them. Read the
[`use-jujutsu-safely`](https://github.com/zaeku/skills/tree/main/plugins/version-control/skills/use-jujutsu-safely)
skill before an unfamiliar `jj` command.

**Write a change description from what the change does, not from what it was
for.** A description is true of the diff or it is false, and whoever reads one
is choosing not to read the diff.

**Do not discard existing changes.** They belong to the user unless a task
identifies them as agent changes.

## Language

Write documentation, project artifacts, code comments, and change descriptions
in English. Reply to the user in the language of their prompt.
