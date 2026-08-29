#!/usr/bin/env python3
"""Verify release versions and optional artifact hashes."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def match(path: Path, pattern: str) -> str:
    text = path.read_text(encoding="utf-8")
    found = re.search(pattern, text, re.MULTILINE)
    if not found:
        raise ValueError(f"{path.relative_to(ROOT)}: expected pattern not found")
    return found.group(1)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--assets-dir", type=Path)
    args = parser.parse_args()

    cargo_version = match(ROOT / "Cargo.toml", r'^version = "([^"]+)"')
    versions = {
        "Cargo.lock": match(
            ROOT / "Cargo.lock",
            r'(?ms)^name = "streamtop"\s+version = "([^"]+)"',
        ),
        "Scoop": match(ROOT / "bucket/streamtop.json", r'"version":\s*"([^"]+)"'),
        "Winget": match(
            ROOT / "dist/winget/Jorji49.streamtop.yaml",
            r"^PackageVersion:\s*(\S+)",
        ),
        "AUR": match(ROOT / "dist/aur/PKGBUILD", r"^pkgver=(\S+)"),
    }
    failures = [
        f"{name}: {version} != {cargo_version}"
        for name, version in versions.items()
        if version != cargo_version
    ]
    for path in [
        ROOT / "bucket/streamtop.json",
        ROOT / "dist/winget/Jorji49.streamtop.installer.yaml",
        ROOT / "dist/winget/Jorji49.streamtop.locale.en-US.yaml",
    ]:
        text = path.read_text(encoding="utf-8")
        for found in re.findall(r"(?:v|PackageVersion:\s*)(\d+\.\d+\.\d+)", text):
            if found != cargo_version:
                failures.append(
                    f"{path.relative_to(ROOT)} references {found}, expected {cargo_version}"
                )

    tracked_text = "\n".join(
        path.read_text(encoding="utf-8", errors="replace")
        for path in [
            ROOT / "bucket/streamtop.json",
            ROOT / "dist/winget/Jorji49.streamtop.installer.yaml",
            ROOT / "dist/aur/PKGBUILD",
        ]
    )
    if "PLACEHOLDER" in tracked_text:
        failures.append("packaging manifests contain PLACEHOLDER")

    if args.assets_dir:
        windows = args.assets_dir / f"streamtop-windows-x86_64-{cargo_version}.zip"
        linux = args.assets_dir / "streamtop-x86_64-unknown-linux-gnu.tar.gz"
        for path in [windows, linux]:
            if not path.is_file():
                failures.append(f"missing artifact: {path}")
        if windows.is_file():
            actual = sha256(windows)
            scoop = json.loads((ROOT / "bucket/streamtop.json").read_text())["architecture"][
                "64bit"
            ]["hash"].lower()
            winget = match(
                ROOT / "dist/winget/Jorji49.streamtop.installer.yaml",
                r"^    InstallerSha256:\s*(\S+)",
            ).lower()
            if scoop != actual:
                failures.append(f"Scoop hash mismatch: {scoop} != {actual}")
            if winget != actual:
                failures.append(f"Winget hash mismatch: {winget} != {actual}")
        if linux.is_file():
            actual = sha256(linux)
            aur = match(
                ROOT / "dist/aur/PKGBUILD",
                r"(?ms)^sha256sums=\('([^']+)'",
            ).lower()
            if aur != actual:
                failures.append(f"AUR archive hash mismatch: {aur} != {actual}")

    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    print(f"PASS: release metadata {cargo_version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
