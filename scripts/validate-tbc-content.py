#!/usr/bin/env python3
"""Validate the checked-in offline TBC creature content pack."""

import argparse
import math
import sys
import tomllib
from pathlib import Path


SCHEMA_VERSION = 2
F32_MAX = 3.4028234663852886e38

ROOT_FIELDS = {"schema_version", "source", "revision", "license", "template", "spawn"}
OPTIONAL_TABLES = {"waypoint", "addon"}

# Field specs are aligned with the Rust deserialization types in
# crates/benilla-game-core/src/content.rs.
TABLE_FIELDS = {
    "template": {
        "entry": "u32",
        "name": "str",
        "subname": "str",
        "level": "u8",
        "display_id": "u32",
        "faction": "u32",
        "scale": "float",
        "creature_type": "u32",
        "npc_flags": "u32",
    },
    "spawn": {
        "source_guid": "u32",
        "entry": "u32",
        "map_id": "u32",
        "x": "float",
        "y": "float",
        "z": "float",
        "orientation": "float",
        "respawn_min_secs": "u32",
        "respawn_max_secs": "u32",
        "wander_radius": "float",
        "movement_type": "u8",
    },
    "waypoint": {
        "source_guid": "u32",
        "point": "u32",
        "x": "float",
        "y": "float",
        "z": "float",
        "orientation": "float",
        "wait_ms": "u32",
    },
    "addon": {
        "source_guid": "u32",
        "stand_state": "u8",
        "sheath_state": "u8",
        "emote": "u32",
    },
}

MOVEMENT_TYPES = {0, 1, 2, 4}


def validate_document(document):
    errors = []
    if not isinstance(document, dict):
        return ["document: expected a TOML table"], {}

    metadata_fields = {"schema_version", "source", "revision", "license"}
    for key in sorted(metadata_fields - document.keys()):
        errors.append(f"{key}: missing required root field")
    for key in sorted(document.keys() - ROOT_FIELDS - OPTIONAL_TABLES):
        errors.append(f"{key}: unknown root field")

    if "schema_version" in document:
        version = document["schema_version"]
        if type(version) is not int or not 0 <= version <= 0xFFFFFFFF:
            errors.append("schema_version: expected an unsigned 32-bit integer")
        elif version != SCHEMA_VERSION:
            errors.append(f"schema_version: expected {SCHEMA_VERSION}, got {version}")

    for key in ("source", "revision", "license"):
        if key in document and (
            not isinstance(document[key], str) or not document[key].strip()
        ):
            errors.append(f"{key}: expected a non-empty provenance string")

    tables = {}
    for table_name, fields in TABLE_FIELDS.items():
        if table_name not in document:
            if table_name in OPTIONAL_TABLES:
                tables[table_name] = []
                continue
            errors.append(f"{table_name}: missing required table array")
            tables[table_name] = []
            continue

        rows = document[table_name]
        if not isinstance(rows, list):
            errors.append(f"{table_name}: expected an array of tables")
            tables[table_name] = []
            continue
        tables[table_name] = rows

        for index, row in enumerate(rows):
            label = f"{table_name}[{index}]"
            if not isinstance(row, dict):
                errors.append(f"{label}: expected a table")
                continue

            for key in sorted(fields.keys() - row.keys()):
                errors.append(f"{label}.{key}: missing required field")
            for key in sorted(row.keys() - fields.keys()):
                errors.append(f"{label}.{key}: unknown field")
            for key, spec in fields.items():
                if key not in row:
                    continue
                value = row[key]
                kind = spec
                field_label = f"{label}.{key}"
                if kind == "str":
                    if not isinstance(value, str):
                        errors.append(f"{field_label}: expected a string")
                elif kind in ("u8", "u32"):
                    maximum = 0xFF if kind == "u8" else 0xFFFFFFFF
                    if type(value) is not int or not 0 <= value <= maximum:
                        errors.append(f"{field_label}: expected an unsigned {kind[1:]}-bit integer")
                elif kind == "float":
                    if type(value) not in (int, float):
                        errors.append(f"{field_label}: expected a finite number")
                        continue
                    try:
                        numeric_value = float(value)
                    except OverflowError:
                        numeric_value = math.inf
                    if not math.isfinite(numeric_value) or abs(numeric_value) > F32_MAX:
                        errors.append(f"{field_label}: expected a finite f32-representable number")

    template_entries = set()
    for index, row in enumerate(tables["template"]):
        if not isinstance(row, dict):
            continue
        entry = row.get("entry")
        if type(entry) is int and 0 <= entry <= 0xFFFFFFFF:
            if entry in template_entries:
                errors.append(f"template[{index}].entry: duplicate template entry {entry}")
            template_entries.add(entry)

    spawn_guids = set()
    for index, row in enumerate(tables["spawn"]):
        if not isinstance(row, dict):
            continue
        source_guid = row.get("source_guid")
        if type(source_guid) is int and 0 <= source_guid <= 0xFFFFFFFF:
            if source_guid in spawn_guids:
                errors.append(f"spawn[{index}].source_guid: duplicate spawn GUID {source_guid}")
            spawn_guids.add(source_guid)

        movement_type = row.get("movement_type")
        if (
            type(movement_type) is int
            and 0 <= movement_type <= 0xFF
            and movement_type not in MOVEMENT_TYPES
        ):
            errors.append(
                f"spawn[{index}].movement_type: unsupported value {movement_type}; "
                f"expected one of {', '.join(map(str, sorted(MOVEMENT_TYPES)))}"
            )

    for index, row in enumerate(tables["spawn"]):
        if not isinstance(row, dict):
            continue
        entry = row.get("entry")
        if type(entry) is int and 0 <= entry <= 0xFFFFFFFF and entry not in template_entries:
            errors.append(f"spawn[{index}].entry: no template with entry {entry}")

    for table_name in ("waypoint", "addon"):
        for index, row in enumerate(tables[table_name]):
            if not isinstance(row, dict):
                continue
            source_guid = row.get("source_guid")
            if (
                type(source_guid) is int
                and 0 <= source_guid <= 0xFFFFFFFF
                and source_guid not in spawn_guids
            ):
                errors.append(
                    f"{table_name}[{index}].source_guid: no spawn with GUID {source_guid}"
                )

    summary = {name: len(rows) for name, rows in tables.items()}
    return errors, summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pack", type=Path, help="TOML content pack to validate")
    args = parser.parse_args()

    try:
        with args.pack.open("rb") as source:
            document = tomllib.load(source)
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        print(f"content pack error: {error}", file=sys.stderr)
        return 1

    errors, summary = validate_document(document)
    if errors:
        print(f"content pack invalid: {len(errors)} error(s)", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return 1

    print(
        "valid TBC content pack: "
        f"{summary['template']} templates, {summary['spawn']} spawns, "
        f"{summary['waypoint']} waypoints, {summary['addon']} addons"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
