#!/usr/bin/env python3
"""Convert the base-world CMaNGOS TBC creature spawns into the offline runtime content pack.

Every non-event ``creature`` row with a template (all maps, overworld and instances), its
``creature_template`` row, and the ``creature_movement``/``creature_addon`` rows that belong to it
are imported. Positive-only ``game_event_creature.event`` associations are omitted; negative
associations identify baseline spawns that an active event would remove. The source SQL stays
outside the repository. This intentionally understands only MySQL bulk INSERT rows for those
tables; it is not a general SQL parser.
"""

import argparse
import gzip
import json
import pathlib
import re


SOURCE = "https://github.com/cmangos/tbc-db"
REVISION = "866723612d93791e442fb1b18d642118fa682edd"
SCHEMA_VERSION = 3
EVENT_POLICY = "base-world-only"


def open_source(path):
    path = pathlib.Path(path)
    if path.suffix == ".gz":
        return gzip.open(path, "rt", encoding="utf-8", errors="replace")
    return path.open(encoding="utf-8", errors="replace")


def table_columns(path, table):
    prefix = f"CREATE TABLE `{table}` ("
    columns = []
    active = False
    with open_source(path) as source:
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
    raise ValueError(f"source is missing CREATE TABLE `{table}`")


def tuples(source):
    start = 0
    quoted = False
    escaped = False
    depth = 0
    for index, char in enumerate(source):
        if escaped:
            escaped = False
        elif char == "\\":
            escaped = quoted
        elif char == "'":
            quoted = not quoted
        elif not quoted and char == "(":
            if depth == 0:
                start = index + 1
            depth += 1
        elif not quoted and char == ")":
            depth -= 1
            if depth == 0:
                yield source[start:index]


def fields(source):
    result = []
    start = 0
    quoted = False
    escaped = False
    for index, char in enumerate(source):
        if escaped:
            escaped = False
        elif char == "\\":
            escaped = quoted
        elif char == "'":
            quoted = not quoted
        elif char == "," and not quoted:
            result.append(value(source[start:index]))
            start = index + 1
    result.append(value(source[start:]))
    return result


def value(source):
    source = source.strip()
    if source == "NULL":
        return ""
    if source.startswith("'") and source.endswith("'"):
        return source[1:-1].replace("\\'", "'").replace("\\\\", "\\")
    return source


def rows(path, table):
    prefix = f"INSERT INTO `{table}` VALUES "
    with open_source(path) as source:
        for line in source:
            if line.startswith(prefix):
                for row in tuples(line[len(prefix) :].rstrip().rstrip(";")):
                    yield fields(row)


# `item_template`'s five spell blocks (`ItemPrototype::_ItemSpell`): the ON_USE spell a "use"
# casts, an ON_EQUIP arm and a CHANCE_ON_HIT arm.
ITEM_SPELL_COLUMNS = tuple(
    f"{stem}_{n}"
    for n in range(1, 6)
    for stem in (
        "spellid",
        "spelltrigger",
        "spellcharges",
        "spellcooldown",
        "spellcategory",
        "spellcategorycooldown",
    )
)


def item_spell_blocks(row, item_index):
    """The row's non-empty spell blocks, in block order, as TOML-ready dicts."""
    blocks = []
    for n in range(1, 6):
        spell_id = int(row[item_index[f"spellid_{n}"]] or 0)
        if spell_id == 0:
            continue
        blocks.append(
            {
                "index": n - 1,
                "spell_id": spell_id,
                "trigger": int(row[item_index[f"spelltrigger_{n}"]] or 0),
                "charges": int(row[item_index[f"spellcharges_{n}"]] or 0),
                "cooldown_ms": int(row[item_index[f"spellcooldown_{n}"]] or 0),
                "category": int(row[item_index[f"spellcategory_{n}"]] or 0),
                "category_cooldown_ms": int(
                    row[item_index[f"spellcategorycooldown_{n}"]] or 0
                ),
            }
        )
    return blocks


