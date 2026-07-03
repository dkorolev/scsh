---
name: harness-smoke
description: "Minimal headless harness smoke test — write a tiny JSON OK result to the declared path and stop. Used by HARNESS-SMOKE.md and scripts/harness-smoke.sh to verify claude, codex, and cursor container harnesses end to end. Use when the user asks to smoke-test claude, codex, or cursor harnesses."
---

# harness-smoke

This is a **smoke test**, not a product feature. Your only job is to prove the harness can read this skill and write the result file.

## Steps

1. Write **exactly** this JSON to `$SCSH_RESULT` (the path is also named in the run prompt, e.g. `tmp/harness-smoke-codex-gpt-5.5.json`):

```json
{
  "result": {
    "status": "OK",
    "skill": "harness-smoke"
  }
}
```

Use a real JSON file on disk — do not only print the JSON to stdout.

2. **Stop.** Do not git fetch, pull, push, or clone. Do not run builds, tests, or linters. Do not commit. Do not edit any file except the result path above.

## Pass criteria

The run succeeds when the result file exists with `"status": "OK"`. Anything else is a failure.
