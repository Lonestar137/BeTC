#!/usr/bin/env python3
"""Convert CMaNGOS TBC starter spells and trainer requirements to local TOML.

The converter reads the full SQL dump from the pinned TBC-DB revision. It supports
the dump's INSERT ... VALUES format and validates the source DDL before reading rows.
"""

import argparse
import json
import pathlib
import re


SOURCE = "https://github.com/cmangos/tbc-db"
REVISION = "866723612d93791e442fb1b18d642118fa682edd"
LICENSE = "GPL-3.0"
SCHEMA_VERSION = 1
CLASS_IDS = {1, 2, 3, 4, 5, 7, 8, 9, 11}
RACE_IDS = {1, 2, 3, 4, 5, 6, 7, 8, 10, 11}
TABLES = {
    "creature_template",
    "npc_trainer",
    "npc_trainer_template",
    "playercreateinfo_action",
    "playercreateinfo_spell",
    "spell_chain",
}
EXPECTED_COLUMNS = {
    "npc_trainer": [
        "entry", "spell", "spellcost", "reqskill", "reqskillvalue", "reqlevel",
        "ReqAbility1", "ReqAbility2", "ReqAbility3", "condition_id",
    ],
    "npc_trainer_template": [
        "entry", "spell", "spellcost", "reqskill", "reqskillvalue", "reqlevel",
        "ReqAbility1", "ReqAbility2", "ReqAbility3", "condition_id",
    ],
    "playercreateinfo_action": ["race", "class", "button", "action", "type"],
    "playercreateinfo_spell": ["race", "class", "Spell", "Note"],
    "spell_chain": ["spell_id", "prev_spell", "first_spell", "rank", "req_spell"],
}
INSERT_START_RE = re.compile(r"^INSERT INTO `([a-z_]+)`")
INSERT_RE = re.compile(r"^INSERT INTO `([a-z_]+)`(?: \(([^)]*)\))? VALUES (.*)$")
CREATE_RE = re.compile(r"^CREATE TABLE `([a-z_]+)`")
COLUMN_RE = re.compile(r"^\s*`([^`]+)`\s+")


class ImportError(Exception):
    pass


def source_schemas(path):
    schemas = {}
    active = None
    columns = []
    with path.open(encoding="utf-8", errors="strict") as source:
        for line_number, line in enumerate(source, 1):
            match = CREATE_RE.match(line)
            if match:
                active = match.group(1) if match.group(1) in TABLES else None
                columns = []
                continue
            if active is None:
                continue
            column = COLUMN_RE.match(line)
            if column:
                columns.append(column.group(1))
            if line.lstrip().startswith(")"):
                if active in schemas:
                    raise ImportError(f"line {line_number}: duplicate CREATE TABLE for {active}")
                schemas[active] = columns
                active = None

    missing = sorted(TABLES - schemas.keys())
    if missing:
        raise ImportError(f"source is missing CREATE TABLE definitions: {', '.join(missing)}")
    for table, expected in EXPECTED_COLUMNS.items():
        if schemas[table] != expected:
            raise ImportError(
                f"unsupported {table} columns: expected {expected!r}, found {schemas[table]!r}"
            )
    for column in ("Entry", "TrainerClass", "TrainerTemplateId"):
        if column not in schemas["creature_template"]:
            raise ImportError(f"unsupported creature_template layout: missing column {column}")
    return schemas


def tuple_values(source):
    start = None
    previous_end = 0
    quoted = False
    escaped = False
    depth = 0
    for index, char in enumerate(source):
        if escaped:
            escaped = False
        elif char == "\\" and quoted:
            escaped = True
        elif char == "'":
            quoted = not quoted
        elif not quoted and char == "(":
            if depth == 0:
                separator = source[previous_end:index].strip()
                expected = "" if previous_end == 0 else ","
                if separator != expected:
                    raise ImportError("unexpected text between SQL tuples")
                start = index + 1
            depth += 1
        elif not quoted and char == ")":
            depth -= 1
            if depth < 0:
                raise ImportError("unexpected closing parenthesis in INSERT values")
            if depth == 0:
                yield source[start:index]
                previous_end = index + 1
    if quoted or depth:
        raise ImportError("unterminated string or tuple in INSERT values")
    if previous_end == 0 or source[previous_end:].strip():
        raise ImportError("malformed trailing text in INSERT values")


