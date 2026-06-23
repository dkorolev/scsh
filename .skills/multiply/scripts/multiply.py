#!/usr/bin/env python3
"""The multiply skill worker: product of X and Y (both required), write JSON under tmp/."""
import json
import os
import pathlib
import sys

# Resolve outputs against the repo root, not the current directory: this file is
# <repo>/.skills/multiply/scripts/multiply.py, so the root is three parents up. The result
# must land in <repo>/tmp/ regardless of where the harness runs the script from.
ROOT = pathlib.Path(__file__).resolve().parents[3]


def integer(name: str) -> int:
    raw = os.environ.get(name, "")
    if not raw.strip():
        sys.exit(f"{name} is required")
    try:
        return int(raw)
    except ValueError:
        sys.exit(f"{name}={raw!r} is not an integer")


x = integer("X")
y = integer("Y")
line = f"{x} * {y} = {x * y}"

(ROOT / "tmp").mkdir(exist_ok=True)
result_rel = os.environ.get("SCSH_RESULT", "tmp/multiply_result.json")
result_path = ROOT / result_rel
result_path.parent.mkdir(parents=True, exist_ok=True)
result_path.write_text(json.dumps({"result": line}))
print(line)
