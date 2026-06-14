# DEMO.md — see `scsh` in action from an empty directory

This is a guided, English walkthrough that **builds a tiny `scsh` project from scratch and runs it**, so you get the look and feel of the tool in a couple of minutes. It is written to be **handed to an AI agent that supports skills**: start your favorite agent **in any empty directory that is not yet a git repository**, and ask it to *follow the steps in this file*. It will create a small demo repo right there and run it — you'll watch the **`add`** skill compute a sum and the **`multiply`** skill insist on its inputs.

> **The one-line pitch:** clone `scsh`, build it, then from an empty directory tell your agent: *"follow the steps in DEMO.md."* That's the whole demo.

> **If you are the agent following this:** run each command, in order, **exactly as written** — every step is a single copy-pasteable command. Don't substitute your own directory names, wrap the demo in a fixed parent dir, or `rm`/delete anything to "start from a clean slate": the steps already choose safe, unique, re-runnable paths (see step 0.2), so reach for `rm` only in the optional step 9. You should **never need to write a script of your own** to follow this demo; if you feel the urge to, something is off — stop and ask the user first. (Skills ship their *own* scripts; *following this walkthrough* needs none.)

## What you'll see

- `scsh init-demo-project` scaffolds a two-skill project and **commits** it.

- **`add`** (A + B) runs by default — with the built-in defaults, and with values you pass — and `scsh` prints the result it computed.

- **`add` is commit-enabled**: it records each sum as a git commit, and `scsh` **rebases that commit onto your branch**. The commit is journaled in the cache too, so even a **cached** re-run replays it — the commit side effect is never lost to a cache hit.

- **`multiply`** (X · Y) lives in the **`multiply` profile** and **requires** X and Y: provide them and it works; omit them and **`scsh` itself refuses it**, before any container starts.

- **Results are cached.** Run a skill again at the same repo content + env and `scsh` returns the cached result **instantly**, printing `(cached)` — no container, no model.

## Where this runs

- **The normal way: from any directory that is NOT inside a git repository** (an empty scratch dir is perfect). The demo repo is created there, in a UTC-stamped subdir, and left for you to inspect.

- **If you happen to be inside the `scsh` repo**, don't pollute it — put the demo under the gitignored `tmp/` instead. (Step 0.2 below detects this and does it for you.)

The happy path (`add`, and `multiply` with values) does a **real run**: it builds a small container image and calls a model, so it needs a container runtime (Docker/Podman/Apple `container`) and opencode configured with a model. The instructive failure (`multiply` without X/Y) is decided by `scsh` itself and needs neither — so even with no runtime you still see the scaffold, the plan, and the refusal.

---

## 0. Set up

**0.1 — Check everything is in place first.** Run this and read the four lines — they decide which steps fully run:

```sh
command -v git  >/dev/null && echo "git: ok"  || echo "git: MISSING (required)"
command -v scsh >/dev/null && echo "scsh: on PATH" || echo "scsh: build it (cargo build --release), or put it on PATH"
{ command -v docker || command -v podman || command -v container; } >/dev/null 2>&1 \
  && echo "container runtime: ok" || echo "container runtime: none — real runs will be skipped"
test -f "${XDG_DATA_HOME:-$HOME/.local/share}/opencode/auth.json" \
  && echo "opencode model auth: ok" || echo "opencode model auth: none — skills can't reach a model"
```

**git** and **`scsh`** (on `PATH`, or buildable with a Rust toolchain) are required for *every* step. A **container runtime** *and* **opencode model auth** are needed for the steps that actually compute (2–4, and the cache *miss* in 7). If either is missing, you still get the network-free parts — scaffold, `scsh list`, the `multiply` refusal, and a cache *hit* — so keep going; each step notes when it needs a model.

**0.2 — Pick where the demo repo goes**, UTC-stamped (`demo-YYYYMMDD-HHMMSS-utc`). If you are inside the `scsh` repo, put it under the gitignored `tmp/`; otherwise create it right here:

```sh
STAMP="demo-$(date -u +%Y%m%d-%H%M%S)-utc"
if grep -qs '^name = "scsh"' Cargo.toml; then mkdir -p tmp; DEMO="tmp/$STAMP"; else DEMO="$STAMP"; fi
mkdir "$DEMO"
```