def decode_value(raw):
    raw = raw.strip()
    if raw == "NULL":
        return None
    if not raw.startswith("'"):
        return raw
    if len(raw) < 2 or not raw.endswith("'"):
        raise ImportError(f"malformed quoted SQL value: {raw!r}")
    escapes = {"0": "\0", "b": "\b", "n": "\n", "r": "\r", "t": "\t", "Z": "\x1a"}
    value = []
    index = 1
    while index < len(raw) - 1:
        char = raw[index]
        if char == "\\":
            index += 1
            if index >= len(raw) - 1:
                raise ImportError(f"malformed SQL escape: {raw!r}")
            char = escapes.get(raw[index], raw[index])
        value.append(char)
        index += 1
    return "".join(value)


def split_fields(source):
    result = []
    start = 0
    quoted = False
    escaped = False
    for index, char in enumerate(source):
        if escaped:
            escaped = False
        elif char == "\\" and quoted:
            escaped = True
        elif char == "'":
            quoted = not quoted
        elif char == "," and not quoted:
            result.append(decode_value(source[start:index]))
            start = index + 1
    if quoted or escaped:
        raise ImportError("unterminated string in SQL row")
    result.append(decode_value(source[start:]))
    return result


def table_rows(path, schemas):
    seen = set()
    with path.open(encoding="utf-8", errors="strict") as source:
        for line_number, line in enumerate(source, 1):
            stripped_line = line.rstrip("\r\n")
            start = INSERT_START_RE.match(stripped_line)
            if not start or start.group(1) not in TABLES:
                continue
            match = INSERT_RE.match(stripped_line)
            if not match:
                raise ImportError(f"line {line_number}: unsupported INSERT layout for {start.group(1)}")
            table, explicit_columns, values = match.groups()
            if explicit_columns is not None:
                raise ImportError(f"line {line_number}: INSERT column lists are unsupported for {table}")
            if not values.rstrip().endswith(";"):
                raise ImportError(f"line {line_number}: incomplete INSERT statement for {table}")
            seen.add(table)
            body = values.rstrip()[:-1]
            for tuple_source in tuple_values(body):
                row = split_fields(tuple_source)
                if len(row) != len(schemas[table]):
                    raise ImportError(
                        f"line {line_number}: {table} row has {len(row)} values, "
                        f"expected {len(schemas[table])} from its pinned DDL"
                    )
                yield table, line_number, dict(zip(schemas[table], row))
    missing = sorted(TABLES - seen)
    if missing:
        raise ImportError(f"source is missing INSERT data for: {', '.join(missing)}")


def integer(row, column, table, line, nullable=False):
    value = row[column]
    if value is None and nullable:
        return 0
    try:
        number = int(value)
    except (TypeError, ValueError) as error:
        raise ImportError(f"line {line}: {table}.{column} is not an integer: {value!r}") from error
    return number


