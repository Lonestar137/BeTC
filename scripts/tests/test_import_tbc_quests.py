import importlib.util
import pathlib
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "import-tbc-quests.py"
SPEC = importlib.util.spec_from_file_location("import_tbc_quests", SCRIPT)
IMPORTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IMPORTER)


QUEST_COLUMNS = (
    "entry Method ZoneOrSort MinLevel MaxLevel QuestLevel Type RequiredClasses RequiredRaces "
    "RequiredSkill RequiredSkillValue RequiredCondition RepObjectiveFaction RepObjectiveValue "
    "RequiredMinRepFaction RequiredMinRepValue RequiredMaxRepFaction RequiredMaxRepValue "
    "SuggestedPlayers LimitTime QuestFlags SpecialFlags CharTitleId PrevQuestId NextQuestId "
    "ExclusiveGroup BreadcrumbForQuestId NextQuestInChain SrcItemId SrcItemCount SrcSpell Title "
    "Details Objectives OfferRewardText RequestItemsText EndText ObjectiveText1 ObjectiveText2 "
    "ObjectiveText3 ObjectiveText4 ReqItemId1 ReqItemId2 ReqItemId3 ReqItemId4 ReqItemCount1 "
    "ReqItemCount2 ReqItemCount3 ReqItemCount4 ReqSourceId1 ReqSourceId2 ReqSourceId3 ReqSourceId4 "
    "ReqSourceCount1 ReqSourceCount2 ReqSourceCount3 ReqSourceCount4 ReqCreatureOrGOId1 "
    "ReqCreatureOrGOId2 ReqCreatureOrGOId3 ReqCreatureOrGOId4 ReqCreatureOrGOCount1 "
    "ReqCreatureOrGOCount2 ReqCreatureOrGOCount3 ReqCreatureOrGOCount4 ReqSpellCast1 "
    "ReqSpellCast2 ReqSpellCast3 ReqSpellCast4 RewChoiceItemId1 RewChoiceItemId2 RewChoiceItemId3 "
    "RewChoiceItemId4 RewChoiceItemId5 RewChoiceItemId6 RewChoiceItemCount1 RewChoiceItemCount2 "
    "RewChoiceItemCount3 RewChoiceItemCount4 RewChoiceItemCount5 RewChoiceItemCount6 RewItemId1 "
    "RewItemId2 RewItemId3 RewItemId4 RewItemCount1 RewItemCount2 RewItemCount3 RewItemCount4 "
    "RewRepFaction1 RewRepFaction2 RewRepFaction3 RewRepFaction4 RewRepFaction5 RewRepValue1 "
    "RewRepValue2 RewRepValue3 RewRepValue4 RewRepValue5 RewMaxRepValue1 RewMaxRepValue2 "
    "RewMaxRepValue3 RewMaxRepValue4 RewMaxRepValue5 RewHonorableKills RewOrReqMoney "
    "RewMoneyMaxLevel RewSpell RewSpellCast RewMailTemplateId RewMailDelaySecs PointMapId PointX "
    "PointY PointOpt"
).split()


def quest_row(**overrides):
    values = {
        "entry": 100,
        "Method": 2,
        "MinLevel": 1,
        "MaxLevel": 255,
        "QuestLevel": 1,
        "RequiredRaces": 512,
        "RequiredClasses": 0,
        "Title": "Test Quest",
        "Details": "Details",
        "Objectives": "Objectives",
        "OfferRewardText": "Reward",
        "RequestItemsText": "Progress",
        "EndText": "",
        "ReqCreatureOrGOId1": 200,
        "ReqCreatureOrGOCount1": 3,
        "RewOrReqMoney": 25,
        "RewMoneyMaxLevel": 60,
        "RewItemId1": 1000,
        "RewItemCount1": 1,
        "RewChoiceItemId1": 1001,
        "RewChoiceItemCount1": 2,
        "RewRepFaction1": 911,
        "RewRepValue1": 250,
        "RewMaxRepValue1": 42999,
        "NextQuestInChain": 101,
        "PointX": 10.5,
        "PointY": -20.25,
        "RequiredCondition": 7,
    }
    values.update(overrides)
    text_columns = (
        "Title", "Details", "Objectives", "OfferRewardText", "RequestItemsText", "EndText",
        "ObjectiveText1", "ObjectiveText2", "ObjectiveText3", "ObjectiveText4",
    )
    return tuple(str(values.get(column, "" if column in text_columns else 0))
                 for column in QUEST_COLUMNS)


