#!/usr/bin/env python3
"""Convert the pinned CMaNGOS TBC quest templates into the offline runtime quest pack.

Every ``quest_template`` row is imported with its chain links, prerequisite conditions, objective
slots, reward slots, reputation rewards, and event association; the creature and gameobject
quest-relation tables supply the givers and turn-ins, ``game_event``/``game_event_time`` supply the
seasonal windows, and every referenced item gets an ``item_template`` row in the runtime shape.
``npc_text`` supplies each quest giver's gossip menu text. The source SQL stays outside the
repository. This intentionally understands only MySQL bulk INSERT rows for those tables; it is not
a general SQL parser.
"""

import argparse
import importlib.util
import pathlib
import re

SOURCE = "https://github.com/cmangos/tbc-db"
REVISION = "866723612d93791e442fb1b18d642118fa682edd"
LICENSE = "GPL-3.0"
SCHEMA_VERSION = 2
CLIENT_BUILD = 8606

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
    "dmg_min1",
    "dmg_max1",
    "delay",
)

ITEM_KEYS = {
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
}

class ImportError(Exception):
    pass


def _sql_helpers():
    path = pathlib.Path(__file__).with_name("import-tbc-content.py")
    spec = importlib.util.spec_from_file_location("tbc_content_importer", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def toml_str(value):
    """A TOML basic string. JSON's escapes are close but not TOML's (``\\/``, surrogates)."""
    if value is None:
        value = ""
    out = ['"']
    for char in value:
        code = ord(char)
        if char == '"':
            out.append('\\"')
        elif char == "\\":
            out.append("\\\\")
        elif char == "\n":
            out.append("\\n")
        elif char == "\r":
            out.append("\\r")
        elif char == "\t":
            out.append("\\t")
        elif char == "\b":
            out.append("\\b")
        elif char == "\f":
            out.append("\\f")
        elif code < 0x20 or code == 0x7F:
            out.append(f"\\u{code:04X}")
        elif code > 0xFFFF:
            out.append(f"\\U{code:08X}")
        else:
            out.append(char)
    out.append('"')
    return "".join(out)


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


def index_of(columns, *names):
    for name in names:
        if name in columns:
            return columns.index(name)
    raise ImportError(f"missing columns {names} in {columns}")


def relations(helpers, source_path, table):
    columns = table_columns(source_path, table, helpers.open_source)
    entry_index = index_of(columns, "id", "entry")
    quest_index = index_of(columns, "quest")
    result = {}
    for row in helpers.rows(source_path, table):
        entry = int(row[entry_index])
        quest = int(row[quest_index])
        result.setdefault(quest, []).append(entry)
    return result


def parse_timestamp(value):
    match = re.match(r"(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2}):(\d{2})", value)
    if not match:
        raise ImportError(f"unsupported game_event_time timestamp {value!r}")
    _, month, day, hour, minute, _ = (int(part) for part in match.groups())
    return month, day, hour * 60 + minute


def quest_condition_closure(helpers, source_path, roots):
    columns = table_columns(source_path, "conditions", helpers.open_source)
    entry_index = index_of(columns, "condition_entry")
    kind_index = index_of(columns, "type")
    value_indexes = [index_of(columns, f"value{i}") for i in range(1, 5)]
    flag_index = index_of(columns, "flags")
    comment_index = index_of(columns, "comments")
    rows = {}
    for row in helpers.rows(source_path, "conditions"):
        rows[int(row[entry_index])] = row
    pending = [root for root in roots if root]
    seen = {}
    while pending:
        entry = pending.pop()
        if entry in seen:
            continue
        row = rows.get(entry)
        if row is None:
            raise ImportError(f"quest references missing condition {entry}")
        kind = int(row[kind_index])
        values = [int(row[index]) for index in value_indexes]
        seen[entry] = (
            kind,
            values[0],
            values[1],
            values[2],
            values[3],
            int(row[flag_index]),
            row[comment_index],
        )
        if kind in (-1, -2, -3):
            pending.extend(value for value in values[:2] if value)
    return seen


def collect_item_ids(helpers, source_path, quests):
    ids = set()
    for quest in quests:
        for i in range(1, 5):
            for key in (f"ReqItemId{i}", f"ReqSourceId{i}", f"RewItemId{i}"):
                item = int(quest[key])
                if item:
                    ids.add(item)
        for i in range(1, 7):
            item = int(quest[f"RewChoiceItemId{i}"])
            if item:
                ids.add(item)
    return ids


def convert(source_path):
    helpers = _sql_helpers()
    open_source = helpers.open_source

    quest_columns = table_columns(source_path, "quest_template", open_source)
    quest_index = {name: index for index, name in enumerate(quest_columns)}
    quests = []
    for row in helpers.rows(source_path, "quest_template"):
        quests.append({name: row[index] for name, index in quest_index.items()})

    creature_givers = relations(helpers, source_path, "creature_questrelation")
    creature_turnins = relations(helpers, source_path, "creature_involvedrelation")
    object_givers = relations(helpers, source_path, "gameobject_questrelation")
    object_turnins = relations(helpers, source_path, "gameobject_involvedrelation")

    event_columns = table_columns(source_path, "game_event", open_source)
    event_index = {name: index for index, name in enumerate(event_columns)}
    event_times = {}
    for row in helpers.rows(source_path, "game_event_time"):
        event_times[int(row[0])] = parse_timestamp(row[1])
    events = {}
    for row in helpers.rows(source_path, "game_event"):
        entry = int(row[event_index["entry"]])
        events[entry] = {
            "entry": entry,
            "schedule_type": int(row[event_index["schedule_type"]]),
            "occurrence_minutes": int(row[event_index["occurence"]]),
            "length_minutes": int(row[event_index["length"]]),
            "holiday": int(row[event_index["holiday"]]),
            "description": row[event_index["description"]],
            "start_month": 0,
            "start_day": 0,
            "start_minute": 0,
        }
        if entry in event_times:
            month, day, minute = event_times[entry]
            events[entry]["start_month"] = month
            events[entry]["start_day"] = day
            events[entry]["start_minute"] = minute

    event_quests = {}
    for row in helpers.rows(source_path, "game_event_quest"):
        quest = int(row[0])
        event = int(row[1])
        if event not in events:
            raise ImportError(f"quest {quest} references missing game event {event}")
        event_quests.setdefault(quest, []).append(event)

    quest_ids = {int(quest["entry"]) for quest in quests}
    for table, mapping in (
        ("creature_questrelation", creature_givers),
        ("creature_involvedrelation", creature_turnins),
        ("gameobject_questrelation", object_givers),
        ("gameobject_involvedrelation", object_turnins),
    ):
        unknown = {quest for quest in mapping if quest not in quest_ids}
        if unknown:
            raise ImportError(f"{table} references unknown quests {sorted(unknown)[:5]}")

    condition_roots = {
        int(quest["RequiredCondition"]) for quest in quests if int(quest["RequiredCondition"])
    }
    conditions = quest_condition_closure(helpers, source_path, condition_roots)

    item_ids = collect_item_ids(helpers, source_path, quests)
    item_columns = table_columns(source_path, "item_template", open_source)
    missing_fields = (
        set(ITEM_FIELDS) | set(helpers.ITEM_SPELL_COLUMNS)
    ) - set(item_columns)
    if missing_fields:
        raise ImportError(f"unsupported item_template columns: missing {sorted(missing_fields)}")
    item_index = {name: index for index, name in enumerate(item_columns)}
    items = {}
    for row in helpers.rows(source_path, "item_template"):
        entry = int(row[item_index["entry"]])
        if entry in item_ids:
            items[entry] = {name: row[item_index[name]] for name in ITEM_FIELDS}
            items[entry]["_spells"] = helpers.item_spell_blocks(row, item_index)
    missing_items = item_ids - items.keys()
    if missing_items:
        raise ImportError(f"source is missing item templates: {sorted(missing_items)[:5]}")

    gossip = import_gossip(helpers, source_path, open_source)
    greetings = import_greetings(helpers, source_path)

    output = []
    output.append(f"schema_version = {SCHEMA_VERSION}")
    output.append(f"source = {toml_str(SOURCE)}")
    output.append(f"revision = {toml_str(REVISION)}")
    output.append(f"license = {toml_str(LICENSE)}")
    output.append("conversion_version = 2")
    output.append(f"client_build = {CLIENT_BUILD}")
    output.append(
        "extraction_command = "
        + toml_str(
            "python3 scripts/import-tbc-quests.py <TBC-DB full SQL dump> "
            "crates/benilla-game-core/content/tbc-quests.toml"
        )
    )
    output.append("")

    for entry in sorted(events):
        event = events[entry]
        output.extend(
            [
                "[[game_event]]",
                f"entry = {event['entry']}",
                f"schedule_type = {event['schedule_type']}",
                f"occurrence_minutes = {event['occurrence_minutes']}",
                f"length_minutes = {event['length_minutes']}",
                f"holiday = {event['holiday']}",
                f"start_month = {event['start_month']}",
                f"start_day = {event['start_day']}",
                f"start_minute = {event['start_minute']}",
                f"description = {toml_str(event['description'])}",
                "",
            ]
        )

    for quest in sorted(quests, key=lambda quest: int(quest["entry"])):
        output.extend(write_quest(quest, creature_givers, creature_turnins, object_givers,
                                  object_turnins, event_quests))
    for entry in sorted(gossip):
        row = gossip[entry]
        output.extend(
            [
                "[[gossip]]",
                f"creature_entry = {entry}",
                f"menu_id = {row['menu_id']}",
                f"text_id = {row['text_id']}",
                f"male = {toml_str(row['male'])}",
                f"female = {toml_str(row['female'])}",
                f"probability = {row['probability']}",
                "",
            ]
        )
    for entry in sorted(greetings):
        for row in greetings[entry]:
            output.extend(
                [
                    "[[greeting]]",
                    f"creature_entry = {entry}",
                    f"kind = {row['kind']}",
                    f"text = {toml_str(row['text'])}",
                    f"emote_id = {row['emote_id']}",
                    f"emote_delay = {row['emote_delay']}",
                    "",
                ]
            )
    for entry in sorted(conditions):
        kind, value1, value2, value3, value4, flags, comment = conditions[entry]
        output.extend(
            [
                "[[condition]]",
                f"id = {entry}",
                f"kind = {kind}",
                f"value1 = {value1}",
                f"value2 = {value2}",
                f"value3 = {value3}",
                f"value4 = {value4}",
                f"flags = {flags}",
                f"comment = {toml_str(comment)}",
                "",
            ]
        )
    for entry in sorted(items):
        output.extend(["[[item_template]]", f"entry = {entry}"])
        for name in ITEM_FIELDS[1:]:
            key = ITEM_KEYS.get(name, name.lower())
            value = items[entry][name]
            if name == "name":
                output.append(f"{key} = {toml_str(value)}")
            elif name in ("dmg_min1", "dmg_max1"):
                output.append(f"{key} = {int(round(float(value)))}")
            else:
                output.append(f"{key} = {int(value)}")
        output.extend(helpers.write_item_spells(items[entry]["_spells"]))
        output.append("")

    output = output[:-1]
    return "\n".join(output) + "\n", {
        "quests": len(quests),
        "events": len(events),
        "conditions": len(conditions),
        "gossip": len(gossip),
        "greetings": sum(len(rows) for rows in greetings.values()),
        "items": len(items),
    }


def objective_kind(creature_or_go, spell):
    if creature_or_go > 0:
        return "creature_spellcast" if spell else "creature_kill"
    if creature_or_go < 0:
        return "gameobject_spellcast" if spell else "gameobject_use"
    return "spell"


def write_quest(quest, creature_givers, creature_turnins, object_givers, object_turnins,
                event_quests):
    quest_id = int(quest["entry"])
    output = ["[[quest]]", f"id = {quest_id}"]
    output.append(f"title = {toml_str(quest['Title'])}")
    output.append(f"method = {int(quest['Method'])}")
    output.append(f"zone_or_sort = {int(quest['ZoneOrSort'])}")
    output.append(f"min_level = {int(quest['MinLevel'])}")
    output.append(f"max_level = {int(quest['MaxLevel'])}")
    output.append(f"quest_level = {int(quest['QuestLevel'])}")
    output.append(f"quest_type = {int(quest['Type'])}")
    output.append(f"quest_flags = {int(quest['QuestFlags'])}")
    output.append(f"special_flags = {int(quest['SpecialFlags'])}")
    output.append(f"required_race_mask = {int(quest['RequiredRaces'])}")
    output.append(f"required_class_mask = {int(quest['RequiredClasses'])}")
    output.append(f"required_skill = {int(quest['RequiredSkill'])}")
    output.append(f"required_skill_value = {int(quest['RequiredSkillValue'])}")
    output.append(f"required_condition = {int(quest['RequiredCondition'])}")
    output.append(f"required_min_rep_faction = {int(quest['RequiredMinRepFaction'])}")
    output.append(f"required_min_rep_value = {int(quest['RequiredMinRepValue'])}")
    output.append(f"required_max_rep_faction = {int(quest['RequiredMaxRepFaction'])}")
    output.append(f"required_max_rep_value = {int(quest['RequiredMaxRepValue'])}")
    output.append(f"suggested_players = {int(quest['SuggestedPlayers'])}")
    output.append(f"limit_time_secs = {int(quest['LimitTime'])}")
    output.append(f"char_title_id = {int(quest['CharTitleId'])}")
    output.append(f"prev_quest_id = {int(quest['PrevQuestId'])}")
    output.append(f"next_quest_id = {int(quest['NextQuestId'])}")
    output.append(f"exclusive_group = {int(quest['ExclusiveGroup'])}")
    output.append(f"breadcrumb_for_quest_id = {int(quest['BreadcrumbForQuestId'])}")
    output.append(f"next_quest_in_chain = {int(quest['NextQuestInChain'])}")
    output.append(f"src_item_id = {int(quest['SrcItemId'])}")
    output.append(f"src_item_count = {int(quest['SrcItemCount'])}")
    output.append(f"src_spell = {int(quest['SrcSpell'])}")
    output.append(f"details = {toml_str(quest['Details'])}")
    output.append(f"objectives_text = {toml_str(quest['Objectives'])}")
    output.append(f"request_items_text = {toml_str(quest['RequestItemsText'])}")
    output.append(f"offer_reward_text = {toml_str(quest['OfferRewardText'])}")
    output.append(f"end_text = {toml_str(quest['EndText'])}")
    output.append(
        "objective_texts = ["
        + ", ".join(toml_str(quest[f"ObjectiveText{i}"]) for i in range(1, 5))
        + "]"
    )
    output.append(f"reward_money_copper = {int(quest['RewOrReqMoney'])}")
    output.append(f"reward_money_max_level = {int(quest['RewMoneyMaxLevel'])}")
    output.append(f"reward_spell = {int(quest['RewSpell'])}")
    output.append(f"reward_spell_cast = {int(quest['RewSpellCast'])}")
    output.append(f"reward_mail_template_id = {int(quest['RewMailTemplateId'])}")
    output.append(f"reward_mail_delay_secs = {int(quest['RewMailDelaySecs'])}")
    output.append(f"reward_honorable_kills = {int(quest['RewHonorableKills'])}")
    output.append(f"point_map_id = {int(quest['PointMapId'])}")
    output.append(f"point_x = {float(quest['PointX'])}")
    output.append(f"point_y = {float(quest['PointY'])}")
    output.append(f"point_opt = {int(quest['PointOpt'])}")
    output.append(f"rep_objective_faction = {int(quest['RepObjectiveFaction'])}")
    output.append(f"rep_objective_value = {int(quest['RepObjectiveValue'])}")
    output.append(
        "giver_creature_entries = ["
        + ", ".join(str(entry) for entry in sorted(creature_givers.get(quest_id, [])))
        + "]"
    )
    output.append(
        "turnin_creature_entries = ["
        + ", ".join(str(entry) for entry in sorted(creature_turnins.get(quest_id, [])))
        + "]"
    )
    output.append(
        "giver_gameobject_entries = ["
        + ", ".join(str(entry) for entry in sorted(object_givers.get(quest_id, [])))
        + "]"
    )
    output.append(
        "turnin_gameobject_entries = ["
        + ", ".join(str(entry) for entry in sorted(object_turnins.get(quest_id, [])))
        + "]"
    )
    output.append(
        "event_ids = ["
        + ", ".join(str(event) for event in sorted(event_quests.get(quest_id, [])))
        + "]"
    )
    output.append("")

    for slot in range(4):
        creature_or_go = int(quest[f"ReqCreatureOrGOId{slot + 1}"])
        spell = int(quest[f"ReqSpellCast{slot + 1}"])
        count = int(quest[f"ReqCreatureOrGOCount{slot + 1}"])
        text = quest[f"ObjectiveText{slot + 1}"]
        if not creature_or_go and not spell:
            continue
        output.extend(
            [
                "[[quest.objective]]",
                f"kind = {toml_str(objective_kind(creature_or_go, spell))}",
                f"slot = {slot}",
                f"entry = {abs(creature_or_go)}",
                f"count = {count}",
                f"spell_id = {spell}",
                f"text = {toml_str(text)}",
                "",
            ]
        )
    for slot in range(4):
        item = int(quest[f"ReqItemId{slot + 1}"])
        count = int(quest[f"ReqItemCount{slot + 1}"])
        if not item:
            continue
        output.extend(
            [
                "[[quest.objective]]",
                'kind = "item"',
                f"slot = {slot}",
                f"entry = {item}",
                f"count = {count}",
                "spell_id = 0",
                f"text = {toml_str(quest[f'ObjectiveText{slot + 1}'])}",
                "",
            ]
        )

    for slot in range(6):
        item = int(quest[f"RewChoiceItemId{slot + 1}"])
        if not item:
            continue
        output.extend(
            [
                "[[quest.reward_choice]]",
                f"item_entry = {item}",
                f"count = {int(quest[f'RewChoiceItemCount{slot + 1}'])}",
                "",
            ]
        )
    for slot in range(4):
        item = int(quest[f"RewItemId{slot + 1}"])
        if not item:
            continue
        output.extend(
            [
                "[[quest.reward_item]]",
                f"item_entry = {item}",
                f"count = {int(quest[f'RewItemCount{slot + 1}'])}",
                "",
            ]
        )
    for slot in range(5):
        faction = int(quest[f"RewRepFaction{slot + 1}"])
        value = int(quest[f"RewRepValue{slot + 1}"])
        max_value = int(quest[f"RewMaxRepValue{slot + 1}"])
        if not faction:
            continue
        output.extend(
            [
                "[[quest.reward_rep]]",
                f"faction = {faction}",
                f"value = {value}",
                f"max_value = {max_value}",
                "",
            ]
        )
    return output


def import_gossip(helpers, source_path, open_source):
    creature_columns = table_columns(source_path, "creature_template", open_source)
    creature_index = index_of(creature_columns, "Entry")
    menu_index = index_of(creature_columns, "gossip_menu_id", "GossipMenuId")
    menus = {}
    for row in helpers.rows(source_path, "creature_template"):
        menu = int(row[menu_index])
        if menu:
            menus[int(row[creature_index])] = menu

    gossip_columns = table_columns(source_path, "gossip_menu", open_source)
    text_index = index_of(gossip_columns, "text_id")
    menu_entry_index = index_of(gossip_columns, "entry")
    condition_index = index_of(gossip_columns, "condition_id")
    menu_texts = {}
    for row in helpers.rows(source_path, "gossip_menu"):
        entry = int(row[menu_entry_index])
        text = int(row[text_index])
        condition = int(row[condition_index])
        current = menu_texts.get(entry)
        # Prefer the unconditioned menu row; ties break on the lowest text id so the pack is stable.
        candidate = (condition != 0, text)
        if current is None or candidate < current[0]:
            menu_texts[entry] = (candidate, text)

    npc_columns = table_columns(source_path, "npc_text", open_source)
    npc_index = index_of(npc_columns, "ID")
    text0_index = index_of(npc_columns, "text0_0")
    text1_index = index_of(npc_columns, "text0_1")
    prob_index = index_of(npc_columns, "prob0")
    texts = {}
    for row in helpers.rows(source_path, "npc_text"):
        texts[int(row[npc_index])] = (row[text0_index], row[text1_index], float(row[prob_index]))

    gossip = {}
    for creature, menu in sorted(menus.items()):
        found = menu_texts.get(menu)
        if found is None:
            continue
        text_id = found[1]
        text = texts.get(text_id)
        if text is None:
            continue
        male, female, probability = text
        if not male and not female:
            continue
        gossip[creature] = {
            "menu_id": menu,
            "text_id": text_id,
            "male": male,
            "female": female,
            "probability": probability,
        }
    return gossip


def import_greetings(helpers, source_path):
    columns = table_columns(source_path, "questgiver_greeting", helpers.open_source)
    entry_index = index_of(columns, "Entry")
    kind_index = index_of(columns, "Type")
    text_index = index_of(columns, "Text")
    emote_index = index_of(columns, "EmoteId")
    delay_index = index_of(columns, "EmoteDelay")
    greetings = {}
    for row in helpers.rows(source_path, "questgiver_greeting"):
        entry = int(row[entry_index])
        greetings.setdefault(entry, []).append(
            {
                "kind": int(row[kind_index]),
                "text": row[text_index],
                "emote_id": int(row[emote_index]),
                "emote_delay": int(row[delay_index]),
            }
        )
    return greetings


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path, help="pinned CMaNGOS TBC full SQL dump")
    parser.add_argument("output", type=pathlib.Path, help="generated quest TOML")
    args = parser.parse_args()
    try:
        content, summary = convert(args.source)
    except (OSError, UnicodeError, ImportError, ValueError, KeyError) as error:
        parser.error(str(error))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(content, encoding="utf-8", newline="\n")
    print(
        f"wrote {summary['quests']} quests, {summary['events']} game events, "
        f"{summary['conditions']} conditions, {summary['gossip']} gossip rows, "
        f"{summary['greetings']} greetings, and {summary['items']} item templates "
        f"to {args.output}"
    )


if __name__ == "__main__":
    main()
