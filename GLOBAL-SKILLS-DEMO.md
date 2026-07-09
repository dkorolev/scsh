# GLOBAL-SKILLS-DEMO.md — run a skill the target repo does not contain

This walkthrough demos **global skill delivery**: an external bundle's skill is installed
**globally inside the container** — for `claude` and `cursor` into the CLI's own user-level
skills directory, where the agent discovers it natively and can resolve it as `/<name>` —
while the **target repository never contains the skill**: no `.scsh.yml`, no `.skills/`,
nothing tracked, `git status` clean before and after.

Like [`DEMO.md`](DEMO.md), this file is written to be **handed to an AI agent**: start your
agent in any directory that is **not** inside a git repository and ask it to *follow the
steps in GLOBAL-SKILLS-DEMO.md*.

> **If you are the agent following this:** run each command, in order, exactly as written.
> Every step is a single copy-pasteable command; you should never need to write a script of
> your own. Report each **Expect:** line as PASS or FAIL, and give a final PASS/FAIL verdict.

## What this proves

1. `scsh run --override-dot-scsh-yml <bundle>/.scsh.yml` runs the bundle's skills against a
   repo that ships neither `.scsh.yml` nor `.skills/`.
2. The skill reaches the agent as a **global, in-container install** — under
   `$CLAUDE_CONFIG_DIR/skills/` for claude (natively discovered, name-resolvable) — never as
   a file in the checkout.
3. The target repo stays byte-for-byte clean: results land only under the gitignored `tmp/`.

## 1. Probe

```sh
[ -f ~/.zshrc ] && . ~/.zshrc 2>/dev/null || true
command -v git  >/dev/null && echo "git: ok"  || echo "git: MISSING (required)"
command -v scsh >/dev/null && echo "scsh: on PATH" || echo "scsh: MISSING (build with cargo build --release)"
```

**Expect:** both `ok`. The run itself probes the agent routes: routes whose harness or
credentials are unavailable are **skipped** (the run fails only when every route is skipped)
— so this demo needs at least ONE of `claude` / `cursor` logged in on the host.

## 2. Work area

```sh
DEMO_ROOT="$(pwd)/scsh-global-skill-demo-$(date -u +%Y%m%d-%H%M%S)" && mkdir -p "$DEMO_ROOT" && echo "$DEMO_ROOT"
```

(If you are inside the `scsh` repo itself, prefix the path with `tmp/` so nothing pollutes
the checkout: `DEMO_ROOT="$(pwd)/tmp/scsh-global-skill-demo-…"`.)

## 3. The target repo — deliberately skill-free

```sh
mkdir -p "$DEMO_ROOT/repo" && cd "$DEMO_ROOT/repo" && git init -q && printf '/tmp\n' > .gitignore && printf '# A repo with no scsh setup at all.\n' > README.md && git add -A && git commit -qm "Init." && ls -a && git status --porcelain && echo "repo: clean"
```

**Expect:** the listing shows NO `.scsh.yml` and NO `.skills/`; `repo: clean` prints.

## 4. The external bundle — config + the skill, outside the repo

```sh
mkdir -p "$DEMO_ROOT/bundle/.skills/greet" && cat > "$DEMO_ROOT/bundle/.scsh.yml" <<'YML'
skills:
  greet:
    timeout: 600
    result: tmp/greet_{name}.json
    invocations:
      claude-sonnet:
        harness: claude
        model: sonnet
      cursor-composer-fast:
        harness: cursor
        model: composer-2.5-fast
YML
cat > "$DEMO_ROOT/bundle/.skills/greet/SKILL.md" <<'MD'
# greet

You are running as a GLOBALLY INSTALLED skill: this SKILL.md is not part of the repository
you are working in. Write the JSON object
{"ok": true, "greeting": "hello from a globally installed skill"}
to the file named by the SCSH_RESULT environment variable, then stop. Do nothing else —
no commits, no network, no other files.
MD
echo "bundle: ready"
```

## 5. Sanity: scsh sees the bundle, the repo stays untouched

```sh
cd "$DEMO_ROOT/repo" && scsh check-profile default --override-dot-scsh-yml "$DEMO_ROOT/bundle/.scsh.yml"
```

**Expect:** exit 0 and a ✓ naming the `default` profile — resolved from the bundle, since
the repo has no `.scsh.yml` of its own.

## 6. Run it

```sh
cd "$DEMO_ROOT/repo" && scsh run --override-dot-scsh-yml "$DEMO_ROOT/bundle/.scsh.yml"
```

**Expect:** the preflight line ends with `· override …/bundle/.scsh.yml`; unavailable routes
are skipped with a warning; every route that runs finishes ✓. (First run on a fresh machine
also builds the harness image.)

## 7. Verify — results in, skill never in the checkout

```sh
cd "$DEMO_ROOT/repo" && ls tmp/greet_*.json && cat tmp/greet_*.json && [ ! -e .skills ] && echo "no .skills in the repo: PASS" && [ -z "$(git status --porcelain)" ] && echo "repo still clean: PASS"
```

**Expect:** one `tmp/greet_<route>.json` per route that ran, each containing `"ok": true`
and the greeting; `no .skills in the repo: PASS`; `repo still clean: PASS`.

## 8. (Optional) See the global install with your own eyes

Re-run step 6 with `SCSH_KEEP_RUNS=1`, then inspect the kept run clone under `/tmp`:

```sh
SCSH_KEEP_RUNS=1 scsh run --override-dot-scsh-yml "$DEMO_ROOT/bundle/.scsh.yml"
ls /tmp/scsh-*-run-*/tmp/.claude-auth/.claude/skills/greet/ 2>/dev/null || ls /tmp/scsh-*-run-*/tmp/.cursor/skills/greet/ 2>/dev/null
```

**Expect:** `SKILL.md` — sitting in the harness's **user-level skills directory** (what the
container sees as `$CLAUDE_CONFIG_DIR/skills/…` / `$CURSOR_CONFIG_DIR/skills/…`), not in the
repository. Clean up the kept clones afterwards: `rm -rf /tmp/scsh-*-run-*`.

## 9. Cleanup

```sh
rm -rf "$DEMO_ROOT" && echo "demo removed"
```

**Final verdict:** PASS only if steps 5–7 all matched their **Expect:** lines.
