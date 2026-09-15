#!/usr/bin/env python3
"""Put one mutant back into the working tree, by the name cargo-mutants gave it.

A mutation run's verdicts are audited rather than trusted (D-01M2C0Z0RAH6EG),
and this is what reproduces one: take a line from `missed.txt` or `caught.txt`,
apply it here, and run `cargo test` to see for yourself. Run `cargo test` and
not `just test`, because a lint failure would look like a mutant being caught.

Prints APPLIED, or a reason and a non-zero exit. It never edits anything it is
not sure of: a mutant it cannot place is no verdict rather than a guess.
"""
import pathlib
import re
import sys

spec = sys.argv[1] if len(sys.argv) > 1 else ""
named = re.match(r"^(.+?):(\d+):(\d+): (.+?)(?: in \S+)?$", spec)
if not named:
    print("UNPARSED: expected <file>:<line>:<col>: <what>")
    raise SystemExit(3)

path, line, col, what = named[1], int(named[2]), int(named[3]), named[4]
source = pathlib.Path(path)
lines = source.read_text().splitlines(keepends=True)


def replace_at(frm: str, to: str) -> None:
    """Swap one token where it was reported, or refuse."""
    at = col - 1
    target = lines[line - 1]
    if target[at : at + len(frm)] != frm:
        print(f"MISMATCH: found {target[at:at + len(frm)]!r} where {frm!r} was named")
        raise SystemExit(4)
    lines[line - 1] = target[:at] + to + target[at + len(frm) :]


returns = re.match(r"^replace (\S+) -> (.+?) with (.+)$", what)
guard = re.match(r"^replace match guard (.+?) with (.+)$", what)
arm = re.match(r"^delete match arm (.+)$", what)
swap = re.match(r"^replace (.+?) with (.+)$", what)
drop = re.match(r"^delete (\S+)$", what)

if returns:
    # The line named is the first line of the body, not the signature, so the
    # return goes in front of it. Assuming the signature wrote the return into
    # whatever function came next and called that a success.
    if line - 1 >= len(lines):
        print("NO-BODY: the named line is past the end of the file")
        raise SystemExit(4)
    body = lines[line - 1]
    indent = " " * (len(body) - len(body.lstrip()))
    lines.insert(line - 1, f"{indent}#[allow(unreachable_code)] return {returns[3]};\n")
elif guard:
    replace_at(guard[1], guard[2])
elif arm:
    # An arm can span lines, so its end is where the braces it opened close.
    depth, end = 0, line - 1
    while end < len(lines):
        depth += lines[end].count("{") - lines[end].count("}")
        tail = lines[end].rstrip()
        if depth <= 0 and (tail.endswith(",") or tail == "}"):
            break
        end += 1
    if end >= len(lines):
        print("NO-ARM: the arm named does not close")
        raise SystemExit(4)
    del lines[line - 1 : end + 1]
elif swap:
    replace_at(swap[1], swap[2])
elif drop:
    replace_at(drop[1], "")
else:
    print(f"UNSUPPORTED: {what!r}")
    raise SystemExit(3)

source.write_text("".join(lines))
print("APPLIED")