def convert(path):
    schemas = source_schemas(path)
    starter_spells = []
    actions = []
    trainers = []
    chains = {}
    creature_templates = {}
    npc_trainer_rows = []
    spell_ids = set()
    keys = {table: set() for table in TABLES}

    for table, line, row in table_rows(path, schemas):
        if table == "playercreateinfo_spell":
            race = integer(row, "race", table, line)
            class_id = integer(row, "class", table, line)
            spell_id = integer(row, "Spell", table, line)
            if race not in RACE_IDS or class_id not in CLASS_IDS or spell_id <= 0:
                raise ImportError(f"line {line}: invalid playercreateinfo_spell race/class/spell")
            key = (race, class_id, spell_id)
            if key in keys[table]:
                raise ImportError(f"line {line}: duplicate playercreateinfo_spell key {key}")
            keys[table].add(key)
            note = row["Note"]
            if note is not None and not isinstance(note, str):
                raise ImportError(f"line {line}: playercreateinfo_spell.Note is not text")
            starter_spells.append((race, class_id, spell_id, note or ""))
            spell_ids.add(spell_id)
        elif table == "playercreateinfo_action":
            race = integer(row, "race", table, line)
            class_id = integer(row, "class", table, line)
            button = integer(row, "button", table, line)
            action = integer(row, "action", table, line)
            action_type = integer(row, "type", table, line)
            if race not in RACE_IDS or class_id not in CLASS_IDS or button < 0 or action < 0:
                raise ImportError(f"line {line}: invalid playercreateinfo_action row")
            key = (race, class_id, button)
            if key in keys[table]:
                raise ImportError(f"line {line}: duplicate playercreateinfo_action key {key}")
            keys[table].add(key)
            actions.append((race, class_id, button, action, action_type))
        elif table == "creature_template":
            entry = integer(row, "Entry", table, line)
            class_id = integer(row, "TrainerClass", table, line)
            template_id = integer(row, "TrainerTemplateId", table, line)
            if entry <= 0 or class_id < 0 or template_id < 0:
                raise ImportError(f"line {line}: invalid creature_template trainer linkage")
            if entry in creature_templates:
                raise ImportError(f"line {line}: duplicate creature_template entry {entry}")
            creature_templates[entry] = (class_id, template_id)
        elif table in {"npc_trainer", "npc_trainer_template"}:
            key = (integer(row, "entry", table, line), integer(row, "spell", table, line))
            if key in keys[table]:
                raise ImportError(f"line {line}: duplicate npc_trainer key {key}")
            keys[table].add(key)
            record = {column: row[column] for column in EXPECTED_COLUMNS[table]}
            record["_line"] = line
            record["_table"] = table
            for column in ("entry", "spell", "spellcost", "reqskill", "reqskillvalue", "reqlevel", "condition_id"):
                record[column] = integer(row, column, table, line, nullable=True)
            for column in ("ReqAbility1", "ReqAbility2", "ReqAbility3"):
                record[column] = integer(row, column, table, line, nullable=True)
            if record["spell"] <= 0 or min(record["spellcost"], record["reqskill"], record["reqskillvalue"], record["reqlevel"], record["condition_id"]) < 0:
                raise ImportError(f"line {line}: invalid npc_trainer requirement")
            npc_trainer_rows.append(record)
            spell_ids.add(record["spell"])
        else:
            spell_id = integer(row, "spell_id", table, line)
            if spell_id <= 0 or spell_id in chains:
                raise ImportError(f"line {line}: invalid or duplicate spell_chain spell_id {spell_id}")
            chain = {
                name: integer(row, name, table, line)
                for name in ("prev_spell", "first_spell", "rank", "req_spell")
            }
            if chain["rank"] <= 0 or min(chain.values()) < 0:
                raise ImportError(f"line {line}: invalid spell_chain row for spell {spell_id}")
            chains[spell_id] = chain
            spell_ids.add(spell_id)

    for race, class_id, button, action, action_type in actions:
        if action_type == 0 and action not in spell_ids:
            raise ImportError(
                f"unresolved action-bar spell {action} for race {race}, class {class_id}, button {button}"
            )
    for spell_id, chain in chains.items():
        if chain["req_spell"] and chain["req_spell"] not in spell_ids:
            raise ImportError(f"spell_chain spell {spell_id} has unresolved req_spell {chain['req_spell']}")

    trainer_npcs = {}
    trainer_templates = {}
    for npc_entry, (class_id, template_id) in creature_templates.items():
        if class_id == 0:
            continue
        if class_id not in CLASS_IDS:
            raise ImportError(f"creature_template {npc_entry} has unsupported TrainerClass {class_id}")
        trainer_npcs[npc_entry] = [(npc_entry, class_id, template_id)]
        if template_id:
            trainer_templates.setdefault(template_id, []).append((npc_entry, class_id, template_id))

    for trainer_row in npc_trainer_rows:
        links_by_entry = trainer_npcs if trainer_row["_table"] == "npc_trainer" else trainer_templates
        links = links_by_entry.get(trainer_row["entry"], [])
        for npc_entry, class_id, template_id in links:
            prereqs = {
                trainer_row[column]
                for column in ("ReqAbility1", "ReqAbility2", "ReqAbility3")
                if trainer_row[column]
            }
            chain_prereq = chains.get(trainer_row["spell"], {}).get("req_spell", 0)
            if chain_prereq:
                prereqs.add(chain_prereq)
            for prerequisite in prereqs:
                if prerequisite not in spell_ids:
                    raise ImportError(
                        f"npc_trainer spell {trainer_row['spell']} has unresolved prerequisite spell {prerequisite}"
                    )
            trainers.append(
                (
                    class_id,
                    trainer_row["spell"],
                    trainer_row["reqlevel"],
                    trainer_row["reqskill"],
                    trainer_row["reqskillvalue"],
                    trainer_row["spellcost"],
                    sorted(prereqs),
                    npc_entry,
                    template_id,
                    trainer_row["condition_id"],
                )
            )
    unique_trainers = {}
    for trainer in trainers:
        key = (trainer[0], trainer[1], trainer[7])
        previous = unique_trainers.setdefault(key, trainer)
        if previous != trainer:
            raise ImportError(
                f"trainer {trainer[7]} has conflicting requirements for class {trainer[0]} spell {trainer[1]}"
            )
    trainer_groups = {}
    for trainer in unique_trainers.values():
        group_key = (
            trainer[0], trainer[1], trainer[2], trainer[3], trainer[4], trainer[5],
            tuple(trainer[6]), trainer[9],
        )
        group = trainer_groups.setdefault(group_key, {"entries": set(), "templates": set()})
        group["entries"].add(trainer[7])
        if trainer[8]:
            group["templates"].add(trainer[8])
    trainers = sorted(trainer_groups.items())

    output = [
        f"schema_version = {SCHEMA_VERSION}",
        f"source = {json.dumps(SOURCE)}",
        f"revision = {json.dumps(REVISION)}",
        f"license = {json.dumps(LICENSE)}",
        'extraction_command = "python3 scripts/import-tbc-progression.py <TBC-DB full SQL dump> crates/benilla-game-core/content/tbc-progression.toml"',
        "",
    ]
    for race, class_id, spell_id, note in sorted(starter_spells):
        output.extend(
            ["[[starter_spell]]", f"race = {race}", f"class = {class_id}", f"spell_id = {spell_id}"]
        )
        if note:
            output.append(f"note = {json.dumps(note)}")
        output.append("")
    for race, class_id, button, action, action_type in sorted(actions):
        output.extend(
            [
                "[[action_bar]]",
                f"race = {race}",
                f"class = {class_id}",
                f"button = {button}",
                f"action = {action}",
                f"type = {action_type}",
                "",
            ]
        )
    for (class_id, spell_id, level, skill, skill_value, cost, prereqs, condition_id), links in trainers:
        output.extend(
            [
                "[[trainer_spell]]",
                f"class = {class_id}",
                f"spell_id = {spell_id}",
                f"required_level = {level}",
                f"required_skill_id = {skill}",
                f"required_skill_value = {skill_value}",
                f"cost_copper = {cost}",
                f"prerequisite_spell_ids = {json.dumps(prereqs)}",
                f"trainer_entries = {json.dumps(sorted(links['entries']))}",
                f"trainer_template_ids = {json.dumps(sorted(links['templates']))}",
                f"condition_id = {condition_id}",
                "",
            ]
        )
    return "\n".join(output).rstrip() + "\n", len(starter_spells), len(actions), len(trainers)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path, help="pinned CMaNGOS TBC full SQL dump")
    parser.add_argument("output", type=pathlib.Path, help="generated local progression TOML")
    args = parser.parse_args()
    try:
        content, starter_count, action_count, trainer_count = convert(args.source)
    except (OSError, UnicodeError, ImportError) as error:
        parser.error(str(error))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(content, encoding="utf-8", newline="\n")
    print(
        f"wrote {starter_count} starter spells, {action_count} action-bar entries, "
        f"and {trainer_count} trainer offers to {args.output}"
    )


if __name__ == "__main__":
    main()
