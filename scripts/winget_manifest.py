#!/usr/bin/env python3
"""Check a generated winget installer manifest and restore the portable alias."""

from __future__ import annotations

import re
import sys
from pathlib import Path

PACKAGE_ID = "Jorji49.streamtop"
DEPENDENCY = "Microsoft.VCRedist.2015+.x64"
ALIAS_LINE = "PortableCommandAlias: streamtop"
EXE_LINE = re.compile(r"^(.*)RelativeFilePath: streamtop\.exe\s*$")


def prepare(text: str, version: str) -> str:
    """Return the installer manifest text to submit."""
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    source = text.split("\n")
    lines: list[str] = []
    index = 0
    while index < len(source):
        line = source[index]
        lines.append(line)
        matched = EXE_LINE.match(line)
        if matched is not None and not line.lstrip().startswith("#"):
            column = line.index("RelativeFilePath")
            alias = f"{' ' * column}{ALIAS_LINE}"
            nxt = source[index + 1] if index + 1 < len(source) else ""
            if nxt.strip() == ALIAS_LINE:
                pass
            elif "PortableCommandAlias:" in nxt:
                raise ValueError("PortableCommandAlias is not streamtop")
            else:
                lines.append(alias)
        index += 1
    text = "\n".join(lines)
    if text and not text.endswith("\n"):
        text += "\n"

    errors: list[str] = []
    asset = f"streamtop-windows-x86_64-{version}.zip"
    url = f"https://github.com/Jorji49/streamtop/releases/download/v{version}/{asset}"

    if f"PackageIdentifier: {PACKAGE_ID}" not in text:
        errors.append(f"PackageIdentifier is not {PACKAGE_ID}")
    if f"PackageVersion: {version}" not in text:
        errors.append(f"PackageVersion is not {version}")
    if "InstallerType: zip" not in text:
        errors.append("InstallerType is not zip")
    if "NestedInstallerType: portable" not in text:
        errors.append("NestedInstallerType is not portable")
    if ALIAS_LINE not in text:
        errors.append(f"nested installer is missing {ALIAS_LINE}")
    if "RelativeFilePath: streamtop.exe" not in text:
        errors.append("nested installer is missing streamtop.exe")

    if DEPENDENCY not in text:
        if re.search(r"(?m)^Dependencies:\s*$", text):
            errors.append(f"Dependencies block is missing {DEPENDENCY}")
        else:
            block = (
                "Dependencies:\n"
                "  PackageDependencies:\n"
                f"    - PackageIdentifier: {DEPENDENCY}\n"
            )
            updated, count = re.subn(r"(?m)^Installers:\n", block + "Installers:\n", text, count=1)
            if count != 1:
                errors.append(f"could not add the {DEPENDENCY} dependency")
            else:
                text = updated

    urls = [
        item.strip().strip('"').strip("'")
        for item in re.findall(r"(?m)^[ ]*InstallerUrl:[ ]*(\S+)\s*$", text)
    ]
    if urls != [url]:
        errors.append(f"installer URL must be only {url}")
    if "Architecture: x64" not in text:
        errors.append("installer architecture is not x64")

    if errors:
        raise ValueError("\n".join(errors))
    return text


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: winget_manifest.py INSTALLER_YAML VERSION", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    version = sys.argv[2]
    try:
        updated = prepare(path.read_text(encoding="utf-8"), version)
    except ValueError as exc:
        for line in str(exc).splitlines():
            print(f"::error::{line}")
        return 1
    path.write_text(updated, encoding="utf-8", newline="\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
