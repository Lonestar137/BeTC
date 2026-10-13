#!/usr/bin/env python3
"""Convert TBC starting outfits to the local starting-gear content pack.

The worn starting set is client data (`CharStartOutfit.dbc`), while the item
stats live in the server-content database. This reads the pinned CMaNGOS TBC-DB
dump plus the install's `CharStartOutfit.dbc` and writes the TOML pack the
game-core loads at character creation.
"""

import argparse
import pathlib
import re
import struct

SOURCE = "https://github.com/cmangos/tbc-db"
REVISION = "866723612d93791e442fb1b18d642118fa682edd"
LICENSE = "GPL-3.0"
SCHEMA_VERSION = 1
CLIENT_BUILD = 8606

# InventoryType -> equipment slot 0..18 (head..tabard). Two-handers and main-hand
# weapons share the main hand; off-hand/shield/holdable share the off hand;
# finger and trinket take the first of their pair.
INVTYPE_SLOT = {
    1: 0,
    2: 1,
    3: 2,
    4: 3,
    5: 4,
    20: 4,
    6: 5,
    7: 6,
    8: 7,
    9: 8,
    10: 9,
    11: 10,
    12: 12,
    13: 15,
    17: 15,
    21: 15,
    14: 16,
    22: 16,
    23: 16,
    15: 17,
    25: 17,
    26: 17,
    28: 17,
    16: 14,
    19: 18,
}

ITEM_COLUMNS = [
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
]


def load_outfits(dbc_path):
    """CharStartOutfit.dbc -> [(race, class, gender, slot, entry)]."""
    bytes_ = pathlib.Path(dbc_path).read_bytes()
    if bytes_[0:4] != b"WDBC":
        raise SystemExit(f"{dbc_path}: not a WDBC file")
    record_count, field_count, record_size, _ = struct.unpack_from("<IIII", bytes_, 4)
    if field_count != 41 or record_size != 152:
        raise SystemExit(
            f"{dbc_path}: unexpected layout (fields {field_count}, record size {record_size})"
        )
    data = bytes_[20:]

    def i32(off):
        return struct.unpack_from("<i", data, off)[0]

    rows = []
    for i in range(record_count):
        off = i * record_size
        race, klass, gender = data[off + 4], data[off + 5], data[off + 6]
        for slot in range(12):
            entry = i32(off + 8 + slot * 4)
            display = i32(off + 56 + slot * 4)
            inv = i32(off + 104 + slot * 4)
            if entry >= 1 and display >= 1 and inv >= 1:
                rows.append((race, klass, gender, slot, entry, inv))
    return rows


def load_item_columns(schema_text):
    m = re.search(r"CREATE TABLE `item_template` \((.*?)\n\) ENGINE", schema_text, re.S)
    if not m:
        raise SystemExit("item_template CREATE TABLE not found")
    return re.findall(r"^\s*`([A-Za-z0-9_]+)`", m.group(1), re.M)


def split_values(body):
    """Split an INSERT ... VALUES list into row tuples of raw value strings."""
    rows = []
    row = []
    token = []
    in_str = False
    escape = False
    started = False
    for ch in body:
        if in_str:
            if escape:
                token.append(ch)
                escape = False
            elif ch == "\\":
                escape = True
            elif ch == "'":
                in_str = False
            else:
                token.append(ch)
            continue
        if ch == "'":
            in_str = True
            started = True
        elif ch == "(":
            started = True
            row = []
            token = []
        elif ch in ",)":
            if started:
                row.append("".join(token).strip())
                token = []
            if ch == ")":
                rows.append(row)
                started = False
        elif started:
            token.append(ch)
    return rows


