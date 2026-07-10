#!/usr/bin/env python3
"""The add skill worker: sum A and B (defaults 2 and 3), append the line to add_log.txt and
commit it, then write the JSON result under tmp/.

The ORDER is the point: the result file is what tells the harness the skill is done, so the
commit must exist before the result is written — a commit attempted after it races the
harness teardown and loses."""
import json
import os
import pathlib
import subprocess
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

with open(ROOT / "add_log.txt", "a", encoding="utf-8") as log:
    log.write(line + "\n")

# Commit the deliverable (add_log.txt only — tmp/ is gitignored scratch). Best-effort so
# the script still works standalone outside a git repo; inside an scsh run clone this
# always succeeds and is what scsh rebases onto the caller's branch.
try:
    subprocess.run(["git", "add", "add_log.txt"], cwd=ROOT, check=True)
    subprocess.run(["git", "commit", "-m", f"add: {line}"], cwd=ROOT, check=True)
except (OSError, subprocess.CalledProcessError) as e:
    print(f"add.py: could not commit add_log.txt ({e})", file=sys.stderr)

# Regenerate PR-DESCRIPTION.md from the full log and commit it SEPARATELY. Inside an scsh
# clone this commit is authored as scsh's bot (dmitry.korolev+elon-presley@gmail.com) —
# exactly packdiff's notes-author convention, so the packed diff for this step lifts the
# file out of the diff and renders it as the commentable Description panel on top.
log_lines = (ROOT / "add_log.txt").read_text(encoding="utf-8").splitlines()
description = (
    "# add — running log\n\n"
    "Committed by the `add` demo skill; `packdiff` lifts this file into the\n"
    "review page's Description panel (the notes-commit convention).\n\n"
    "## Results so far\n\n" + "".join(f"- `{l}`\n" for l in log_lines)
)
(ROOT / "PR-DESCRIPTION.md").write_text(description, encoding="utf-8")
try:
    subprocess.run(["git", "add", "PR-DESCRIPTION.md"], cwd=ROOT, check=True)
    subprocess.run(["git", "commit", "-m", f"add: describe the run ({line})"], cwd=ROOT, check=True)
except (OSError, subprocess.CalledProcessError) as e:
    print(f"add.py: could not commit PR-DESCRIPTION.md ({e})", file=sys.stderr)

(ROOT / "tmp").mkdir(exist_ok=True)
result_rel = os.environ.get("SCSH_RESULT", "tmp/add_result.json")
result_path = ROOT / result_rel
result_path.parent.mkdir(parents=True, exist_ok=True)
result_path.write_text(json.dumps({"result": line}))
print(line)
