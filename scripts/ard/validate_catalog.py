#!/usr/bin/env python3
"""Validate the ARD catalog against the vendored ai-catalog JSON Schema.

ARD (Agentic Resource Discovery) is new and fluid, so we pin specVersion 1.0
and validate against a vendored copy of the schema (scripts/ard/) rather than
fetching upstream at CI time. Run from anywhere:

    python3 scripts/ard/validate_catalog.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

try:
    from jsonschema import Draft202012Validator
except ModuleNotFoundError:
    sys.exit("error: jsonschema not installed — `pip install jsonschema`")

REPO = Path(__file__).resolve().parents[2]
SCHEMA = REPO / "scripts" / "ard" / "ai-catalog.schema.json"
CATALOG = REPO / "docs" / ".well-known" / "ai-catalog.json"

# The one entry we expect to advertise; guards against accidental drift.
EXPECTED_IDENTIFIER = "urn:ai:github.com:nonokia:dlog"
EXPECTED_SPEC_VERSION = "1.0"


def main() -> int:
    schema = json.loads(SCHEMA.read_text())
    catalog = json.loads(CATALOG.read_text())

    validator = Draft202012Validator(schema)
    errors = sorted(validator.iter_errors(catalog), key=lambda e: list(e.path))
    if errors:
        for err in errors:
            loc = "/".join(str(p) for p in err.path) or "<root>"
            print(f"schema error at {loc}: {err.message}", file=sys.stderr)
        return 1

    # A couple of project-specific sanity checks beyond the generic schema.
    if catalog.get("specVersion") != EXPECTED_SPEC_VERSION:
        print(
            f"specVersion is pinned to {EXPECTED_SPEC_VERSION}; "
            f"got {catalog.get('specVersion')!r}",
            file=sys.stderr,
        )
        return 1
    identifiers = [e.get("identifier") for e in catalog.get("entries", [])]
    if EXPECTED_IDENTIFIER not in identifiers:
        print(
            f"expected entry {EXPECTED_IDENTIFIER!r} not found; got {identifiers}",
            file=sys.stderr,
        )
        return 1

    print(
        f"ok: {CATALOG.relative_to(REPO)} is valid "
        f"(specVersion {catalog['specVersion']}, {len(identifiers)} entr"
        f"{'y' if len(identifiers) == 1 else 'ies'})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
