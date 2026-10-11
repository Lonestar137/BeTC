#!/usr/bin/env python3
"""Convert selected CMaNGOS TBC creature rows into the offline runtime content pack.

The source SQL stays outside the repository. This intentionally understands only MySQL bulk INSERT
rows for creature_template, creature, creature_movement and creature_addon; it is not a general SQL
parser.
"""

import argparse
import json
import pathlib


SOURCE = "https://github.com/cmangos/tbc-db"
REVISION = "866723612d93791e442fb1b18d642118fa682edd"
SCHEMA_VERSION = 2
ZONES = {
    "sunstrider": (530, 9_400.0, 11_300.0, -7_400.0, -5_300.0),
    "northshire": (0, -9_300.0, -8_600.0, -700.0, 500.0),
}


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
    with path.open(encoding="utf-8", errors="replace") as source:
        for line in source:
            if line.startswith(prefix):
                for row in tuples(line[len(prefix) :].rstrip().rstrip(";")):
                    yield fields(row)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path, help="CMaNGOS full SQL dump")
    parser.add_argument("output", type=pathlib.Path, help="generated TOML content pack")
    parser.add_argument("--zone", choices=ZONES, action="append")
    args = parser.parse_args()

    bounds = [ZONES[zone] for zone in (args.zone or ZONES)]
    spawns = []
    entries = set()
    movement_types = {}
    for row in rows(args.source, "creature"):
        guid, entry, map_id, _mask, x, y, z, orientation, respawn_min, respawn_max, distance, movement = row
        if not int(entry):
            continue
        if any(
            int(map_id) == zone_map and min_x <= float(x) <= max_x and min_y <= float(y) <= max_y
            for zone_map, min_x, max_x, min_y, max_y in bounds
        ):
            spawns.append((guid, entry, map_id, x, y, z, orientation, respawn_min, respawn_max, distance, movement))
            entries.add(entry)
            movement_types[guid] = int(movement)

    templates = {}
    for row in rows(args.source, "creature_template"):
        entry = row[0]
        if entry in entries:
            templates[entry] = row

    missing = sorted(entries - templates.keys(), key=int)
    if missing:
        raise SystemExit(f"missing creature templates for entries: {', '.join(missing)}")

    # The pinned dump keeps vendor stock in `npc_vendor` and does not carry
    # `UNIT_NPC_FLAG_VENDOR` (0x4) in `creature_template.NpcFlags`: verified against the dump,
    # none of the zone vendors' flags have the bit. The server derives the service from the
    # `npc_vendor` join at load, so the converter ORs the bit in from the same join; without it
    # the client's service ladder never offers Buy and an innkeeper arm wins instead.
    vendor_entries = {row[0] for row in rows(args.source, "npc_vendor")}

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8") as output:
        output.write(f"schema_version = {SCHEMA_VERSION}\n")
        output.write(f'source = "{SOURCE}"\n')
        output.write(f'revision = "{REVISION}"\n')
        output.write('license = "GPL-3.0"\n\n')
        for entry in sorted(entries, key=int):
            row = templates[entry]
            output.write("[[template]]\n")
            output.write(f"entry = {entry}\n")
            output.write(f"name = {json.dumps(row[1])}\n")
            output.write(f"subname = {json.dumps(row[2])}\n")
            output.write(f"level = {row[4]}\n")
            output.write(f"display_id = {row[7]}\n")
            output.write(f"faction = {row[15]}\n")
            output.write(f"scale = {row[16]}\n")
            output.write(f"creature_type = {row[18]}\n")
            npc_flags = int(row[22])
            if entry in vendor_entries:
                npc_flags |= 0x4  # UNIT_NPC_FLAG_VENDOR, from the npc_vendor join above
            output.write(f"npc_flags = {npc_flags}\n\n")
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

    print(
        f"wrote {len(templates)} templates, {len(spawns)} spawns, "
        f"{sum(map(len, waypoints.values()))} waypoints and creature addons to {args.output}"
    )


if __name__ == "__main__":
    main()
