#!/usr/bin/env python3
"""Gate the identities and contents of commits entering a pull request."""

from __future__ import annotations

import dataclasses
import os
import pathlib
import re
import subprocess
import sys

CANONICAL_EMAIL = "dmitry.korolev@gmail.com"
CANONICAL_EMAIL_EXAMPLE = "dmitry.korolev+something@gmail.com"
ELON_NAME = "dkorolev-neon-elon-bot"
ELON_EMAIL = "dmitry.korolev+elon-presley@gmail.com"
PR_DESCRIPTION = "pr-description.md"


@dataclasses.dataclass(frozen=True)
class Identity:
  """One author or committer identity stored in a Git commit."""

  name: str
  email: str


@dataclasses.dataclass(frozen=True)
class Commit:
  """The identity metadata needed to check one pull-request commit."""

  sha: str
  author: Identity
  committer: Identity


def git_bytes(*args: str) -> bytes:
  return subprocess.check_output(("git",) + args)


def git_text(*args: str) -> str:
  return git_bytes(*args).decode("utf-8", errors="replace")


def commits_in_range(base: str, head: str) -> list[str]:
  return git_text("rev-list", "--reverse", f"{base}..{head}").splitlines()


def read_commit(sha: str) -> Commit:
  raw = git_bytes("show", "-s", "--format=%H%x00%an%x00%ae%x00%cn%x00%ce%x00", sha)
  fields = raw.split(b"\0")
  if len(fields) < 6:
    raise SystemExit(f"could not read identity metadata for commit {sha}")
  commit_sha, author_name, author_email, committer_name, committer_email = (
    field.decode("utf-8", errors="replace") for field in fields[:5]
  )
  return Commit(
    sha=commit_sha,
    author=Identity(author_name, author_email),
    committer=Identity(committer_name, committer_email),
  )


def changed_paths(sha: str) -> list[str]:
  raw = git_bytes("diff-tree", "--root", "--no-commit-id", "--name-only", "-r", "-m", "-z", sha)
  return [os.fsdecode(path) for path in raw.split(b"\0") if path]


def is_dima_or_korolev(name: str) -> bool:
  lowered = name.casefold()
  return "dima" in lowered or "korolev" in lowered


def is_canonical_email(email: str) -> bool:
  return email == CANONICAL_EMAIL or bool(re.fullmatch(r"dmitry\.korolev\+[^@\s]+@gmail\.com", email))


def is_elon(identity: Identity) -> bool:
  lowered_name = identity.name.casefold()
  return lowered_name in {ELON_NAME.casefold(), "elon presley"} or identity.email.casefold() == ELON_EMAIL.casefold()


def identities(commit: Commit) -> tuple[tuple[str, Identity], ...]:
  if commit.author == commit.committer:
    return (("author and committer", commit.author),)
  return (("author", commit.author), ("committer", commit.committer))


def check_pr_commits(base: str, head: str) -> None:
  """Reject forbidden files and identities in commits reachable only from `head`."""
  violations: list[str] = []
  for sha in commits_in_range(base, head):
    commit = read_commit(sha)
    short = commit.sha[:12]
    if any(pathlib.PurePosixPath(path).name.casefold() == PR_DESCRIPTION for path in changed_paths(sha)):
      violations.append(
        f"{short}: changes PR-DESCRIPTION.md; put that text in the GitHub PR body and remove the notes commit"
      )
    for role, identity in identities(commit):
      if is_elon(identity):
        violations.append(
          f"{short}: {role} {identity.name} <{identity.email}> is the local-only Elon Presley identity; "
          "remove or re-author that commit before pushing"
        )
      elif is_dima_or_korolev(identity.name) and not is_canonical_email(identity.email):
        violations.append(
          f"{short}: {role} {identity.name} <{identity.email}> must use {CANONICAL_EMAIL} "
          f"or {CANONICAL_EMAIL_EXAMPLE}"
        )
  if violations:
    raise SystemExit("pull-request commit policy failed:\n- " + "\n- ".join(violations))


def main(argv: list[str]) -> None:
  if len(argv) != 3:
    raise SystemExit("usage: check-pr-commits.py <base-sha> <head-sha>")
  check_pr_commits(argv[1], argv[2])
  print("pull-request commit policy verified")


if __name__ == "__main__":
  main(sys.argv)
