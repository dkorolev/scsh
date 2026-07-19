#!/usr/bin/env python3
"""Deterministic tests for the pull-request commit-policy gate."""

from __future__ import annotations

import importlib.util
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("check-pr-commits.py")
SPEC = importlib.util.spec_from_file_location("check_pr_commits", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
check_pr_commits = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = check_pr_commits
SPEC.loader.exec_module(check_pr_commits)


class PolicyConstants(unittest.TestCase):
  def test_elon_identity_matches_the_scsh_clone_identity(self) -> None:
    source = SCRIPT.parent.parent.joinpath("src/main.rs").read_text(encoding="utf-8")
    self.assertIn(f'SCSH_COMMIT_NAME: &str = "{check_pr_commits.ELON_NAME}"', source)
    self.assertIn(f'SCSH_COMMIT_EMAIL: &str = "{check_pr_commits.ELON_EMAIL}"', source)

  def test_plus_alias_requires_a_nonempty_whitespace_free_tag(self) -> None:
    self.assertTrue(check_pr_commits.is_canonical_email("dmitry.korolev+github@gmail.com"))
    for email in ("dmitry.korolev+@gmail.com", "dmitry.korolev+bad tag@gmail.com"):
      with self.subTest(email=email):
        self.assertFalse(check_pr_commits.is_canonical_email(email))


class PullRequestCommitPolicy(unittest.TestCase):
  """Exercise the gate against real commits in a scratch repository."""

  def setUp(self) -> None:
    repo = tempfile.mkdtemp(prefix="check-pr-commits-")
    self.addCleanup(shutil.rmtree, repo)
    self.addCleanup(os.chdir, os.getcwd())
    os.chdir(repo)
    self.git("init", "-q", "-b", "main")
    self.git("config", "commit.gpgsign", "false")
    self.base = self.commit("Base state.", "base.txt")

  def git(self, *args: str, env: dict[str, str] | None = None) -> str:
    return subprocess.check_output(("git",) + args, text=True, env=env).strip()

  def commit(
    self,
    subject: str,
    path: str,
    *,
    author_name: str = "Alice Example",
    author_email: str = "alice@example.com",
    committer_name: str | None = None,
    committer_email: str | None = None,
  ) -> str:
    target = pathlib.Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(f"{subject}\n", encoding="utf-8")
    self.git("add", "-A")
    env = os.environ.copy()
    env.update(
      {
        "GIT_AUTHOR_NAME": author_name,
        "GIT_AUTHOR_EMAIL": author_email,
        "GIT_COMMITTER_NAME": committer_name or author_name,
        "GIT_COMMITTER_EMAIL": committer_email or author_email,
      }
    )
    self.git("commit", "-q", "-m", subject, env=env)
    return self.git("rev-parse", "HEAD")

  def rejection(self) -> str:
    with self.assertRaises(SystemExit) as rejected:
      check_pr_commits.check_pr_commits(self.base, "HEAD")
    return str(rejected.exception)

  def test_an_empty_or_ordinary_pull_request_passes(self) -> None:
    check_pr_commits.check_pr_commits(self.base, self.base)
    self.commit("Add a feature.", "feature.txt")
    check_pr_commits.check_pr_commits(self.base, "HEAD")

  def test_dima_and_korolev_names_accept_the_canonical_email_and_plus_aliases(self) -> None:
    self.commit(
      "Add first feature.",
      "first.txt",
      author_name="dImA Example",
      author_email="dmitry.korolev@gmail.com",
    )
    self.commit(
      "Add second feature.",
      "second.txt",
      author_name="Dmitry KOROLEV",
      author_email="dmitry.korolev+github@gmail.com",
    )
    check_pr_commits.check_pr_commits(self.base, "HEAD")

  def test_rejects_a_matching_author_with_another_email(self) -> None:
    self.commit(
      "Add a feature.",
      "feature.txt",
      author_name="DIMA Example",
      author_email="other@example.com",
      committer_name="Alice Example",
      committer_email="alice@example.com",
    )
    message = self.rejection()
    self.assertIn("author DIMA Example <other@example.com>", message)
    self.assertIn("dmitry.korolev+something@gmail.com", message)

  def test_rejects_a_matching_committer_with_another_email(self) -> None:
    self.commit(
      "Add a feature.",
      "feature.txt",
      committer_name="Kate Korolev",
      committer_email="kate@example.com",
    )
    self.assertIn("committer Kate Korolev <kate@example.com>", self.rejection())

  def test_rejects_pr_description_at_the_root_or_below_it(self) -> None:
    for path in ("PR-DESCRIPTION.md", "notes/pr-description.MD"):
      with self.subTest(path=path):
        self.git("reset", "--hard", "-q", self.base)
        self.commit("Add pull request notes.", path)
        self.assertIn("changes PR-DESCRIPTION.md", self.rejection())

  def test_rejects_the_elon_identity_by_name_or_email(self) -> None:
    cases = (
      ("dkorolev-neon-elon-bot", "bot@example.com"),
      ("Elon Presley", "bot@example.com"),
      ("Notes Bot", "dmitry.korolev+elon-presley@gmail.com"),
    )
    for name, email in cases:
      with self.subTest(name=name, email=email):
        self.git("reset", "--hard", "-q", self.base)
        self.commit("Add a feature.", "feature.txt", author_name=name, author_email=email)
        self.assertIn("local-only Elon Presley identity", self.rejection())

  def test_checks_the_explicit_head_from_a_synthetic_merge_checkout(self) -> None:
    head = self.commit("Add a feature.", "feature.txt", author_name="Dima", author_email="wrong@example.com")
    self.git("checkout", "-q", "-b", "pr-merge", self.base)
    env = os.environ.copy()
    env.update(
      {
        "GIT_AUTHOR_NAME": "GitHub",
        "GIT_AUTHOR_EMAIL": "noreply@github.com",
        "GIT_COMMITTER_NAME": "GitHub",
        "GIT_COMMITTER_EMAIL": "noreply@github.com",
      }
    )
    self.git("merge", "-q", "--no-ff", head, "-m", "Synthetic merge.", env=env)
    with self.assertRaises(SystemExit):
      check_pr_commits.check_pr_commits(self.base, head)


if __name__ == "__main__":
  unittest.main()
