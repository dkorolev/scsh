# Agent skills (canonical)

This directory is the **single source of truth** for repo agent skills. Each skill is a folder with `SKILL.md` (YAML frontmatter + markdown body) and optional `scripts/`, `references/`, and `assets/`.

For the beautiful family and the five code-review specialties, these copies are temporarily canonical while the integrated `scsh` workflow lands. Follow-up PRs will reconcile [`dkorolev/beautiful-skills`](https://github.com/dkorolev/beautiful-skills) and [`dkorolev/code-review-skills`](https://github.com/dkorolev/code-review-skills) after this branch merges.

Edit skills here — not in the tool-specific paths below (they are symlinks).

## Tool discovery paths

| Tool | Project path | Notes |
| --- | --- | --- |
| **Canonical** | `.skills/<name>/` | Author here |
| Cursor | `.cursor/skills/` → `.skills` | Also `~/.cursor/skills/` for personal skills |
| Claude Code | `.claude/skills/` → `.skills` | Also `~/.claude/skills/` |
| Codex | `.agents/skills/`, `.codex/skills/` → `.skills` | Repo; also `~/.agents/skills/`, `~/.codex/skills/` |
| OpenCode | `.opencode/skills/` → `.skills` | Also reads `.claude/skills`, `.agents/skills` |

All symlinks point at this directory so one edit updates every host.

## Skills in this repo

| Skill | Purpose |
| --- | --- |
| [big-beautiful-build](big-beautiful-build/SKILL.md) | Build a complete feature after one intake question |
| [fast-beautiful-forward](fast-beautiful-forward/SKILL.md) | Replay local work onto the freshest upstream default branch |
| [code-beautiful-review](code-beautiful-review/SKILL.md) | Run and summarize the 15-route code-review fleet |
| [the-beautiful-loop](the-beautiful-loop/SKILL.md) | Fix review findings and repeat preparation and review until the strict bar passes |
| [prepare-beautiful-pr](prepare-beautiful-pr/SKILL.md) | Shape commits and write the local PR description |
| [send-beautiful-pr](send-beautiful-pr/SKILL.md) | Audit authorship, push once, and open the GitHub PR |
| [conventions-reviewer](conventions-reviewer/SKILL.md) | Enforce the repository's own conventions |
| [justification-reviewer](justification-reviewer/SKILL.md) | Challenge scope, necessity, and complexity |
| [reviewability-reviewer](reviewability-reviewer/SKILL.md) | Review commit and PR presentation for humans |
| [sanity-reviewer](sanity-reviewer/SKILL.md) | Catch obvious security, performance, and resource-leak problems |
| [testing-reviewer](testing-reviewer/SKILL.md) | Check that changed behavior is verifiable and test tooling cleans up |
| [scsh-harness-demo-and-selftest](scsh-harness-demo-and-selftest/SKILL.md) | Follow `DEMO.md` to bootstrap a tiny `scsh` demo repo and run it, reporting PASS/FAIL |
| [harness-smoke](harness-smoke/SKILL.md) | Minimal JSON OK smoke test for **grok** and **cursor** harnesses — run via [`HARNESS-SMOKE.md`](../HARNESS-SMOKE.md) or `./scripts/harness-smoke.sh` |
| [add](add/SKILL.md) | Sum of env vars `A`+`B` (defaults `2`,`3`); reports `A + B = sum` |
| [subtract](subtract/SKILL.md) | Difference `C`−`D` (defaults `10`,`4`); commit-enabled companion to `add` |
| [multiply](multiply/SKILL.md) | Product of `X`·`Y` with **no defaults** — errors if either `X` or `Y` is unset |
| [demo-pr](demo-pr/SKILL.md) | Minimal fake PR: write `demo_pr_note.txt` + `PR-DESCRIPTION.md`, two commits (packdiff Description panel) |

The root `.scsh.yml` is the source of truth for the beautiful skills' profiles and the 15 reviewer routes. The `add` and `multiply` reference examples are scaffolded by `scsh init-demo-project`. See [`DEMO.md`](../DEMO.md).

## Adding a skill

1. Create `.skills/<skill-name>/SKILL.md` with `name` and `description` frontmatter (name must match the folder).
2. Author only here — never in the symlinked host paths (`.claude/skills/`, `.cursor/skills/`, …); see [`CONTRIBUTING.md`](../CONTRIBUTING.md) for the layout and house rules.
3. Invoke via your host (`/skill-name`, `$skill-name`, or natural-language trigger per `description`).
