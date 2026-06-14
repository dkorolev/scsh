# Agent skills (canonical)

This directory is the **single source of truth** for repo agent skills. Each skill is a folder with `SKILL.md` (YAML frontmatter + markdown body) and optional `scripts/`, `references/`, and `assets/`.

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
| [scsh-harness-demo-and-selftest](scsh-harness-demo-and-selftest/SKILL.md) | Follow `DEMO.md` to bootstrap a tiny `scsh` demo repo and run it, reporting PASS/FAIL — scsh's bundled demo + self-test (installed by a no-URL `scsh installskills`) |
| [add](add/SKILL.md) | Sum of env vars `A`+`B` (defaults `2`,`3`); reports `A + B = sum` |
| [multiply](multiply/SKILL.md) | Product of `X`·`Y` with **no defaults** — errors if either `X` or `Y` is unset |

## Adding a skill

1. Create `.skills/<skill-name>/SKILL.md` with `name` and `description` frontmatter (name must match the folder).
2. Author only here — never in the symlinked host paths (`.claude/skills/`, `.cursor/skills/`, …); see [`CONTRIBUTING.md`](../CONTRIBUTING.md) for the layout and house rules.
3. Invoke via your host (`/skill-name`, `$skill-name`, or natural-language trigger per `description`).
