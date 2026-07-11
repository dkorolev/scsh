#!/usr/bin/env python3
"""Gate the release shape of a pull request.

A PR that leaves the version alone passes (after a Cargo.lock consistency check). A PR
that bumps the version must be a proper release: exactly one patch/minor/major step from
the base, the base version already published on crates.io, and the final commit a
manifest-only commit titled exactly `Bump version to X.Y.Z.`.

This supersedes check-version-above-crates-io.py, whose strictly-greater rule failed
every no-bump PR opened after the current version reached crates.io.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
import urllib.error
import urllib.request

CRATE = "scsh"
USER_AGENT = "scsh-ci (https://github.com/dkorolev/scsh)"
API = f"https://crates.io/api/v1/crates/{CRATE}"

Version = tuple[int, int, int]


def run(*args: str) -> str:
  return subprocess.check_output(args, text=True).strip()


def fmt(version: Version) -> str:
  return ".".join(map(str, version))


def parse_version(version: str) -> Version:
  m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", version.strip())
  if not m:
    raise SystemExit(f"unsupported version format: {version!r}")
  return int(m.group(1)), int(m.group(2)), int(m.group(3))


def toml_version_at(ref: str) -> Version:
  text = run("git", "show", f"{ref}:Cargo.toml")
  m = re.search(r'^version = "([^"]+)"$', text, re.MULTILINE)
  if not m:
    raise SystemExit(f"could not read the package version from Cargo.toml at {ref}")
  return parse_version(m.group(1))


def lock_version_at(ref: str) -> Version:
  text = run("git", "show", f"{ref}:Cargo.lock")
  m = re.search(r'name = "scsh"\nversion = "([^"]+)"', text)
  if not m:
    raise SystemExit(f"could not read the scsh version from Cargo.lock at {ref}")
  return parse_version(m.group(1))


def next_step(old: Version, new: Version) -> bool:
  major, minor, patch = old
  return new in {(major, minor, patch + 1), (major, minor + 1, 0), (major + 1, 0, 0)}


def published_version() -> Version | None:
  req = urllib.request.Request(API, headers={"User-Agent": USER_AGENT})
  try:
    with urllib.request.urlopen(req, timeout=30) as resp:
      data = json.load(resp)
  except urllib.error.HTTPError as e:
    if e.code == 404:
      return None
    raise
  if "crate" not in data:
    detail = data.get("errors", [{}])[0].get("detail", "unknown crates.io error")
    raise SystemExit(f"crates.io API error: {detail}")
  return parse_version(data["crate"]["max_version"])


def check_shape(base: str, head: str) -> tuple[Version, Version] | None:
  """Enforce the release shape on the PR's own commits (no network involved).

  Returns (old, new) for a release PR, None when the PR leaves the version alone. The
  shape is checked on `head`, never on the checkout: on pull_request events the checkout
  is GitHub's synthetic merge commit, whose subject and empty diff-tree carry no release
  shape. A PR consisting of nothing but the bump commit is legal by construction
  (`head^` is then the base): the repair path when a release must be re-cut without
  code changes.
  """
  old, new = toml_version_at(base), toml_version_at(head)
  lock = lock_version_at(head)
  if lock != new:
    raise SystemExit(
      f"Cargo.lock pins scsh {fmt(lock)} but Cargo.toml says {fmt(new)}; run `cargo update -w`"
    )
  if new == old:
    return None
  if not next_step(old, new):
    raise SystemExit(
      f"version must advance exactly one patch/minor/major step: {fmt(old)} -> {fmt(new)}"
    )
  last_files = set(run("git", "diff-tree", "--no-commit-id", "--name-only", "-r", head).splitlines())
  if not last_files or not last_files <= {"Cargo.toml", "Cargo.lock"}:
    raise SystemExit(
      f"the final version-bump commit may touch only Cargo.toml and Cargo.lock; got {sorted(last_files)}"
    )
  if toml_version_at(f"{head}^") != old:
    raise SystemExit("the final commit must contain the version bump")
  expected_subject = f"Bump version to {fmt(new)}."
  subject = run("git", "show", "-s", "--format=%s", head)
  if subject != expected_subject:
    raise SystemExit(f"the final commit subject must be `{expected_subject}`; got `{subject}`")
  return old, new


def main(argv: list[str]) -> None:
  if len(argv) not in (2, 3):
    raise SystemExit("usage: check-release.py <base-sha> [head-sha]")
  base, head = argv[1], argv[2] if len(argv) == 3 else "HEAD"
  step = check_shape(base, head)
  if step is None:
    print("ok: no version change; nothing to release")
    return
  old, new = step
  published = published_version()
  if published is not None and published != old:
    raise SystemExit(
      f"base version {fmt(old)} must equal the crates.io latest {fmt(published)}"
    )
  print(f"release step verified: {fmt(old)} -> {fmt(new)}")


if __name__ == "__main__":
  main(sys.argv)
