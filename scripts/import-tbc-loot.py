#!/usr/bin/env python3
"""Convert the pinned TBC creature loot tables used by the selected starting-zone pack."""

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
ITEM_FIELDS = (
    "entry",
    "class",
    "subclass",
    "name",
    "displayid",
    "Quality",
    "BuyPrice",
    "SellPrice",
    "InventoryType",
    "AllowableClass",
    "AllowableRace",
    "ItemLevel",
    "stackable",
    "armor",
    "bonding",
    "Material",
    "MaxDurability",
)


class ImportError(Exception):
    pass


def _sql_helpers():
    path = pathlib.Path(__file__).with_name("import-tbc-content.py")
    spec = importlib.util.spec_from_file_location("tbc_content_importer", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def table_columns(source_path, table):
    prefix = f"CREATE TABLE `{table}` ("
    columns = []
    active = False
    with source_path.open(encoding="utf-8", errors="replace") as source:
        for line in source:
            if line.startswith(prefix):
                active = True
                continue
            if active:
                column = re.match(r"\s*`([^`]+)`", line)
                if column:
                    columns.append(column.group(1))
                if line.startswith(") ENGINE"):
                    return columns
    raise ImportError(f"source is missing CREATE TABLE `{table}`")


def convert(source_path, zone_pack_path):
    helpers = _sql_helpers()
    template_entries = {
        int(template["entry"])
        for template in tomllib.loads(zone_pack_path.read_text(encoding="utf-8"))["template"]
    }
    creature_columns = table_columns(source_path, "creature_template")
    creature_index = {name: index for index, name in enumerate(creature_columns)}
    creature_rows = {}
    for row in helpers.rows(source_path, "creature_template"):
        entry = int(row[creature_index["Entry"]])
        if entry in template_entries:
            creature_rows[entry] = row
    missing_creatures = template_entries - creature_rows.keys()
    if missing_creatures:
        raise ImportError(f"source is missing creature templates: {sorted(missing_creatures)}")

    creatures = []
    table_ids = set()
    for entry, row in sorted(creature_rows.items()):
        loot_id = int(row[creature_index["LootId"]])
        creatures.append(
            (
                entry,
                loot_id,
                int(row[creature_index["MinLootGold"]]),
                int(row[creature_index["MaxLootGold"]]),
            )
        )
        if loot_id:
            table_ids.add(loot_id)

    loot_tables = {}
    for row in helpers.rows(source_path, "creature_loot_template"):
        table_id = int(row[0])
        if table_id in table_ids:
            loot_tables.setdefault(table_id, []).append(row)

    reference_tables = {}
    pending = {
        -int(row[4])
        for rows in loot_tables.values()
        for row in rows
        if int(row[4]) < 0
    }
    while pending:
        current = pending - reference_tables.keys()
        if not current:
            break
        found = {table_id: [] for table_id in current}
        for row in helpers.rows(source_path, "reference_loot_template"):
            table_id = int(row[0])
            if table_id in current:
                found[table_id].append(row)
        missing = current - {table_id for table_id, rows in found.items() if rows}
        if missing:
            raise ImportError(f"source is missing reference loot tables: {sorted(missing)}")
        reference_tables.update(found)
        pending = {
            -int(row[4])
            for rows in found.values()
            for row in rows
            if int(row[4]) < 0
        }

    item_ids = {
        int(row[1])
        for rows in [*loot_tables.values(), *reference_tables.values()]
        for row in rows
        if int(row[4]) >= 0 and int(row[1]) > 0
    }
    item_columns = table_columns(source_path, "item_template")
    missing_fields = set(ITEM_FIELDS) - set(item_columns)
    if missing_fields:
        raise ImportError(f"unsupported item_template columns: missing {sorted(missing_fields)}")
    item_index = {name: index for index, name in enumerate(item_columns)}
    items = {}
    for row in helpers.rows(source_path, "item_template"):
        entry = int(row[item_index["entry"]])
        if entry in item_ids:
            items[entry] = {name: row[item_index[name]] for name in ITEM_FIELDS}
    if item_ids - items.keys():
        raise ImportError(f"source is missing item templates: {sorted(item_ids - items.keys())}")

    output = [
        f"schema_version = {SCHEMA_VERSION}",
        f"source = {json.dumps(SOURCE)}",
        f"revision = {json.dumps(REVISION)}",
        f"license = {json.dumps(LICENSE)}",
        'extraction_command = "python3 scripts/import-tbc-loot.py <TBC-DB full SQL dump> '
        "crates/benilla-game-core/content/tbc-starting-zones.toml "
        'crates/benilla-game-core/content/tbc-creature-loot.toml"',
        "",
    ]
    for entry, loot_id, min_gold, max_gold in creatures:
        output.extend(
            [
                "[[creature]]",
                f"entry = {entry}",
                f"loot_id = {loot_id}",
                f"min_gold = {min_gold}",
                f"max_gold = {max_gold}",
                "",
            ]
        )
    for table_id, rows in sorted(loot_tables.items()):
        output.extend(_write_table(table_id, rows, "creature_loot"))
    for table_id, rows in sorted(reference_tables.items()):
        output.extend(_write_table(table_id, rows, "reference_loot"))
    for entry, item in sorted(items.items()):
        output.extend(["[[item_template]]", f"entry = {entry}"])
        for name in ITEM_FIELDS[1:]:
            key = {
                "displayid": "display_id",
                "Quality": "quality",
                "BuyPrice": "buy_price",
                "SellPrice": "sell_price",
                "InventoryType": "inventory_type",
                "AllowableClass": "allowable_class",
                "AllowableRace": "allowable_race",
                "ItemLevel": "item_level",
                "Material": "material",
                "MaxDurability": "max_durability",
            }.get(name, name.lower())
            value = item[name]
            output.append(f"{key} = {json.dumps(value) if name == 'name' else int(value)}")
        output.append("")
    return "\n".join(output).rstrip() + "\n", len(creatures), len(loot_tables), len(reference_tables), len(items)


def _write_table(table_id, rows, table_name):
    output = []
    for row in rows:
        output.extend(
            [
                f"[[{table_name}]]",
                f"table_id = {table_id}",
                f"item = {int(row[1])}",
                f"chance = {float(row[2])}",
                f"group_id = {int(row[3])}",
                f"min_count_or_ref = {int(row[4])}",
                f"max_count = {int(row[5])}",
                f"condition_id = {int(row[6])}",
                f"comment = {json.dumps(row[7])}",
                "",
            ]
        )
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path, help="pinned CMaNGOS TBC full SQL dump")
    parser.add_argument("zone_pack", type=pathlib.Path, help="converted starting-zone content pack")
    parser.add_argument("output", type=pathlib.Path, help="generated creature loot TOML")
    args = parser.parse_args()
    try:
        content, creatures, tables, references, items = convert(args.source, args.zone_pack)
    except (OSError, UnicodeError, ImportError, ValueError, KeyError) as error:
        parser.error(str(error))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(content, encoding="utf-8", newline="\n")
    print(
        f"wrote {creatures} creatures, {tables} creature loot tables, {references} references, "
        f"and {items} item templates to {args.output}"
    )


if __name__ == "__main__":
    main()
