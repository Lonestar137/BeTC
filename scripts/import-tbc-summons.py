#!/usr/bin/env python3
"""Convert the summonable-pet creature rows into the offline runtime's summon content pack.

The source SQL stays outside the repository. This intentionally understands only MySQL bulk INSERT
rows for creature_template; it is not a general SQL parser.

The pack's `[[grant]]` section is AUTHORED, not converted: stock TBC teaches a warlock its demons
(Imp at 1, Voidwalker at 10, Succubus at 20, Felhunter at 30) and the pinned Vengeance-One DB
expresses none of those acquisitions in `playercreateinfo_spell`, `npc_trainer`,
`npc_trainer_template` or `spell_learn_spell` (verified against the pinned dump: the pet spell ids
appear zero times in `npc_trainer`, while a control spell appears). The grants restore the stock
spellbook so the class plays as stock TBC; every grant's note says so.
"""

import argparse
import json
import pathlib


SOURCE = "https://github.com/cmangos/tbc-db"
REVISION = "866723612d93791e442fb1b18d642118fa682edd"
SCHEMA_VERSION = 1

# The creature entries the `SPELL_EFFECT_SUMMON` rows summon: the warlock demons.
ENTRIES = ("416", "1860", "1863", "417", "17252")

# Authored stock-TBC warlock grants: spell id -> (stock level, note).
GRANTS = (
    ("688", 1, "Summon Imp"),
    ("697", 10, "Summon Voidwalker"),
    ("712", 20, "Summon Succubus"),
    ("691", 30, "Summon Felhunter"),
)
GRANT_CLASSES = ("9",)


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


def value(source):
    source = source.strip()
    if source == "NULL":
        return ""
    if source.startswith("'") and source.endswith("'"):
        return source[1:-1].replace("\\'", "'").replace("\\\\", "\\")
    return source


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
    args = parser.parse_args()

    templates = {}
    for row in rows(args.source, "creature_template"):
        if row[0] in ENTRIES:
            templates[row[0]] = row
    missing = sorted(set(ENTRIES) - templates.keys(), key=int)
    if missing:
        raise SystemExit(f"missing creature templates for entries: {', '.join(missing)}")

    # Every race that has create-info rows for a granted class, so one grant per (race, class).
    races_by_class = {cls: set() for cls in GRANT_CLASSES}
    for row in rows(args.source, "playercreateinfo_spell"):
        if len(row) > 1 and row[1] in races_by_class:
            races_by_class[row[1]].add(row[0])

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8") as output:
        output.write(f"schema_version = {SCHEMA_VERSION}\n")
        output.write(f'source = "{SOURCE}"\n')
        output.write(f'revision = "{REVISION}"\n')
        output.write('license = "GPL-3.0"\n')
        output.write("client_build = 8606\n")
        output.write(
            'extraction_command = "python3 scripts/import-tbc-summons.py '
            '<TBC-DB full SQL dump> crates/benilla-game-core/content/tbc-summons.toml"\n'
            "authored = \"[[grant]] rows restore the stock-TBC warlock demon spellbook; the "
            "pinned DB teaches none of them (see the script's docstring)\"\n\n"
        )
        for entry in sorted(templates, key=int):
            row = templates[entry]
            output.write("[[summon]]\n")
            output.write(f"entry = {entry}\n")
            output.write(f"name = {json.dumps(row[1])}\n")
            output.write(f"display_id = {row[7]}\n")
            output.write(f"faction = {row[15]}\n")
            output.write(f"scale = {float(row[16])!r}\n")
            output.write(f"creature_type = {row[18]}\n")
            output.write(f"family = {row[17]}\n")
            output.write(f"health = {row[53]}\n\n")
        for cls in GRANT_CLASSES:
            for race in sorted(races_by_class[cls], key=int):
                for spell, level, note in GRANTS:
                    output.write("[[grant]]\n")
                    output.write(f"race = {race}\nclass = {cls}\n")
                    output.write(f"spell_id = {spell}\n")
                    output.write(
                        f'note = "Summon (authored: stock TBC level-{level} {note}; '
                        'the pinned DB teaches no demon)"\n\n'
                    )


if __name__ == "__main__":
    main()