def sql_literal(value):
    if isinstance(value, str):
        return "'" + value.replace("\\", "\\\\").replace("'", "\\'") + "'"
    return str(value)


def insert(table, columns, rows):
    tuples = ",".join("(" + ",".join(sql_literal(value) for value in row) + ")" for row in rows)
    return f"INSERT INTO `{table}` VALUES {tuples};"


def create(table, columns):
    body = ",\n  ".join(f"`{column}` int NOT NULL DEFAULT 0" for column in columns)
    return f"CREATE TABLE `{table}` (\n  {body}\n) ENGINE=MyISAM;"


def build_source():
    parts = [
        create("quest_template", QUEST_COLUMNS),
        create("creature_questrelation", ["id", "quest"]),
        create("creature_involvedrelation", ["id", "quest"]),
        create("gameobject_questrelation", ["id", "quest"]),
        create("gameobject_involvedrelation", ["id", "quest"]),
        create("game_event", ["entry", "schedule_type", "occurence", "length", "holiday",
                              "linkedTo", "EventGroup", "description"]),
        create("game_event_time", ["entry", "start_time", "end_time"]),
        create("game_event_quest", ["quest", "event"]),
        create("conditions", ["condition_entry", "type", "value1", "value2", "value3", "value4",
                              "flags", "comments"]),
        create("item_template", IMPORTER.ITEM_FIELDS),
        create("npc_text", ["ID", "text0_0", "text0_1", "prob0"]),
        create("gossip_menu", ["entry", "text_id", "script_id", "condition_id"]),
        create("creature_template", ["Entry", "gossip_menu_id"]),
        create("questgiver_greeting", ["Entry", "Type", "Text", "EmoteId", "EmoteDelay"]),
        insert("quest_template", QUEST_COLUMNS, [
            quest_row(),
            quest_row(entry=101, Title="Second Quest", PrevQuestId=100, NextQuestInChain=0,
                      ReqCreatureOrGOId1=0, ReqCreatureOrGOCount1=0, RewItemId1=0,
                      RewChoiceItemId1=0, RequiredCondition=0, RewRepFaction1=0,
                      ReqItemId1=1002, ReqItemCount1=4),
            quest_row(entry=102, Method=0, ReqCreatureOrGOId1=0, ReqCreatureOrGOCount1=0,
                      RewItemId1=0, RewChoiceItemId1=0, RewRepFaction1=0, RequiredCondition=0),
        ]),
        insert("creature_questrelation", ["id", "quest"], [(300, 100), (301, 102)]),
        insert("creature_involvedrelation", ["id", "quest"], [(300, 100)]),
        insert("gameobject_questrelation", ["id", "quest"], [(400, 101)]),
        insert("gameobject_involvedrelation", ["id", "quest"], []),
        insert("game_event", ["entry", "schedule_type", "occurence", "length", "holiday",
                              "linkedTo", "EventGroup", "description"],
               [(10, 1, 525600, 20160, 341, 0, 0, "Midsummer")]),
        insert("game_event_time", ["entry", "start_time", "end_time"],
               [(10, "2012-06-21 02:00:00", "2025-12-31 00:00:00")]),
        insert("game_event_quest", ["quest", "event"], [(102, 10)]),
        insert("conditions", ["condition_entry", "type", "value1", "value2", "value3", "value4",
                              "flags", "comments"],
               [(7, -1, 8, 9, 0, 0, 0, "AND"),
                (8, 8, 5, 0, 0, 0, 0, "Quest 5 rewarded"),
                (9, 2, 1002, 4, 0, 0, 0, "Has 4 items")]),
        insert("item_template", IMPORTER.ITEM_FIELDS, [
            (1000, 4, 1, "Reward Item", 11, 1, 20, 4, 0, -1, -1, 1, 20, 0, 0, 0, 0, 0, 0, 0),
            (1001, 4, 1, "Choice Item", 12, 1, 30, 5, 0, -1, -1, 1, 20, 0, 0, 0, 0, 0, 0, 0),
            (1002, 12, 0, "Objective Item", 13, 1, 10, 2, 0, -1, -1, 1, 20, 0, 0, 0, 0, 0, 0, 0),
        ]),
        insert("npc_text", ["ID", "text0_0", "text0_1", "prob0"], [(900, "Hello $N", "", 1.0)]),
        insert("gossip_menu", ["entry", "text_id", "script_id", "condition_id"], [(500, 900, 0, 0)]),
        insert("creature_template", ["Entry", "gossip_menu_id"], [(300, 500)]),
        insert("questgiver_greeting", ["Entry", "Type", "Text", "EmoteId", "EmoteDelay"],
               [(300, 0, "Greetings", 1, 0)]),
    ]
    return "\n".join(parts) + "\n"


