#!/usr/bin/env python3
"""Set the Homebrew tap formula url and sha256 from a streamtop release tag."""

from __future__ import annotations

import argparse
import base64
import difflib
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

TAP_REPO = "Jorji49/homebrew-tap"
FORMULA_PATH = "Formula/streamtop.rb"
TAG_URL = "https://github.com/Jorji49/streamtop/archive/refs/tags/v{version}.tar.gz"
USER_AGENT = "streamtop-homebrew-tap"
AUTHOR = {"name": "Jorji49", "email": "kamaahmetkayra303@outlook.com"}

URL_LINE = re.compile(
    r'^(  url ")https://github.com/Jorji49/streamtop/archive/refs/tags/v[^"]+\.tar\.gz(")\s*$',
    re.MULTILINE,
)
SHA_LINE = re.compile(r'^(  sha256 ")[0-9a-fA-F]{64}(")\s*$', re.MULTILINE)


def parse_version(raw: str) -> str:
    version = raw.strip()
    if version.startswith("v"):
        version = version[1:]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise SystemExit(f"version must look like 1.8.0, got {raw!r}")
    return version


def download(url: str) -> bytes:
    last_error: Exception | None = None
    for attempt in range(1, 9):
        request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return response.read()
        except urllib.error.HTTPError as exc:
            last_error = exc
            if exc.code not in {404, 429, 500, 502, 503, 504} or attempt == 8:
                raise
        except urllib.error.URLError as exc:
            last_error = exc
            if attempt == 8:
                raise
        time.sleep(min(2 * attempt, 15))
    raise RuntimeError(f"download failed: {last_error}")


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def render_formula(formula: str, version: str, digest: str) -> str:
    url = TAG_URL.format(version=version)
    updated, url_count = URL_LINE.subn(rf"\g<1>{url}\2", formula, count=1)
    if url_count != 1:
        raise SystemExit("Formula/streamtop.rb has no streamtop tag url")
    updated, sha_count = SHA_LINE.subn(rf"\g<1>{digest}\2", updated, count=1)
    if sha_count != 1:
        raise SystemExit("Formula/streamtop.rb has no sha256 line")
    return updated


def api_request(url: str, token: str, method: str = "GET", payload: dict | None = None) -> dict:
    data = None if payload is None else json.dumps(payload).encode()
    headers = {
        "Accept": "application/vnd.github+json",
        "User-Agent": USER_AGENT,
        "X-GitHub-Api-Version": "2022-11-28",
    }
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if data is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            body = response.read()
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", "replace")
        raise SystemExit(f"GitHub API {method} {url} failed: HTTP {exc.code} {detail}") from exc
    return json.loads(body) if body else {}


def fetch_formula(token: str) -> tuple[str, str]:
    url = f"https://api.github.com/repos/{TAP_REPO}/contents/{FORMULA_PATH}?ref=main"
    document = api_request(url, token)
    encoded = document["content"].replace("\n", "")
    text = base64.b64decode(encoded).decode("utf-8")
    return text, document["sha"]


def put_formula(token: str, formula: str, blob_sha: str, version: str) -> None:
    url = f"https://api.github.com/repos/{TAP_REPO}/contents/{FORMULA_PATH}"
    payload = {
        "message": f"streamtop {version}",
        "content": base64.b64encode(formula.encode("utf-8")).decode("ascii"),
        "sha": blob_sha,
        "branch": "main",
        "author": AUTHOR,
        "committer": AUTHOR,
    }
    api_request(url, token, method="PUT", payload=payload)


def show_diff(current: str, updated: str) -> None:
    diff = "".join(
        difflib.unified_diff(
            current.splitlines(keepends=True),
            updated.splitlines(keepends=True),
            fromfile=f"a/{FORMULA_PATH}",
            tofile=f"b/{FORMULA_PATH}",
        )
    )
    if diff:
        print(diff, end="" if diff.endswith("\n") else "\n")
    else:
        print("formula already matches")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    version = parse_version(args.version)
    token = os.environ.get("HOMEBREW_TAP_TOKEN", "")
    if not args.dry_run and not token:
        print(
            "HOMEBREW_TAP_TOKEN is not set. Create a fine-grained personal access "
            "token limited to Jorji49/homebrew-tap with Contents read and write, "
            "then store it as the HOMEBREW_TAP_TOKEN secret on Jorji49/streamtop.",
            file=sys.stderr,
        )
        return 1

    url = TAG_URL.format(version=version)
    digest = sha256(download(url))
    print(f"url: {url}")
    print(f"sha256: {digest}")

    formula, blob_sha = fetch_formula("" if args.dry_run else token)
    updated = render_formula(formula, version, digest)
    show_diff(formula, updated)
    if updated == formula:
        return 0
    if args.dry_run:
        print("dry run: formula was not pushed")
        return 0
    put_formula(token, updated, blob_sha, version)
    print(f"updated {TAP_REPO} {FORMULA_PATH} to {version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
