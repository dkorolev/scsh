#!/usr/bin/env python3
"""The subtract skill worker: C minus D (defaults 10 and 4), append the line to
subtract_log.txt and commit it, then write the JSON result under tmp/.

The ORDER is the point: the result file is what tells the harness the skill is done, so the
commit must exist before the result is written — a commit attempted after it races the
harness teardown and loses."""
import json
import os
import pathlib
import subprocess
import sys

# Resolve outputs against the repo root, not the current directory: this file is
# <repo>/.skills/subtract/scripts/subtract.py, so the root is three parents up. The result
# must land in <repo>/tmp/ regardless of where the harness runs the script from.
ROOT = pathlib.Path(__file__).resolve().parents[3]


def integer(name: str, default: str) -> int:
    raw = os.environ.get(name, "")
    raw = raw if raw.strip() else default
    try:
        return int(raw)
    except ValueError:
        sys.exit(f"{name}={raw!r} is not an integer")


c = integer("C", "10")
d = integer("D", "4")
line = f"{c} - {d} = {c - d}"

with open(ROOT / "subtract_log.txt", "a", encoding="utf-8") as log:
    log.write(line + "\n")

# Commit the deliverable (subtract_log.txt only — tmp/ is gitignored scratch). Best-effort
# so the script still works standalone outside a git repo; inside an scsh run clone this
# always succeeds and is what scsh rebases onto the caller's branch.
try:
    subprocess.run(["git", "add", "subtract_log.txt"], cwd=ROOT, check=True)
    subprocess.run(["git", "commit", "-m", f"subtract: {line}"], cwd=ROOT, check=True)
except (OSError, subprocess.CalledProcessError) as e:
    print(f"subtract.py: could not commit subtract_log.txt ({e})", file=sys.stderr)

(ROOT / "tmp").mkdir(exist_ok=True)
result_rel = os.environ.get("SCSH_RESULT", "tmp/subtract_result.json")
result_path = ROOT / result_rel
result_path.parent.mkdir(parents=True, exist_ok=True)
result_path.write_text(json.dumps({"result": line}))
print(line)
