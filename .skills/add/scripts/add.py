#!/usr/bin/env python3
"""The `add` skill's worker: sum A and B (defaults 2 and 3), write the result file, and
append the line to add_log.txt (which the skill then commits). The SKILL.md just runs this
script — there is no arithmetic for the harness to (re)derive."""
import json
import os
import pathlib
import sys

# Resolve outputs against the repo root, not the current directory: this file is
# <repo>/.skills/add/scripts/add.py, so the root is three parents up. The result must land
# in <repo>/tmp/ regardless of where the harness runs the script from.
ROOT = pathlib.Path(__file__).resolve().parents[3]


def integer(name: str, default: str) -> int:
    raw = os.environ.get(name, "")
    raw = raw if raw.strip() else default
    try:
        return int(raw)
    except ValueError:
        sys.exit(f"{name}={raw!r} is not an integer")


a = integer("A", "2")
b = integer("B", "3")
line = f"{a} + {b} = {a + b}"

(ROOT / "tmp").mkdir(exist_ok=True)
(ROOT / "tmp" / "add_result.json").write_text(json.dumps({"result": line}))
with open(ROOT / "add_log.txt", "a", encoding="utf-8") as log:
    log.write(line + "\n")
print(line)