> The UTC stamp makes this directory **unique to this run**, which is the whole point: the demo is meant to be run **as many times as you like**, each run creating its own fresh `demo-…-utc` dir alongside any earlier ones. So just `mkdir` the stamped dir and work in it — **do not** hard-code a fixed directory name, reuse a previous run's dir, or `rm -rf` anything first to "reset". There is nothing to clean up before a run; optional teardown is step 9.

**0.3 — Get an `scsh` binary.** Use it from your `PATH`, or build it once from your `scsh` checkout. Set `$SCSH` to whatever you'll invoke:

```sh
if command -v scsh >/dev/null 2>&1; then
  SCSH=scsh
else
  ( cd /path/to/your/scsh/checkout && cargo build --release )   # one-time build
  SCSH=/path/to/your/scsh/checkout/target/release/scsh
fi
```

> No binary on `PATH`? You may substitute every `$SCSH` below with `cargo run --release --quiet --` run from your `scsh` checkout — but a built binary is the smoother experience.

**0.4 — Enter the demo repo and make it a git repo.** A real run clones *committed* state, so it must be a clean git repo:

```sh
cd "$DEMO"
git init -q .
git config user.email demo@example.com
git config user.name  "scsh demo"
```

---

## 1. Scaffold the demo project

```sh
$SCSH init-demo-project
BASE=$(git rev-parse HEAD)   # remember the scaffold commit — the cache demo (step 7) returns here
```

**Confirm** the output reports `✓ committed the scaffold` and prints usage examples for the two skills. It has written and committed: a `.scsh.yml` with **`add`** (runs by default) and **`multiply`** (in the `multiply` profile); `.skills/add/SKILL.md` and `.skills/multiply/SKILL.md`; the harness discovery symlinks (`.claude/skills`, …); and a `/tmp` gitignore. The working tree is clean and ready to run.

`$BASE` is the **scaffold commit** — the repo state the first `add` run (step 2) caches its result against. We return to it in step 7 to get a cache hit; capturing it now (not later) matters, because each `add` run commits and moves `HEAD`.

---

## 2. Run `add` — works with defaults

```sh
$SCSH run
```

**Confirm** `scsh` builds the image once, then runs **only `add`** (multiply is profiled). `add` reads the defaults `scsh` injected (`A=2`, `B=3`), computes the sum, and `scsh` prints the message the skill produced — ending in **`2 + 3 = 5`**:

```
✓ opencode: add   …s   2 + 3 = 5
✓ add: brought in 1 commit (rebased onto main)
✓ all 1 skill completed successfully
```

The result file is collected to `tmp/add_result.json`, **and** — because `add` is `commits: true` — `scsh` brought back the commit the skill made and rebased it onto your branch. Confirm it landed:

```sh
git log --oneline -2          # top commit: "add: 2 + 3 = 5"
cat add_log.txt               # one line: 2 + 3 = 5
git status --porcelain        # empty — the tree is clean again
```

---

## 3. Run `add` with your own values

```sh
A=10 B=20 $SCSH run
```

**Confirm** `add` now reports **`10 + 20 = 30`** — `scsh` forwarded your `A` and `B` into the container (host values win over the `${A:-2}` / `${B:-3}` defaults).

---

## 4. Run `multiply` (the profile) with its required inputs

```sh
X=6 Y=7 $SCSH run --profile multiply
```

**Confirm** this runs **only** `multiply` (not the default `add`) — `--profile multiply` selects *that* profile — and it reports **`6 * 7 = 42`**. `X` and `Y` are required, and you provided them. (To run the default `add` *and* `multiply` together, use `--profile default,multiply`.)

---

## 5. Run `multiply` **without** its inputs — `scsh` refuses it

```sh
$SCSH run --profile multiply
```

**Confirm** `multiply` is **refused by `scsh` itself**, before its container ever starts (only `multiply` is selected by this profile):

```
✗ multiply: Environmental variable X is not provided, use the ${X:-} syntax to allow for empty values as defaults
✗ 1 of 1 skill failed
```

That refusal is enforced at the **`scsh` level** because `multiply`'s `env:` declares `X: ${X}` and `Y: ${Y}` (required, no default). You can see *what* a skill requires without building anything — `scsh list` is network-free:

```sh
$SCSH list             # multiply's line ends with `· env: X, Y` — the variables it requires
$SCSH list --verbose   # the same, plus the image Dockerfile and the exact build/run commands
```

`list` reports the variables each skill declares (here, `X` and `Y`), not a verdict against your current environment — so the actual refusal is the run-time check you just saw, decided before any container starts.