def load_items(sql_path):
    text = pathlib.Path(sql_path).read_text(encoding="utf-8", errors="replace")
    columns = load_item_columns(text)
    index = {name: i for i, name in enumerate(columns)}

    def num(raw):
        return 0 if raw in ("NULL", "") else int(raw)

    items = {}
    for m in re.finditer(r"INSERT INTO `item_template` VALUES (.*?);\n", text, re.S):
        for raw in split_values(m.group(1)):
            if len(raw) != len(columns):
                continue
            entry = int(raw[index["entry"]])
            spells = []
            for n in range(1, 6):
                spell_id = num(raw[index[f"spellid_{n}"]])
                if spell_id == 0:
                    continue
                spells.append(
                    {
                        "index": n - 1,
                        "spell_id": spell_id,
                        "trigger": num(raw[index[f"spelltrigger_{n}"]]),
                        "charges": num(raw[index[f"spellcharges_{n}"]]),
                        "cooldown_ms": num(raw[index[f"spellcooldown_{n}"]]),
                        "category": num(raw[index[f"spellcategory_{n}"]]),
                        "category_cooldown_ms": num(
                            raw[index[f"spellcategorycooldown_{n}"]]
                        ),
                    }
                )
            items[entry] = {
                "entry": entry,
                "class": int(raw[index["class"]]),
                "subclass": int(raw[index["subclass"]]),
                "name": raw[index["name"]],
                "display_id": int(raw[index["displayid"]]),
                "quality": int(raw[index["Quality"]]),
                "buy_price": int(raw[index["BuyPrice"]]),
                "sell_price": int(raw[index["SellPrice"]]),
                "inventory_type": int(raw[index["InventoryType"]]),
                "allowable_class": int(raw[index["AllowableClass"]]),
                "allowable_race": int(raw[index["AllowableRace"]]),
                "item_level": int(raw[index["ItemLevel"]]),
                "stackable": int(raw[index["stackable"]]),
                "armor": int(raw[index["armor"]]),
                "bonding": int(raw[index["bonding"]]),
                "material": int(raw[index["Material"]]),
                "max_durability": int(raw[index["MaxDurability"]]),
                "dmg_min": int(round(float(raw[index["dmg_min1"]]))),
                "dmg_max": int(round(float(raw[index["dmg_max1"]]))),
                "delay": int(raw[index["delay"]]),
                "spells": spells,
            }
    return items, index


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sql", help="pinned CMaNGOS TBC-DB full SQL dump")
    parser.add_argument("start_outfit", help="CharStartOutfit.dbc from the install")
    parser.add_argument("output", help="TOML content pack to write")
    args = parser.parse_args()

    outfits = load_outfits(args.start_outfit)
    items, _ = load_items(args.sql)

    starting = []
    seen_items = set()
    for race, klass, gender, _slot, entry, inv in sorted(outfits):
        slot = INVTYPE_SLOT.get(inv)
        if slot is None or entry not in items:
            continue
        starting.append((race, klass, gender, entry, slot))
        seen_items.add(entry)

    out = []
    out.append("schema_version = %d" % SCHEMA_VERSION)
    out.append('source = "%s"' % SOURCE)
    out.append('revision = "%s"' % REVISION)
    out.append('license = "%s"' % LICENSE)
    out.append("client_build = %d" % CLIENT_BUILD)
    out.append(
        'extraction_command = "python3 scripts/import-tbc-starting-gear.py '
        '<TBC-DB full SQL dump> <CharStartOutfit.dbc> '
        'crates/benilla-game-core/content/tbc-starting-gear.toml"'
    )
    for entry in sorted(seen_items):
        item = items[entry]
        out.append("")
        out.append("[[item_template]]")
        for key in [
            "entry",
            "class",
            "subclass",
            "display_id",
            "quality",
            "buy_price",
            "sell_price",
            "inventory_type",
            "allowable_class",
            "allowable_race",
            "item_level",
            "stackable",
            "armor",
            "bonding",
            "material",
            "max_durability",
            "dmg_min",
            "dmg_max",
            "delay",
        ]:
            out.append("%s = %d" % (key, item[key]))
        out.append("name = %s" % toml_string(item["name"]))
        if item["spells"]:
            entries = ", ".join(
                "{ index = %(index)d, spell_id = %(spell_id)d, trigger = %(trigger)d, "
                "charges = %(charges)d, cooldown_ms = %(cooldown_ms)d, "
                "category = %(category)d, "
                "category_cooldown_ms = %(category_cooldown_ms)d }" % block
                for block in item["spells"]
            )
            out.append("spells = [%s]" % entries)
    for race, klass, gender, entry, slot in starting:
        out.append("")
        out.append("[[starting_item]]")
        out.append("race = %d" % race)
        out.append("class = %d" % klass)
        out.append("gender = %d" % gender)
        out.append("entry = %d" % entry)
        out.append("slot = %d" % slot)
        out.append("amount = 1")

    pathlib.Path(args.output).write_text("\n".join(out) + "\n", encoding="utf-8")
    print(
        "starting gear: %d item template(s), %d starting item(s) -> %s"
        % (len(seen_items), len(starting), args.output)
    )


def toml_string(value):
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return '"' + escaped + '"'


if __name__ == "__main__":
    main()
