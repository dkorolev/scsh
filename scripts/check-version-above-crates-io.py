#!/usr/bin/env python3
"""Fail unless Cargo.toml version is strictly greater than crates.io max_version."""

from __future__ import annotations

import json
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

CRATE = "scsh"
USER_AGENT = "scsh-ci (https://github.com/dkorolev/scsh)"
API = f"https://crates.io/api/v1/crates/{CRATE}"


def parse_version(version: str) -> tuple[int, ...]:
  m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-.*)?", version.strip())
  if not m:
    raise ValueError(f"unsupported version format: {version!r}")
  return int(m.group(1)), int(m.group(2)), int(m.group(3))


def local_version() -> str:
  cargo = Path(__file__).resolve().parents[1] / "Cargo.toml"
  for line in cargo.read_text(encoding="utf-8").splitlines():
    if line.startswith("version"):
      m = re.search(r'"([^"]+)"', line)
      if m:
        return m.group(1)
  raise SystemExit("could not read version from Cargo.toml")


def published_version() -> str | None:
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
  return data["crate"]["max_version"]


def main() -> None:
  local = local_version()
  published = published_version()
  if published is None:
    print(f"ok: {CRATE} is not on crates.io yet; local version is {local}")
    return
  if parse_version(local) <= parse_version(published):
    print(
      f"FAIL: Cargo.toml version {local} must be strictly above crates.io {published}",
      file=sys.stderr,
    )
    sys.exit(1)
  print(f"ok: local {local} > crates.io {published}")


if __name__ == "__main__":
  main()
