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

All three layers are Jujutsu colocated with Git, so `jj` and `git` both work in
each. Read the
[`use-jujutsu-safely`](https://github.com/zaeku/skills/tree/main/plugins/version-control/skills/use-jujutsu-safely)
skill before an unfamiliar `jj` command.

**Commit unsigned and sign before pushing.** `master` on the remote requires a
signature, and signing each commit as it is made puts a hardware approval in the
middle of every change. `jj sign` signs what `revsets.sign` names, which is what
is still mutable — everything not yet pushed. Then `jj git push`.

**Do not sign what is already pushed.** The remote refuses a non-fast-forward,
so a re-signed commit that is already there cannot land. `mutable()` excludes
them for the same reason, so a bare `jj sign` is already the safe one.

**Write a change description from what the change does, not from what it was
for.** A description is true of the diff or it is false, and whoever reads one
is choosing not to read the diff.

**Do not discard existing changes.** They belong to the user unless a task
identifies them as agent changes.

## Language

Write documentation, project artifacts, code comments, and change descriptions
in English. Reply to the user in the language of their prompt.
