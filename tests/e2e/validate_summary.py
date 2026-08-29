#!/usr/bin/env python3
"""Validate streamtop summary JSON against schemas/summary.v1.json."""

from __future__ import annotations

import json
import sys
from pathlib import Path


def basic_validate(doc: dict) -> list[str]:
    errs: list[str] = []
    required = [
        "schema",
        "schema_version",
        "verdict",
        "ok",
        "health_score",
        "health_label",
        "status",
        "latency",
        "cdn",
        "origin_stalls",
        "critical_rfc_errors",
        "url",
        "errors",
        "saw_segment",
    ]
    for key in required:
        if key not in doc:
            errs.append(f"missing required field: {key}")
    if doc.get("schema") != "streamtop.summary.v1":
        errs.append("schema must be streamtop.summary.v1")
    if doc.get("verdict") not in ("PASS", "FAIL"):
        errs.append("invalid verdict")
    if doc.get("status") not in ("LIVE", "DEGRADED", "ERROR"):
        errs.append("invalid status")
    score = doc.get("health_score")
    if not isinstance(score, int) or not (0 <= score <= 100):
        errs.append("health_score out of range")
    return errs


def jsonschema_validate(doc: dict, schema_path: Path) -> list[str]:
    try:
        import jsonschema  # type: ignore
    except ImportError:
        return basic_validate(doc)
    schema = json.loads(schema_path.read_text(encoding="utf-8"))
    validator = jsonschema.Draft202012Validator(schema)
    return [e.message for e in validator.iter_errors(doc)]


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: validate_summary.py <summary.json>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    doc = json.loads(path.read_text(encoding="utf-8"))
    schema = Path(__file__).resolve().parents[2] / "schemas" / "summary.v1.json"
    errs = jsonschema_validate(doc, schema)
    if errs:
        for e in errs:
            print(f"FAIL: {e}", file=sys.stderr)
        return 1
    print("PASS: summary matches schema")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