class ImportTbcQuestsTests(unittest.TestCase):
    def convert(self, source_text):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "world.sql"
            source.write_text(source_text, encoding="utf-8")
            first = IMPORTER.convert(source)
            second = IMPORTER.convert(source)
        self.assertEqual(first, second)
        return first

    def test_converts_quests_relations_events_conditions_and_items_deterministically(self):
        content, summary = self.convert(build_source())
        self.assertEqual(
            summary,
            {"quests": 3, "events": 1, "conditions": 3, "gossip": 1, "greetings": 1, "items": 3},
        )
        self.assertIn('title = "Test Quest"', content)
        self.assertIn("giver_creature_entries = [300]", content)
        self.assertIn("turnin_creature_entries = [300]", content)
        self.assertIn("giver_gameobject_entries = [400]", content)
        self.assertIn("event_ids = [10]", content)
        self.assertIn('kind = "creature_kill"', content)
        self.assertIn('kind = "item"', content)
        self.assertIn("start_month = 6", content)
        self.assertIn("start_day = 21", content)
        self.assertIn("start_minute = 120", content)
        self.assertIn('male = "Hello $N"', content)
        self.assertIn('text = "Greetings"', content)
        self.assertIn("reward_money_copper = 25", content)
        self.assertIn("reward_money_max_level = 60", content)
        self.assertIn('name = "Objective Item"', content)

    def test_gameobject_objective_sign_selects_the_use_kind(self):
        source = build_source()
        original = ",".join(sql_literal(value) for value in quest_row())
        replacement = ",".join(sql_literal(value) for value in quest_row(ReqCreatureOrGOId1=-200))
        source = source.replace(original, replacement, 1)
        content, _ = self.convert(source)
        self.assertIn('kind = "gameobject_use"', content)
        self.assertNotIn('kind = "creature_kill"', content)

    def test_missing_quest_condition_is_a_conversion_error(self):
        source = build_source().replace(
            insert("conditions", ["condition_entry", "type", "value1", "value2", "value3", "value4",
                                  "flags", "comments"],
                   [(7, -1, 8, 9, 0, 0, 0, "AND"),
                    (8, 8, 5, 0, 0, 0, 0, "Quest 5 rewarded"),
                    (9, 2, 1002, 4, 0, 0, 0, "Has 4 items")]),
            "",
        )
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source_path = root / "world.sql"
            source_path.write_text(source, encoding="utf-8")
            with self.assertRaisesRegex(IMPORTER.ImportError, "missing condition"):
                IMPORTER.convert(source_path)


if __name__ == "__main__":
    unittest.main()
