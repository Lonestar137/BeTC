#!/usr/bin/env python3
"""Convert trainer-referenced conditions from the pinned CMaNGOS TBC SQL dump to TOML."""

import argparse
import importlib.util
import json
import pathlib
import re
import tomllib


SOURCE = "https://github.com/cmangos/tbc-db"
REVISION = "866723612d93791e442fb1b18d642118fa682edd"
LICENSE = "GPL-3.0"
SCHEMA_VERSION = 1
EXPECTED_COLUMNS = [
    "condition_entry",
    "type",
    "value1",
    "value2",
    "value3",
    "value4",
    "flags",
    "comments",
]
CREATE_RE = re.compile(r"^CREATE TABLE `conditions`\s*\(")
INSERT_RE = re.compile(r"^INSERT INTO `conditions` VALUES (.*);$")
COLUMN_RE = re.compile(r"^\s*`([^`]+)`\s+")


class ImportError(Exception):
    pass


def _sql_helpers():
    path = pathlib.Path(__file__).with_name("import-tbc-progression.py")
    spec = importlib.util.spec_from_file_location("tbc_progression_importer", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def conditions_from_sql(path):
    schemas = []
    active = False
    inserts = []
    helpers = _sql_helpers()
    with path.open(encoding="utf-8", errors="strict") as source:
        for line_number, line in enumerate(source, 1):
            if CREATE_RE.match(line):
                if active or schemas:
                    raise ImportError(f"line {line_number}: duplicate conditions CREATE TABLE")
                active = True
                continue
            if active:
                column = COLUMN_RE.match(line)
                if column:
                    schemas.append(column.group(1))
                if line.lstrip().startswith(")"):
                    active = False
                    if schemas != EXPECTED_COLUMNS:
                        raise ImportError(
                            f"unsupported conditions columns: expected {EXPECTED_COLUMNS!r}, "
                            f"found {schemas!r}"
                        )
            match = INSERT_RE.match(line.rstrip("\r\n"))
            if match:
                for values in helpers.tuple_values(match.group(1)):
                    row = helpers.split_fields(values)
                    if len(row) != len(EXPECTED_COLUMNS):
                        raise ImportError(
                            f"line {line_number}: condition has {len(row)} fields, "
                            f"expected {len(EXPECTED_COLUMNS)}"
                        )
                    try:
                        numbers = tuple(int(row[i]) for i in range(7))
                    except (TypeError, ValueError) as error:
                        raise ImportError(
                            f"line {line_number}: condition numeric column is invalid"
                        ) from error
                    condition_id, kind, value1, value2, value3, value4, flags = numbers
                    if condition_id == 0:
                        continue
                    if condition_id < 0 or kind < -128 or kind > 127 or min(
                        value1, value2, value3, value4, flags
                    ) < 0:
                        raise ImportError(
                            f"line {line_number}: invalid condition row {numbers!r}"
                        )
                    inserts.append(
                        {
                            "id": condition_id,
                            "kind": kind,
                            "value1": value1,
                            "value2": value2,
                            "value3": value3,
                            "value4": value4,
                            "flags": flags,
                            "comment": row[7] or "",
                        }
                    )
    if not schemas:
        raise ImportError("source is missing CREATE TABLE `conditions`")
    if not inserts:
        raise ImportError("source is missing INSERT data for `conditions`")
    by_id = {}
    for condition in inserts:
        if condition["id"] in by_id:
            raise ImportError(f"duplicate condition id {condition['id']}")
        by_id[condition["id"]] = condition
    return by_id


def convert(source_path, progression_path):
    try:
        progression = tomllib.loads(progression_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise ImportError(f"cannot read progression content: {error}") from error
    source_ids = {
        int(offer["condition_id"])
        for offer in progression.get("trainer_spell", [])
        if int(offer.get("condition_id", 0)) != 0
    }
    if not source_ids:
        raise ImportError("progression pack contains no trainer condition references")

    conditions = conditions_from_sql(source_path)
    included = set()
    visiting = set()

    def include(condition_id):
        if condition_id in included:
            return
        if condition_id in visiting:
            raise ImportError(f"condition expression cycle includes id {condition_id}")
        condition = conditions.get(condition_id)
        if condition is None:
            raise ImportError(f"trainer condition {condition_id} is unresolved in source")
        visiting.add(condition_id)
        if condition["kind"] in (-1, -2):
            children = [
                condition[f"value{i}"]
                for i in range(1, 5)
                if condition[f"value{i}"] != 0
            ]
            if not children:
                raise ImportError(f"boolean condition {condition_id} has no operands")
            for child in children:
                include(child)
        visiting.remove(condition_id)
        included.add(condition_id)

    for condition_id in sorted(source_ids):
        include(condition_id)

    output = [
        f"schema_version = {SCHEMA_VERSION}",
        f"source = {json.dumps(SOURCE)}",
        f"revision = {json.dumps(REVISION)}",
        f"license = {json.dumps(LICENSE)}",
        'extraction_command = "python3 scripts/import-tbc-spell-conditions.py '
        '<TBC-DB full SQL dump> crates/benilla-game-core/content/tbc-spell-conditions.toml '
        'crates/benilla-game-core/content/tbc-progression.toml"',
        "",
    ]
    for condition_id in sorted(included):
        condition = conditions[condition_id]
        output.extend(
            [
                "[[condition]]",
                f"id = {condition['id']}",
                f"kind = {condition['kind']}",
                f"value1 = {condition['value1']}",
                f"value2 = {condition['value2']}",
                f"value3 = {condition['value3']}",
                f"value4 = {condition['value4']}",
                f"flags = {condition['flags']}",
                f"comment = {json.dumps(condition['comment'])}",
                "",
            ]
        )
    return "\n".join(output).rstrip() + "\n", len(included)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path, help="pinned CMaNGOS TBC full SQL dump")
    parser.add_argument("output", type=pathlib.Path, help="generated condition TOML")
    parser.add_argument("progression", type=pathlib.Path, help="converted progression TOML")
    args = parser.parse_args()
    try:
        content, count = convert(args.source, args.progression)
    except (OSError, UnicodeError, ImportError) as error:
        parser.error(str(error))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(content, encoding="utf-8", newline="\n")
    print(f"wrote {count} referenced trainer condition(s) to {args.output}")


if __name__ == "__main__":
    main()