---

## 6. Commits come back — and stack up (the important part)

`add` is marked **`commits: true`** in `.scsh.yml`. Each run, the skill appends its sum to `add_log.txt` and commits inside its own clone; after the run, `scsh` **rebases that commit onto your current branch**. By now you've already run `add` three times (steps 2, 3, 4), so your branch already carries three `add: …` commits:

```sh
git log --oneline             # three "add: …" commits on top of the scaffold
cat add_log.txt               # 2 + 3 = 5 / 10 + 20 = 30 / 2 + 3 = 5
git log -1 --format='%an <%ae>'   # author: dkorolev-neon-elon-bot <dmitry.korolev+elon-presley@gmail.com>
```

**Notice the author.** `scsh` stamps these commits with a deliberately unmistakable bot — `dkorolev-neon-elon-bot` (a neon-cyberpunk Elon) — never a real contributor. They're **local-only by design** (`scsh` rebases, never pushes), so if that face ever shows up in a code review or a pushed commit list, you'll know instantly you pushed something you shouldn't have. (See `scsh help cache`.)

**Run it once more to see commits are a side effect, not a cached no-op:**

```sh
$SCSH run                     # add again → "✓ add: brought in 1 commit (rebased onto main)"
git log --oneline | head -1   # a NEW "add: 2 + 3 = 5" commit, even though the inputs repeat
```

Running again **adds another commit** — because the repo changed (a new `add_log.txt` line was committed), the next run sees a different state, so it's a fresh run. (Reset to the *same* state and re-run and you'll get a cache **hit** — instant, no model — that **still replays the commit**, so a cached re-run reproduces the side effect too. That's §7.)

**Why a rebase, not a fast-forward?** Each skill commits on a clone taken from your branch *before* the run, so several commit-enabled skills would all branch from the same point. `scsh` replays each skill's commits onto your branch in turn (the second skill rebases onto the branch the first one advanced), so order doesn't matter.

**If a skill's commits can't apply cleanly**, `scsh` doesn't touch your branch — it saves them to a distinct branch named `scsh/incoming/<skill>-<UTC>-<short-hash>` and tells you:

```
⚠ add: 1 commit didn't rebase cleanly — saved to branch scsh/incoming/add-20260615-040000-utc-1a2b3c4 (inspect, then merge/cherry-pick)
```

You can then `git log scsh/incoming/…` to see exactly what it added and merge or cherry-pick it yourself. (In this demo the commits always apply cleanly; this is what you'd see if you'd edited `add_log.txt` in a conflicting way between runs.)

---

## 7. Results are cached (run it again — instantly)

`scsh` caches each skill's result, keyed on a SHA-256 of **the repo's committed content + the skill's files + the resolved env**. If all three match a previous run, `scsh` returns the cached result instantly — no clone, no container, no model call — and prints **`(cached)`**.

`add` already ran (and cached its result) in step 2 — but it also *committed*, which moved `HEAD`, so the repo is no longer at that input state. **Return to `$BASE`** — the scaffold commit you captured in step 1, the exact state step 2 cached against — then run again:

```sh
git reset --hard "$BASE"      # back to the exact state add was first run from (step 1's commit)
$SCSH run                     # SAME content + skill + env (defaults A=2 B=3) as step 2
```

> Capture `$BASE` at step 1, **not here** — by now `HEAD` has moved forward with each `add` commit, so `BASE=$(git rev-parse HEAD)` at this point would reset to *nowhere* (a no-op) and you'd get a cache miss. It must be the scaffold commit.

**Confirm** the `add` line ends with **`(cached)`** and finishes in ~0s (no clone, no container, no model) — **and** that `scsh` still **replays the journaled commit**, so `git log` shows a fresh `add: 2 + 3 = 5` on top:

```
✓ opencode: add  0.0s  2 + 3 = 5  (cached)
✓ add: brought in 1 commit (rebased onto main)
```

The cache records **both** halves of a run — the result file *and* the commits the skill made — so a hit reproduces the commit side effect, not just the result. (`git log` before vs. after this cached run makes the replayed commit obvious.)

**The env is part of the key.** From the same state, change the inputs, then come back:

```sh
git reset --hard "$BASE"
A=5 B=7 $SCSH run             # different env → cache MISS → really runs → 5 + 7 = 12
git reset --hard "$BASE"
A=2 B=3 $SCSH run             # back to the first env → cache HIT → 2 + 3 = 5  (cached)
```

