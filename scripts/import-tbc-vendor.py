#!/usr/bin/env python3
"""Convert the pinned TBC vendor inventories used by the checked-in world spawn pack.

The pinned dump keeps direct vendor stock in `npc_vendor` and reusable stock in
`npc_vendor_template`, referenced by `creature_template.VendorTemplateId`.
`maxcount`/`incrtime` are carried for provenance; the offline slice serves unlimited stock and
never restocks (project decision).
"""

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
    # Server-side weapon damage and speed (the 2.4.3 client's Item.dbc carries display
    # data only). Slot 1 is the weapon swing; the floats are rounded to whole points.
    "dmg_min1",
    "dmg_max1",
    "delay",
)


class ImportError(Exception):
    pass


def _sql_helpers():
    path = pathlib.Path(__file__).with_name("import-tbc-content.py")
    spec = importlib.util.spec_from_file_location("tbc_content_importer", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def table_columns(source_path, table, open_source):
    prefix = f"CREATE TABLE `{table}` ("
    columns = []
    active = False
    with open_source(source_path) as source:
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


def convert(source_path, spawn_pack_path):
    helpers = _sql_helpers()
    template_entries = {
        int(template["entry"])
        for template in tomllib.loads(spawn_pack_path.read_text(encoding="utf-8"))["template"]
    }

    creature_columns = table_columns(source_path, "creature_template", helpers.open_source)
    creature_index = {name.lower(): index for index, name in enumerate(creature_columns)}
    creature_fields = {"entry", "vendortemplateid"}
    if missing := creature_fields - creature_index.keys():
        raise ImportError(f"creature_template is missing columns: {sorted(missing)}")
    creature_rows = {}
    vendor_template_entries = {}
    for row in helpers.rows(source_path, "creature_template"):
        entry = int(row[creature_index["entry"]])
        if entry in template_entries:
            creature_rows[entry] = row
            vendor_template = int(row[creature_index["vendortemplateid"]])
            if vendor_template:
                vendor_template_entries.setdefault(vendor_template, []).append(entry)
    if missing := template_entries - creature_rows.keys():
        raise ImportError(f"source is missing creature templates: {sorted(missing)}")

    vendor_columns = table_columns(source_path, "npc_vendor", helpers.open_source)
    vendor_index = {name.lower(): index for index, name in enumerate(vendor_columns)}
    template_vendor_columns = table_columns(
        source_path, "npc_vendor_template", helpers.open_source
    )
    template_vendor_index = {
        name.lower(): index for index, name in enumerate(template_vendor_columns)
    }
    vendors = []
    seen_vendor_items = set()

    def append_vendor(entry, row, columns):
        item = int(row[columns["item"]])
        key = (entry, item)
        if item == 0 or key in seen_vendor_items:
            return
        seen_vendor_items.add(key)
        vendors.append(
            (
                entry,
                item,
                int(row[columns["maxcount"]]),
                int(row[columns["incrtime"]]),
                int(row[columns["slot"]]),
            )
        )

    for row in helpers.rows(source_path, "npc_vendor"):
        entry = int(row[vendor_index["entry"]])
        if entry in template_entries:
            append_vendor(entry, row, vendor_index)

    for row in helpers.rows(source_path, "npc_vendor_template"):
        vendor_template = int(row[template_vendor_index["entry"]])
        for entry in vendor_template_entries.get(vendor_template, ()):
            append_vendor(entry, row, template_vendor_index)

    if not vendors:
        raise ImportError("source has no vendor rows for the world templates")

    item_ids = {item for _, item, _, _, _ in vendors}
    item_columns = table_columns(source_path, "item_template", helpers.open_source)
    missing_fields = (set(ITEM_FIELDS) | {"BuyCount"}) - set(item_columns)
    if missing_fields:
        raise ImportError(f"unsupported item_template columns: missing {sorted(missing_fields)}")
    item_index = {name: index for index, name in enumerate(item_columns)}
    items = {}
    for row in helpers.rows(source_path, "item_template"):
        entry = int(row[item_index["entry"]])
        if entry in item_ids:
            items[entry] = {
                name: row[item_index[name]] for name in (*ITEM_FIELDS, "BuyCount")
            }
    if item_ids - items.keys():
        raise ImportError(f"source is missing item templates: {sorted(item_ids - items.keys())}")

    output = [
        f"schema_version = {SCHEMA_VERSION}",
        f"source = {json.dumps(SOURCE)}",
        f"revision = {json.dumps(REVISION)}",
        f"license = {json.dumps(LICENSE)}",
        'extraction_command = "python3 scripts/import-tbc-vendor.py <TBC-DB full SQL dump> '
        "crates/benilla-game-core/content/tbc-world-spawns.toml "
        'crates/benilla-game-core/content/tbc-vendor.toml"',
        "",
    ]
    for entry, item, maxcount, incrtime, slot in sorted(vendors):
        buy_count = max(1, int(items[item]["BuyCount"]))
        output.extend(
            [
                "[[vendor]]",
                f"entry = {entry}",
                f"item = {item}",
                f"maxcount = {maxcount}",
                f"incrtime = {incrtime}",
                f"buy_count = {buy_count}",
                "",
            ]
        )
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
                "dmg_min1": "dmg_min",
                "dmg_max1": "dmg_max",
            }.get(name, name.lower())
            value = item[name]
            if name == "name":
                output.append(f"{key} = {json.dumps(value)}")
            elif name in ("dmg_min1", "dmg_max1"):
                output.append(f"{key} = {int(round(float(value)))}")
            else:
                output.append(f"{key} = {int(value)}")
        output.append("")
    return "\n".join(output).rstrip() + "\n", len(vendors), len(items)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path, help="pinned CMaNGOS TBC full SQL dump")
    parser.add_argument("spawn_pack", type=pathlib.Path, help="converted world spawn pack")
    parser.add_argument("output", type=pathlib.Path, help="generated vendor TOML")
    args = parser.parse_args()
    try:
        content, vendors, items = convert(args.source, args.spawn_pack)
    except (OSError, UnicodeError, ImportError, ValueError, KeyError) as error:
        parser.error(str(error))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(content, encoding="utf-8", newline="\n")
    print(f"wrote {vendors} vendor rows and {items} item templates to {args.output}")


if __name__ == "__main__":
    main()
