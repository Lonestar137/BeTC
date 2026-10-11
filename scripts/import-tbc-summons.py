#!/usr/bin/env python3
"""Convert the summonable-pet creature rows into the offline runtime's summon content pack.

The source SQL stays outside the repository. This intentionally understands only MySQL bulk INSERT
rows for creature_template and creature_template_spells; it is not a general SQL parser.

The pack's `[[grant]]` section is AUTHORED, not converted: stock TBC teaches a warlock its demons
(Imp at 1, Voidwalker at 10, Succubus at 20, Felhunter at 30) and the pinned Vengeance-One DB
expresses none of those acquisitions in `playercreateinfo_spell`, `npc_trainer`,
`npc_trainer_template` or `spell_learn_spell` (verified against the pinned dump: the pet spell ids
appear zero times in `npc_trainer`, while a control spell appears). The grants restore the stock
spellbook so the class plays as stock TBC; every grant's note says so.

The `swing = true` marker is AUTHORED too: it names the guardian creatures (the mage's Water
Elemental, the priest's Shadowfiend, the druid's Treants, and the elemental totems' fighters)
whose pet AI is keyed nowhere the conversion reads. A marked creature swings the creature
baseline (`CREATURE_DAMAGE` per `CREATURE_SWING_MS`) like every creature in the simulation; its
converted template still supplies identity, and its level-scaled pet damage is the named fidelity.
"""

import argparse
import json
import pathlib


SOURCE = "https://github.com/cmangos/tbc-db"
REVISION = "866723612d93791e442fb1b18d642118fa682edd"
SCHEMA_VERSION = 1

# The creature entries the `SPELL_EFFECT_SUMMON` and `SPELL_EFFECT_SUMMON_TOTEM` rows summon: the
# warlock demons, every totem the shaman trainers and talents teach (all ranks), and the guardian
# creatures the other classes' talent and trainer rows summon. Pinned from the build-8606 rows
# (Spell.dbc's `EffectMiscValue`), so a new rank is added here when its row is first audited.
ENTRIES = (
    # The warlock demons (SPELL_EFFECT_SUMMON).
    "416", "417", "17252", "1860", "1863",
    # The shaman totems (SPELL_EFFECT_SUMMON_TOTEM), every trainer rank.
    "2630", "3573", "3579", "3902", "3903", "3904", "3906", "3907", "3908", "3909",
    "3911", "3912", "3913", "3968", "5874", "5879", "5913", "5919", "5920", "5921",
    "5922", "5923", "5924", "5925", "5926", "5927", "5929", "5950", "6012", "6110",
    "6111", "6112", "7366", "7367", "7368", "7398", "7399", "7400", "7402", "7403",
    "7412", "7413", "7414", "7415", "7416", "7423", "7424", "7425", "7464", "7465",
    "7466", "7467", "7468", "7469", "7483", "7484", "7486", "7487", "7844", "7845",
    "9687", "9688", "9689", "10557", "15430", "15439", "15447", "15463", "15464",
    "15470", "15474", "15478", "15479", "15480", "15482", "15483", "15484", "15485",
    "15486", "15487", "15488", "15489", "15490", "15492", "15496", "15497", "15803",
    # The shaman talents' totems.
    "10467", "17539",
    # The other classes' guardians and observers (Water Elemental, Treants, Shadowfiend, the Eye
    # of Kilrogg).
    "510", "1964", "19668", "4277",
)

# The guardian creatures whose pet AI keys no converted table (see the docstring): they swing the
# creature baseline.
SWING = ("510", "1964", "19668", "15430", "15439")

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

    # Each summoned creature's own cast list (`creature_template_spells`): the totem's passive
    # rows, which the runtime folds into the totem's buff and pulse.
    casts = {}
    for row in rows(args.source, "creature_template_spells"):
        if row[0] in ENTRIES:
            casts.setdefault(row[0], [])
            casts[row[0]].extend(spell for spell in row[2:] if spell and spell != "0")

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
            "authored = \"[[grant]] rows restore the stock-TBC warlock demon spellbook and "
            "swing = true names the guardian creatures whose pet AI keys no converted table; "
            "the pinned DB expresses neither (see the script's docstring)\"\n\n"
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
            output.write(f"health = {row[53]}\n")
            spells = casts.get(entry, [])
            if spells:
                output.write(f"spells = [{', '.join(spells)}]\n")
            if entry in SWING:
                output.write("swing = true\n")
            output.write("\n")
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
