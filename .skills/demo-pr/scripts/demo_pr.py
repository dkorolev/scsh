#!/usr/bin/env python3
"""The demo-pr skill worker: write a tiny feature note + PR-DESCRIPTION.md, commit each,
then write the JSON result under tmp/.

This is the minimal "fake PR" deliverable for packdiff: the notes-author commit of
PR-DESCRIPTION.md lifts into the job page's Description panel; the feature file is the
diff body. No arithmetic, no tests — just two files and two commits.
"""
import json
import os
import pathlib
import subprocess
import sys

# <repo>/.skills/demo-pr/scripts/demo_pr.py → repo root is three parents up.
ROOT = pathlib.Path(__file__).resolve().parents[3]

title = os.environ.get("TITLE", "").strip() or "Hello from demo-pr"
feature = (
    f"# {title}\n\n"
    "A one-line feature stub committed by the `demo-pr` skill so the job page has a\n"
    "real file diff to show next to `PR-DESCRIPTION.md`.\n"
)
description = (
    f"# {title}\n\n"
    "## Summary\n"
    "- Add `demo_pr_note.txt` — a tiny tracked feature stub.\n"
    "- Add `PR-DESCRIPTION.md` — this pull-request description (packdiff lifts it into\n"
    "  the review page's Description panel via the notes-author convention).\n"
)

(ROOT / "demo_pr_note.txt").write_text(feature, encoding="utf-8")
(ROOT / "PR-DESCRIPTION.md").write_text(description, encoding="utf-8")

try:
    subprocess.run(["git", "add", "demo_pr_note.txt"], cwd=ROOT, check=True)
    subprocess.run(
        ["git", "commit", "-m", f"demo-pr: add feature note ({title})"],
        cwd=ROOT,
        check=True,
    )
except (OSError, subprocess.CalledProcessError) as e:
    print(f"demo_pr.py: could not commit demo_pr_note.txt ({e})", file=sys.stderr)

try:
    subprocess.run(["git", "add", "PR-DESCRIPTION.md"], cwd=ROOT, check=True)
    subprocess.run(
        ["git", "commit", "-m", "demo-pr: add PR-DESCRIPTION.md"],
        cwd=ROOT,
        check=True,
    )
except (OSError, subprocess.CalledProcessError) as e:
    print(f"demo_pr.py: could not commit PR-DESCRIPTION.md ({e})", file=sys.stderr)

(ROOT / "tmp").mkdir(exist_ok=True)
result_rel = os.environ.get("SCSH_RESULT", "tmp/demo_pr_result.json")
result_path = ROOT / result_rel
result_path.parent.mkdir(parents=True, exist_ok=True)
payload = {
    "ok": True,
    "title": title,
    "files": ["demo_pr_note.txt", "PR-DESCRIPTION.md"],
}
result_path.write_text(json.dumps(payload))
print(f"demo-pr: committed {title!r} + PR-DESCRIPTION.md")
