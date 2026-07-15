# Beautiful loop demo

This is a code-first demonstration of a realistic pull-request cycle. It starts with a deliberately small, imperfect word-counting command, asks Cursor Auto to implement a change that requires rewriting part of it, prepares the change as a local PR, and then reviews it from several independent points of view.

Create a fresh demo repository and scaffold its deliberately imperfect starting point:

```sh
DEMO="tmp/demo-beautiful-$(date -u +%Y%m%d-%H%M%S)-utc"
mkdir "$DEMO"
cd "$DEMO"
git init -q .
git config user.name "scsh beautiful demo"
git config user.email demo@example.com
scsh init-beautiful-demo
python3 -m unittest -v
```

The scaffold contains only a dependency-free `wordstats.py`, one focused baseline test,
`README.md`, and short repository guidance. It has no incident report, generated review result,
or prewritten PR description. The command commits the starting point and gitignores `tmp/`, so
the repository is immediately ready for a real workflow run.

Then run the built-in workflow:

```sh
scsh run --def demo-beautiful-loop
```

The same workflow is also available in the local session browser as `demo-beautiful-loop`.

## What happens

1. Cursor Auto changes the program from counting individual words to counting adjacent word groups. The request adds `--group-size` and `--limit`, deterministic ordering, input validation, tests, and documentation, so the code stays small but the change is not trivial.
2. Cursor Auto writes and commits `PR-DESCRIPTION.md`. The whole-job Packdiff page lifts this file into its Description panel, so the user sees the local branch as the PR it is intended to become.
3. Five reviewer specialties examine the change: conventions, justification, reviewability, sanity, and testing. Each specialty runs independently on Claude Opus 4.8, Codex Terra, and Cursor Auto, for 15 review cycles in total.
4. The first review batch runs before the loop. The loop begins with the decision step: if the batch already meets the approval bar, it breaks immediately; otherwise Cursor applies the combined feedback and all 15 reviewers run again.
5. The loop stops only when every available review succeeds, every grade is `good` or `excellent`, `excellent` outnumbers `good`, and the mean score is at least 4.5. The job diagram shows completed iterations and makes it clear that more iterations were possible.

The demo does not push a branch or open a GitHub pull request. It produces the commits, the PR description, and the local Packdiff review page; `send-beautiful-pr` is the separate shipping step that requires the user's authorization.

## The built-in beautiful family

- `big-beautiful-build` builds a complete feature after one intake question.
- `fast-beautiful-forward` rebases the local work onto the freshest upstream default branch.
- `code-beautiful-review` runs the five-specialty, 15-route reviewer fleet and clusters its findings.
- `the-beautiful-loop` applies important review fixes and repeats preparation and review until the strict bar passes.
- `prepare-beautiful-pr` shapes the commits and writes the local PR description.
- `send-beautiful-pr` performs the explicitly authorized push and opens the GitHub PR.

A plain `scsh installskills` installs this family, the five reviewer skills, and the matching `.scsh.yml` profiles into a clean consumer repository.

## Attribution

The beautiful delivery skills are adapted from [`dkorolev/beautiful-skills`](https://github.com/dkorolev/beautiful-skills), and the five reviewer specialties are adapted from [`dkorolev/code-review-skills`](https://github.com/dkorolev/code-review-skills). During this integration, the copies under `scsh/.skills/` are temporarily the source of truth; follow-up PRs will reconcile both upstream skill repositories after the `scsh` PR merges. The implementation and this document follow [`dkorolev/principles`](https://github.com/dkorolev/principles), including stable UI identity, explicit resource cleanup, focused commits, and Markdown that keeps one logical thought per line.
