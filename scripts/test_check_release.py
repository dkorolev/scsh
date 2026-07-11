#!/usr/bin/env python3
"""Deterministic tests for the release-shape gate (no network)."""

from __future__ import annotations

import importlib.util
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("check-release.py")
SPEC = importlib.util.spec_from_file_location("check_release", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
check_release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check_release)


class VersionLadder(unittest.TestCase):
  def test_accepts_exactly_one_semantic_version_step(self) -> None:
    old = (1, 17, 2)
    for new in ((1, 17, 3), (1, 18, 0), (2, 0, 0)):
      with self.subTest(new=new):
        self.assertTrue(check_release.next_step(old, new))

  def test_rejects_skips_resets_and_unchanged_versions(self) -> None:
    old = (1, 15, 1)
    # (1, 17, 0) is the real skip that shipped once: 1.16 never existed on crates.io.
    for new in ((1, 15, 1), (1, 17, 0), (1, 16, 1), (2, 0, 1), (1, 14, 9)):
      with self.subTest(new=new):
        self.assertFalse(check_release.next_step(old, new))


class ReleaseShape(unittest.TestCase):
  """Exercise `check_shape` against real commits in a scratch repository."""

  def setUp(self) -> None:
    repo = tempfile.mkdtemp(prefix="check-release-")
    self.addCleanup(shutil.rmtree, repo)
    self.addCleanup(os.chdir, os.getcwd())
    os.chdir(repo)
    self.git("init", "-q", "-b", "main")
    self.git("config", "user.name", "test")
    self.git("config", "user.email", "test@example.com")
    self.git("config", "commit.gpgsign", "false")
    self.base = self.commit("1.17.2", "Initial state.")

  def git(self, *args: str) -> str:
    return subprocess.check_output(("git",) + args, text=True).strip()

  def commit(self, version: str, subject: str, extra: str | None = None, lock_version: str | None = None) -> str:
    pathlib.Path("Cargo.toml").write_text(f'[package]\nname = "scsh"\nversion = "{version}"\n')
    pathlib.Path("Cargo.lock").write_text(f'[[package]]\nname = "scsh"\nversion = "{lock_version or version}"\n')
    if extra is not None:
      pathlib.Path(extra).write_text("content\n")
    self.git("add", "-A")
    self.git("commit", "-q", "-m", subject)
    return self.git("rev-parse", "HEAD")

  def test_a_pr_without_a_version_bump_passes(self) -> None:
    self.commit("1.17.2", "Add a feature.", extra="feature.txt")
    self.assertIsNone(check_release.check_shape(self.base, "HEAD"))

  def test_a_bump_only_pr_is_a_legal_release(self) -> None:
    self.commit("1.17.3", "Bump version to 1.17.3.")
    self.assertEqual(check_release.check_shape(self.base, "HEAD"), ((1, 17, 2), (1, 17, 3)))

  def test_the_explicit_head_survives_a_synthetic_merge_checkout(self) -> None:
    self.commit("1.17.2", "Add a feature.", extra="feature.txt")
    head = self.commit("1.17.3", "Bump version to 1.17.3.")
    # Reproduce what CI has checked out on pull_request events: GitHub's synthetic
    # refs/pull/N/merge commit, whose subject and empty diff-tree are meaningless.
    self.git("checkout", "-q", "-b", "pr-merge", self.base)
    self.git("merge", "-q", "--no-ff", head, "-m", f"Merge {head[:7]} into {self.base[:7]}")
    self.assertEqual(check_release.check_shape(self.base, head), ((1, 17, 2), (1, 17, 3)))
    with self.assertRaises(SystemExit):
      check_release.check_shape(self.base, "HEAD")

  def test_a_version_skip_is_rejected(self) -> None:
    self.commit("1.19.0", "Bump version to 1.19.0.")
    with self.assertRaises(SystemExit):
      check_release.check_shape(self.base, "HEAD")

  def test_a_final_commit_with_code_changes_is_rejected(self) -> None:
    self.commit("1.17.3", "Bump version to 1.17.3.", extra="feature.txt")
    with self.assertRaises(SystemExit):
      check_release.check_shape(self.base, "HEAD")

  def test_the_old_bump_subject_convention_is_rejected(self) -> None:
    self.commit("1.17.3", "Bump to 1.17.3.")
    with self.assertRaises(SystemExit):
      check_release.check_shape(self.base, "HEAD")

  def test_a_stale_cargo_lock_is_rejected_even_without_a_bump(self) -> None:
    # The 1.17.2 release shipped with Cargo.toml bumped but Cargo.lock stale; the gate
    # now rejects that desync on every PR, bump or not.
    self.commit("1.17.3", "Bump version to 1.17.3.", lock_version="1.17.2")
    with self.assertRaises(SystemExit):
      check_release.check_shape(self.base, "HEAD")


if __name__ == "__main__":
  unittest.main()
