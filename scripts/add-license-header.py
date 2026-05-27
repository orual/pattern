#!/usr/bin/env python3
"""Prepend the Pattern MPL-2.0 license header to source files.

Skips files that already contain "Mozilla Public" within the first 20 lines.

Usage:
    scripts/add-license-header.py [--dry-run] [--check-only] [path ...]

With no paths, defaults to: crates/ plugins/ crates/pattern_runtime/haskell/

Rust (.rs) files use `//` line comments; Haskell (.hs) files use `--`.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

HEADER_LINES = [
    "Copyright 2026 Pattern contributors",
    "",
    "This Source Code Form is subject to the terms of the Mozilla Public",
    "License, v. 2.0. If a copy of the MPL was not distributed with this",
    "file, you can obtain one at http://mozilla.org/MPL/2.0/.",
]

EXCLUDED_DIR_NAMES = {
    "target",
    ".git",
    ".direnv",
    "rewrite-staging",
    ".jj",
    "node_modules",
    ".pattern",
    ".pattern-plugin",
    ".orual",
    ".playwright-mcp",
    "outbox_archive",
}

EXCLUDED_PATH_SUFFIXES = (
    ".db",
)

PRESENCE_MARKER = "Mozilla Public"
PRESENCE_SCAN_LINES = 20


def header_for(suffix: str) -> str:
    if suffix == ".rs":
        prefix = "// "
        empty = "//"
    elif suffix == ".hs":
        prefix = "-- "
        empty = "--"
    else:
        raise ValueError(f"unsupported suffix: {suffix}")
    lines = []
    for line in HEADER_LINES:
        lines.append(empty if line == "" else f"{prefix}{line}")
    return "\n".join(lines) + "\n\n"


def is_excluded(path: Path) -> bool:
    for part in path.parts:
        if part in EXCLUDED_DIR_NAMES:
            return True
    return any(str(path).endswith(s) for s in EXCLUDED_PATH_SUFFIXES)


def already_has_header(path: Path) -> bool:
    try:
        with path.open("r", encoding="utf-8", errors="replace") as f:
            for i, line in enumerate(f):
                if i >= PRESENCE_SCAN_LINES:
                    break
                if PRESENCE_MARKER in line:
                    return True
    except OSError:
        return False
    return False


def collect_targets(roots: list[Path]) -> list[Path]:
    targets: list[Path] = []
    for root in roots:
        if not root.exists():
            print(f"skip (missing): {root}", file=sys.stderr)
            continue
        if root.is_file():
            if root.suffix in (".rs", ".hs") and not is_excluded(root):
                targets.append(root)
            continue
        for path in root.rglob("*"):
            if not path.is_file():
                continue
            if path.suffix not in (".rs", ".hs"):
                continue
            if is_excluded(path.relative_to(REPO_ROOT) if path.is_absolute() and REPO_ROOT in path.parents else path):
                continue
            targets.append(path)
    return targets


def prepend_header(path: Path, dry_run: bool) -> bool:
    header = header_for(path.suffix)
    original = path.read_text(encoding="utf-8")
    new_contents = header + original
    if dry_run:
        return True
    path.write_text(new_contents, encoding="utf-8")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="*", type=Path)
    parser.add_argument("--dry-run", action="store_true", help="report what would change without writing")
    parser.add_argument("--check-only", action="store_true", help="exit nonzero if any file is missing the header")
    args = parser.parse_args()

    if args.paths:
        roots = [p if p.is_absolute() else (REPO_ROOT / p) for p in args.paths]
    else:
        roots = [
            REPO_ROOT / "crates",
            REPO_ROOT / "plugins",
        ]

    targets = collect_targets(roots)
    targets.sort()

    skipped = 0
    updated = 0
    missing: list[Path] = []

    for path in targets:
        if already_has_header(path):
            skipped += 1
            continue
        missing.append(path)

    if args.check_only:
        for path in missing:
            print(f"missing: {path.relative_to(REPO_ROOT)}")
        print(f"\ntotal: {len(targets)}  with-header: {skipped}  missing: {len(missing)}")
        return 1 if missing else 0

    for path in missing:
        prepend_header(path, dry_run=args.dry_run)
        verb = "would add" if args.dry_run else "added"
        print(f"{verb}: {path.relative_to(REPO_ROOT)}")
        updated += 1

    print(f"\ntotal: {len(targets)}  with-header: {skipped}  {'would-update' if args.dry_run else 'updated'}: {updated}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
