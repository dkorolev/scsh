# Build a Complete Feature from the Web UI

`big-beautiful-build` turns one feature brief into a committed, runnable implementation without asking follow-up questions. Cursor Auto writes the code, documentation, demo, and automated verification, runs the demo and verification, records every assumption, and commits the result.

## What the User Does

1. Open the local `scsh` session browser and select **Run**.
2. Open a clean, committed repository by path, or enter a project name and select **New project**.
3. Select **big-beautiful-build** from Definitions.
4. In **FEATURE**, describe the complete feature, including behavior, inputs and outputs, boundaries, acceptance criteria, preferred technology, and hard constraints.
5. Select **Start job** once. This submission is the skill's only intake answer.
6. Follow the job page while Cursor builds and verifies the feature.
7. When the job succeeds, inspect the structured result and committed changes through **commits diff**. The complete report is preserved at `tmp/scsh/<job>/big-beautiful-build.md` in the repository.

The browser rejects an empty feature brief. The repository must be clean because `scsh` runs against a committed clone and integrates only the commits created by the job.

## What `scsh` Does

The browser launches a built-in one-step workflow. Its typed multiline `FEATURE` parameter is forwarded intact, including newlines, to Cursor Auto. The step resolves the canonical embedded `.skills/big-beautiful-build/SKILL.md` rather than maintaining a copied prompt, so `scsh installskills` and the Web UI execute the same delivery contract.

The workflow requires a strict JSON completion result with the summary, changed files, commands, verification, and assumptions. It also requires `big-beautiful-build.md` as a preserved job artifact and enables commit integration, so success means the job has both a complete report and authored commits.

## Assumptions

- Assumed: existing repositories already contain any project-specific agent instructions that Cursor must follow.
- Assumed: a newly created project intentionally begins with only Git metadata and a gitignored `tmp/`; the feature brief tells Cursor what product to create.
- Assumed: Cursor is installed, authenticated, and its image is current on the Setup page before a real build begins.
- Assumed: building and committing locally is authorized by starting the job; pushing, publishing, opening a pull request, spending money, or destructive external actions remain outside the workflow.

## Expected Outcome

A successful job has one green Cursor `build` step, at least one integrated commit for the delivered feature, a commits diff, a valid structured result, and the preserved `big-beautiful-build.md` report. The resulting repository's own demo and automated verification pass using the commands recorded in that report.
