#!/usr/bin/env python3
"""Convert the pinned CMaNGOS TBC player XP thresholds to the local progression pack."""

import argparse
import importlib.util
import json
import pathlib
import re


SOURCE = "https://github.com/cmangos/tbc-db"
REVISION = "866723612d93791e442fb1b18d642118fa682edd"
LICENSE = "GPL-3.0"
SCHEMA_VERSION = 1
EXPECTED_COLUMNS = ["lvl", "xp_for_next_level"]
CREATE_RE = re.compile(r"^CREATE TABLE `player_xp_for_level`\s*\(")
INSERT_RE = re.compile(r"^INSERT INTO `player_xp_for_level` VALUES (.*);$")
COLUMN_RE = re.compile(r"^\s*`([^`]+)`\s+")


class ImportError(Exception):
    pass


def _sql_helpers():
    path = pathlib.Path(__file__).with_name("import-tbc-progression.py")
    spec = importlib.util.spec_from_file_location("tbc_progression_importer", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def convert(source_path):
    columns = []
    active = False
    rows = []
    helpers = _sql_helpers()
    with source_path.open(encoding="utf-8", errors="strict") as source:
        for line_number, line in enumerate(source, 1):
            if CREATE_RE.match(line):
                if columns or active:
                    raise ImportError(f"line {line_number}: duplicate player_xp_for_level DDL")
                active = True
                continue
            if active:
                column = COLUMN_RE.match(line)
                if column:
                    columns.append(column.group(1))
                if line.lstrip().startswith(")"):
                    active = False
                    if columns != EXPECTED_COLUMNS:
                        raise ImportError(
                            f"unsupported player_xp_for_level columns: expected "
                            f"{EXPECTED_COLUMNS!r}, found {columns!r}"
                        )
            match = INSERT_RE.match(line.rstrip("\r\n"))
            if not match:
                continue
            for raw in helpers.tuple_values(match.group(1)):
                fields = helpers.split_fields(raw)
                if len(fields) != len(EXPECTED_COLUMNS):
                    raise ImportError(
                        f"line {line_number}: XP row has {len(fields)} fields, expected 2"
                    )
                try:
                    level, xp = (int(value) for value in fields)
                except (TypeError, ValueError) as error:
                    raise ImportError(f"line {line_number}: XP fields must be integers") from error
                if not 1 <= level < 70 or xp <= 0:
                    raise ImportError(f"line {line_number}: invalid level XP row {(level, xp)}")
                rows.append((level, xp))
    if not columns:
        raise ImportError("source is missing CREATE TABLE `player_xp_for_level`")
    by_level = {}
    for level, xp in rows:
        if level in by_level:
            raise ImportError(f"duplicate XP threshold for level {level}")
        by_level[level] = xp
    if set(by_level) != set(range(1, 70)):
        raise ImportError("source must provide exactly the 69 thresholds for levels 1-69")

    output = [
        f"schema_version = {SCHEMA_VERSION}",
        f"source = {json.dumps(SOURCE)}",
        f"revision = {json.dumps(REVISION)}",
        f"license = {json.dumps(LICENSE)}",
        'extraction_command = "python3 scripts/import-tbc-player-levels.py '
        "<TBC-DB full SQL dump> crates/benilla-game-core/content/tbc-player-levels.toml\"",
        "",
    ]
    for level, xp in sorted(by_level.items()):
        output.extend(["[[level_xp]]", f"level = {level}", f"xp_to_next = {xp}", ""])
    return "\n".join(output).rstrip() + "\n", len(by_level)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path, help="pinned CMaNGOS TBC full SQL dump")
    parser.add_argument("output", type=pathlib.Path, help="generated level-XP TOML")
    args = parser.parse_args()
    try:
        content, count = convert(args.source)
    except (OSError, UnicodeError, ImportError) as error:
        parser.error(str(error))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(content, encoding="utf-8", newline="\n")
    print(f"wrote {count} TBC level XP thresholds to {args.output}")


if __name__ == "__main__":
    main()