def write_item_spells(blocks):
    """The `spells = [...]` TOML line; an item with no blocks writes nothing."""
    if not blocks:
        return []
    entries = ", ".join(
        "{{ index = {index}, spell_id = {spell_id}, trigger = {trigger}, "
        "charges = {charges}, cooldown_ms = {cooldown_ms}, category = {category}, "
        "category_cooldown_ms = {category_cooldown_ms} }}".format(**block)
        for block in blocks
    )
    return [f"spells = [{entries}]"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path, help="CMaNGOS SQL dump (plain or gzip-compressed)")
    parser.add_argument("output", type=pathlib.Path, help="generated TOML content pack")
    args = parser.parse_args()

    event_columns = {
        name.lower(): index
        for index, name in enumerate(table_columns(args.source, "game_event_creature"))
    }
    if {"guid", "event"} - event_columns.keys():
        raise SystemExit("game_event_creature must contain guid and event columns")
    event_associations = {}
    for row in rows(args.source, "game_event_creature"):
        guid = int(row[event_columns["guid"]])
        event_id = int(row[event_columns["event"]])
        if event_id:
            event_associations.setdefault(guid, set()).add(event_id)

    template_columns = {
        name.lower(): index
        for index, name in enumerate(table_columns(args.source, "creature_template"))
    }
    template_fields = {
        "entry",
        "name",
        "subname",
        "minlevel",
        "displayid1",
        "faction",
        "scale",
        "creaturetype",
        "npcflags",
    }
    if missing := template_fields - template_columns.keys():
        raise SystemExit(f"creature_template is missing columns: {sorted(missing)}")

    spawns = []
    entries = set()
    movement_types = {}
    excluded_event_guids = set()
    excluded_events = {}
    for row in rows(args.source, "creature"):
        guid, entry, map_id, _mask, x, y, z, orientation, respawn_min, respawn_max, distance, movement = row
        if not int(entry):
            continue
        associated_events = event_associations.get(int(guid), set())
        positive_events = {event_id for event_id in associated_events if event_id > 0}
        negative_events = {event_id for event_id in associated_events if event_id < 0}
        # Positive rows exist only during an event; negative rows mark a baseline spawn removed by
        # that event. If both signs are present, the negative association keeps the baseline copy.
        if positive_events and not negative_events:
            excluded_event_guids.add(int(guid))
            for event_id in positive_events:
                excluded_events[event_id] = excluded_events.get(event_id, 0) + 1
            continue
        spawns.append((guid, entry, map_id, x, y, z, orientation, respawn_min, respawn_max, distance, movement))
        entries.add(entry)
        movement_types[guid] = int(movement)

    templates = {}
    for row in rows(args.source, "creature_template"):
        entry = row[template_columns["entry"]]
        if entry in entries:
            templates[entry] = row

    missing = sorted(entries - templates.keys(), key=int)
    if missing:
        raise SystemExit(f"missing creature templates for entries: {', '.join(missing)}")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8") as output:
        output.write(f"schema_version = {SCHEMA_VERSION}\n")
        output.write(f'source = "{SOURCE}"\n')
        output.write(f'revision = "{REVISION}"\n')
        output.write('license = "GPL-3.0"\n\n')
        output.write(f'event_policy = "{EVENT_POLICY}"\n\n')
        for entry in sorted(entries, key=int):
            row = templates[entry]
            output.write("[[template]]\n")
            output.write(f"entry = {entry}\n")
            output.write(f'name = {json.dumps(row[template_columns["name"]])}\n')
            output.write(f'subname = {json.dumps(row[template_columns["subname"]] or "")}\n')
            output.write(f'level = {row[template_columns["minlevel"]]}\n')
            output.write(f'display_id = {row[template_columns["displayid1"]]}\n')
            output.write(f'faction = {row[template_columns["faction"]]}\n')
            output.write(f'scale = {row[template_columns["scale"]]}\n')
            output.write(f'creature_type = {row[template_columns["creaturetype"]]}\n')
            output.write(f'npc_flags = {row[template_columns["npcflags"]]}\n\n')
        for row in sorted(spawns, key=lambda spawn: int(spawn[0])):
            guid, entry, map_id, x, y, z, orientation, respawn_min, respawn_max, distance, movement = row
            output.write("[[spawn]]\n")
            output.write(f"source_guid = {guid}\nentry = {entry}\nmap_id = {map_id}\n")
            output.write(f"x = {x}\ny = {y}\nz = {z}\norientation = {orientation}\n")
            output.write(f"respawn_min_secs = {respawn_min}\nrespawn_max_secs = {respawn_max}\n")
            output.write(f"wander_radius = {distance}\nmovement_type = {movement}\n\n")

        spawn_ids = set(movement_types)
        waypoints = {}
        for row in rows(args.source, "creature_movement"):
            source_guid, point, x, y, z, orientation, wait_ms, _script_id, _comment = row
            if source_guid in spawn_ids and movement_types[source_guid] == 2:
                waypoints.setdefault(source_guid, []).append(
                    (int(point), x, y, z, orientation, wait_ms)
                )
        for source_guid in sorted(waypoints, key=int):
            for point, x, y, z, orientation, wait_ms in sorted(
                waypoints[source_guid], key=lambda waypoint: waypoint[0]
            ):
                output.write("[[waypoint]]\n")
                output.write(f"source_guid = {source_guid}\npoint = {point}\n")
                output.write(f"x = {x}\ny = {y}\nz = {z}\norientation = {orientation}\n")
                output.write(f"wait_ms = {wait_ms}\n\n")

        addons = []
        for row in rows(args.source, "creature_addon"):
            source_guid, _mount, stand_state, sheath_state, emote, _moveflags, _auras = row
            if source_guid not in spawn_ids:
                continue
            stand_state, sheath_state, emote = int(stand_state), int(sheath_state), int(emote)
            if not (stand_state or sheath_state or emote):
                continue
            addons.append((int(source_guid), stand_state, sheath_state, emote))
        for source_guid, stand_state, sheath_state, emote in sorted(addons):
            output.write("[[addon]]\n")
            output.write(f"source_guid = {source_guid}\nstand_state = {stand_state}\n")
            output.write(f"sheath_state = {sheath_state}\nemote = {emote}\n\n")
        output.seek(output.tell() - 1)
        output.truncate()

    event_summary = ", ".join(
        f"{event_id}={count}" for event_id, count in sorted(excluded_events.items())
    ) or "none"
    print(
        f"wrote {len(templates)} templates, {len(spawns)} spawns, "
        f"{sum(map(len, waypoints.values()))} waypoints and creature addons to {args.output}; "
        f"excluded {len(excluded_event_guids)} event-only spawns ({event_summary})"
    )


if __name__ == "__main__":
    main()