**Confirm** the `A=5 B=7` run is *not* cached (it computes `5 + 7 = 12`), and the final `A=2 B=3` run *is* `(cached)`. The cache lives in the repo's gitignored `tmp/.sccache/<sha256>.json`; `git reset` never touches it (it's gitignored), which is why the hit survives the resets.

> Because `add` commits, a plain re-run (without the `git reset`) would be a cache *miss* — the commit changed the repo, so the key changed. That's intended: re-running from a new state re-does the work. See `scsh help cache`.

---

## 8. Install more skills from another repo (the manifest merge) — optional, needs network

`scsh` can pull skills from any git repo that ships them. When that repo has its **own** `.scsh.yml`, the manifest drives the install: `scsh` validates it, installs each skill it lists — **except** the authoring-only ones (marked `autoinstall: false`, or named with the `internal-` prefix) — and **merges those skills' entries into your `.scsh.yml`**, so they are runnable immediately.

```sh
$SCSH installskills https://github.com/dimacurrentai/code-review-skills
```

You'll see `scsh` install the reviewer skills and add them to your `.scsh.yml` under `profile: code-review`, while **skipping** the authoring-only `internal-self-check-reviewers` (its `internal-` name marks it internal to that repo):

```
✓ from …/code-review-skills: 5 skills — conventions-reviewer, justification-reviewer, …
✓ skipped 1 authoring-only (autoinstall: false or internal-*): internal-self-check-reviewers
✓ added 5 skills to .scsh.yml: conventions-reviewer, …
```

Your existing `add`/`multiply` entries are left untouched — the merge is **append-only**, and re-running `installskills` is idempotent (already-present skills are reported, never duplicated). The new skills are now first-class; commit them, then a real run needs a clean tree:

```sh
git add -A && git commit -m "install code-review skills"
$SCSH run --profile code-review     # runs the five reviewers against origin/main..HEAD
```

Skills the manifest doesn't list are skipped (the manifest is the shipping list); `scsh updateskills <url>` overwrites the skill *files* with the source's version. See `scsh help`.

---

## 9. Clean up (optional)

The demo repo is yours to poke at. When you're done:

```sh
cd ..
rm -rf "$STAMP"          # inside the scsh repo it was created as tmp/$STAMP — remove that
```

---

## What this demonstrates

- A directory goes from **empty → a working, committed `scsh` project** with a single command (`init-demo-project`).

- `scsh` **forwards and injects** environment variables for a skill (`add`: built-in defaults, or your values) and **prints the result** each skill computed — not just a file path.

- **Profiles** keep a skill that needs inputs (`multiply`) out of the default run, and **`scsh` enforces required variables itself**: a clear, early failure at the `scsh` level instead of a confusing one deep inside a container.

- **Commit-enabled skills contribute back**: `scsh` rebases a skill's commits onto your branch (or saves them to a `scsh/incoming/…` branch if they don't apply cleanly), and treats adding a commit as a real, repeatable side effect — run twice, two commits.

- **Results are content-addressed and cached**: same repo content + skill + env returns the cached result instantly (`(cached)`); change any of them and it re-runs. The cache lives in `tmp/.sccache/`.

- **Skills install from other repos, manifest-aware**: a source repo's own `.scsh.yml` decides what ships — `scsh installskills` validates it, merges its skills into yours (append-only), and keeps authoring-only skills out of consumers (marked `autoinstall: false`, or named `internal-*`, like `internal-self-check-reviewers`).

> Don't have a container runtime or a configured model? You'll still see step 1 scaffold and commit, `scsh list` show the skills, step 5's refusal, and a **cache hit** in step 7 (a hit needs no container) — all with no network. Only the actual sums, products, and commit-backs in steps 2–4 and 6 need the runtime and a model.

### env syntax, for reference

`scsh` resolves each skill's `env:` values with a small, shell-like syntax — that's why `add` can default and `multiply` can require:

| You write | Meaning |
| --- | --- |
| `X: ${X}` or `X: $X` | **Require** host `X`; refuse the skill if it is unset (this is `multiply`). |
| `A: ${A:-2}` | Forward host `A`, or inject `2` when unset (this is `add`; `${A:-}` = empty). |
| `A: ${A:?message}` | **Require** host `A`; refuse with your `message` if unset. |
| `A: A` | A **literal** — sets `A` to the string `"A"`, not a variable. |
