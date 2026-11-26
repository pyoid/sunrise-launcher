#!/usr/bin/env python3
"""Add, rename or remove source-English keys in the launcher's catalogs.

`lang/en.json` is the source: it lists every key, each mapped to itself. The other catalogs hold
only real translations; a key a language lacks shows in English. New keys therefore go into
`en.json` alone, and Crowdin offers them to translators from there. Renaming a key carries its
translations along; removing one removes it everywhere.
"""

import argparse
import json
from pathlib import Path

LANG = Path(__file__).resolve().parents[1] / "lang"
SOURCE = LANG / "en.json"


def load(path: Path) -> tuple[str, dict[str, str]]:
    text = path.read_text(encoding="utf-8")
    return text, json.loads(text)


def save(path: Path, before: str, table: dict[str, str]) -> None:
    after = json.dumps(table, ensure_ascii=False, indent=2) + "\n"
    if after != before:
        path.write_text(after, encoding="utf-8")


def renamed(table: dict[str, str], old: str, new: str, source: bool) -> dict[str, str]:
    """`table` with `old` replaced by `new` in place, keeping the entry's position."""
    out = {}
    for key, value in table.items():
        if key == old:
            if new not in table:
                out[new] = new if source else value
        else:
            out[key] = value
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("keys", nargs="*", help="New source-English strings used by tr/trf")
    parser.add_argument("--remove", action="append", default=[], help="Obsolete key, removed everywhere")
    parser.add_argument(
        "--rename",
        nargs=2,
        action="append",
        default=[],
        metavar=("OLD", "NEW"),
        help="Change a key's English, keeping its translations",
    )
    args = parser.parse_args()
    if not (args.keys or args.remove or args.rename):
        parser.error("nothing to do: give keys to add, --remove or --rename")

    for path in sorted(LANG.glob("*.json")):
        source = path == SOURCE
        before, table = load(path)
        for key in args.remove:
            table.pop(key, None)
        for old, new in args.rename:
            table = renamed(table, old, new, source)
        if source:
            for key in args.keys:
                table.setdefault(key, key)
        save(path, before, table)


if __name__ == "__main__":
    main()
