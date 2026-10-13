import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "validate-tbc-content.py"

VALID_PACK = '''\
schema_version = 3
source = "https://example.invalid/tbc-db"
revision = "0123456789abcdef0123456789abcdef01234567"
license = "GPL-3.0"
event_policy = "base-world-only"

[[template]]
entry = 10
name = "Test creature"
subname = ""
level = 1
display_id = 20
faction = 30
scale = 1.0
creature_type = 7
npc_flags = 0

[[spawn]]
source_guid = 100
entry = 10
map_id = 530
x = 1.25
y = -2.5
z = 3.75
orientation = 0.0
respawn_min_secs = 30
respawn_max_secs = 60
wander_radius = 0.0
movement_type = 2

[[waypoint]]
source_guid = 100
point = 1
x = 4.0
y = 5.0
z = 6.0
orientation = 1.0
wait_ms = 500

[[addon]]
source_guid = 100
stand_state = 0
sheath_state = 1
emote = 0
'''


class ValidateTbcContentTests(unittest.TestCase):
    def run_validator(self, text):
        with tempfile.TemporaryDirectory() as directory:
            pack = Path(directory) / "fixture.toml"
            pack.write_text(text, encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(SCRIPT), str(pack)],
                check=False,
                capture_output=True,
                text=True,
            )

    def test_valid_pack_has_deterministic_summary(self):
        result = self.run_validator(VALID_PACK)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout,
            "valid TBC content pack: 1 templates, 1 spawns, 1 waypoints, 1 addons\n",
        )

    def test_optional_tables_may_be_absent(self):
        pack = VALID_PACK.split("\n[[waypoint]]", 1)[0] + "\n"
        result = self.run_validator(pack)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("1 templates, 1 spawns, 0 waypoints, 0 addons", result.stdout)

    def test_accepts_each_supported_movement_type(self):
        for movement_type in (0, 1, 2, 3, 4):
            with self.subTest(movement_type=movement_type):
                result = self.run_validator(
                    VALID_PACK.replace("movement_type = 2", f"movement_type = {movement_type}")
                )
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_schema_and_provenance_errors(self):
        cases = (
            (VALID_PACK.replace("schema_version = 3", "schema_version = 2"), "schema_version"),
            (VALID_PACK.replace('source = "https://example.invalid/tbc-db"\n', ""), "source"),
            (VALID_PACK.replace('event_policy = "base-world-only"\n', ""), "event_policy"),
            (
                VALID_PACK.replace(
                    'revision = "0123456789abcdef0123456789abcdef01234567"', "revision = 7"
                ),
                "revision",
            ),
            (
                VALID_PACK.replace(
                    "0123456789abcdef0123456789abcdef01234567",
                    "0123456789ABCDEF0123456789ABCDEF01234567",
                ),
                "40-character lowercase hexadecimal",
            ),
            (
                VALID_PACK.replace(
                    "0123456789abcdef0123456789abcdef01234567", "deadbeef"
                ),
                "40-character lowercase hexadecimal",
            ),
            (VALID_PACK.replace('license = "GPL-3.0"', 'license = "  "'), "license"),
            (
                VALID_PACK.replace('event_policy = "base-world-only"', 'event_policy = "active"'),
                "event_policy: expected 'base-world-only'",
            ),
        )
        for pack, message in cases:
            with self.subTest(message=message):
                result = self.run_validator(pack)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_rejects_unknown_root_field(self):
        pack = VALID_PACK.replace(
            'license = "GPL-3.0"\n', 'license = "GPL-3.0"\nextra = true\n'
        )
        result = self.run_validator(pack)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("extra: unknown root field", result.stderr)

    def test_rejects_required_table_that_is_not_an_array(self):
        pack = VALID_PACK.replace("[[template]]", "template = {}")
        result = self.run_validator(pack)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("template: expected an array of tables", result.stderr)

    def test_rejects_duplicate_template_entries_and_spawn_guids(self):
        duplicate_template = VALID_PACK + '''\
[[template]]
entry = 10
name = "Duplicate"
subname = ""
level = 1
display_id = 20
faction = 30
scale = 1.0
creature_type = 7
npc_flags = 0
'''
        duplicate_spawn = VALID_PACK + '''\
[[spawn]]
source_guid = 100
entry = 10
map_id = 530
x = 0.0
y = 0.0
z = 0.0
orientation = 0.0
respawn_min_secs = 30
respawn_max_secs = 60
wander_radius = 0.0
movement_type = 0
'''
        for pack, message in (
            (duplicate_template, "duplicate template entry"),
            (duplicate_spawn, "duplicate spawn GUID"),
        ):
            with self.subTest(message=message):
                result = self.run_validator(pack)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_rejects_non_finite_and_unrepresentable_numbers(self):
        for number in ("inf", "1e100"):
            with self.subTest(number=number):
                result = self.run_validator(VALID_PACK.replace("x = 1.25", f"x = {number}"))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("finite f32-representable", result.stderr)

    def test_rejects_missing_template_and_invalid_movement_type(self):
        cases = (
            (
                VALID_PACK.replace("entry = 10\nmap_id = 530", "entry = 11\nmap_id = 530"),
                "no template",
            ),
            (VALID_PACK.replace("movement_type = 2", "movement_type = 5"), "unsupported value 5"),
        )
        for pack, message in cases:
            with self.subTest(message=message):
                result = self.run_validator(pack)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_rejects_waypoint_and_addon_references_to_missing_spawns(self):
        cases = (
            (
                VALID_PACK.replace("source_guid = 100\npoint = 1", "source_guid = 101\npoint = 1"),
                "waypoint[0]",
            ),
            (
                VALID_PACK.replace("source_guid = 100\nstand_state", "source_guid = 101\nstand_state"),
                "addon[0]",
            ),
        )
        for pack, message in cases:
            with self.subTest(message=message):
                result = self.run_validator(pack)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_reports_malformed_toml_and_missing_row_fields(self):
        malformed = self.run_validator("schema_version = [\n")
        self.assertNotEqual(malformed.returncode, 0)
        self.assertIn("content pack error", malformed.stderr)

        missing_field = self.run_validator(VALID_PACK.replace('name = "Test creature"\n', ""))
        self.assertNotEqual(missing_field.returncode, 0)
        self.assertIn("template[0].name: missing required field", missing_field.stderr)


VALID_VENDOR_PACK = '''\
schema_version = 1
source = "https://example.invalid/tbc-db"
revision = "0123456789abcdef0123456789abcdef01234567"
license = "GPL-3.0"
extraction_command = "python3 scripts/import-tbc-vendor.py <dump> zones.toml vendor.toml"

[[vendor]]
entry = 100
item = 1000
maxcount = 2
incrtime = 9000
buy_count = 1

[[item_template]]
entry = 1000
class = 0
subclass = 5
name = "Spring Water"
display_id = 11
quality = 1
buy_price = 25
sell_price = 1
inventory_type = 0
allowable_class = -1
allowable_race = -1
item_level = 5
stackable = 20
armor = 0
bonding = 0
material = -1
max_durability = 0
dmg_min = 0
dmg_max = 0
delay = 0
'''


class ValidateTbcVendorTests(unittest.TestCase):
    def run_validator(self, text):
        with tempfile.TemporaryDirectory() as directory:
            pack = Path(directory) / "fixture.toml"
            pack.write_text(text, encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(SCRIPT), str(pack)],
                check=False,
                capture_output=True,
                text=True,
            )

    def test_valid_vendor_pack_has_deterministic_summary(self):
        result = self.run_validator(VALID_VENDOR_PACK)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout,
            "valid TBC vendor pack: 1 stock rows, 1 item templates\n",
        )

    def test_rejects_a_stock_row_without_an_item_template(self):
        result = self.run_validator(
            VALID_VENDOR_PACK.replace("item = 1000", "item = 2000")
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("vendor[0].item: no item_template with entry 2000", result.stderr)

    def test_rejects_a_zero_buy_count_and_missing_provenance(self):
        zero = self.run_validator(VALID_VENDOR_PACK.replace("buy_count = 1", "buy_count = 0"))
        self.assertNotEqual(zero.returncode, 0)
        self.assertIn("vendor[0].buy_count: expected at least 1", zero.stderr)

        missing = self.run_validator(
            VALID_VENDOR_PACK.replace(
                'extraction_command = "python3 scripts/import-tbc-vendor.py <dump> zones.toml '
                'vendor.toml"\n',
                "",
            )
        )
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("extraction_command: missing required root field", missing.stderr)

    def test_rejects_an_unknown_item_template_field(self):
        result = self.run_validator(VALID_VENDOR_PACK.replace("armor = 0", "armour = 0"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("item_template[0].armour: unknown field", result.stderr)
        self.assertIn("item_template[0].armor: missing required field", result.stderr)


VALID_QUEST_PACK = '''\
schema_version = 2
source = "https://example.invalid/tbc-db"
revision = "0123456789abcdef0123456789abcdef01234567"
license = "GPL-3.0"
conversion_version = 2
client_build = 8606
extraction_command = "python3 scripts/import-tbc-quests.py <dump> quests.toml"

[[game_event]]
entry = 10
schedule_type = 1
occurrence_minutes = 525600
length_minutes = 20160
holiday = 201
start_month = 4
start_day = 29
start_minute = 120
description = "Children's Week"

[[condition]]
id = 7
kind = 8
value1 = 5
value2 = 0
value3 = 0
value4 = 0
flags = 0
comment = "Quest 5 rewarded"

[[quest]]
id = 100
title = "Test quest"
method = 2
zone_or_sort = 0
min_level = 1
max_level = 255
quest_level = 1
quest_type = 0
quest_flags = 0
special_flags = 0
required_race_mask = 512
required_class_mask = 0
required_skill = 0
required_skill_value = 0
required_condition = 7
required_min_rep_faction = 0
required_min_rep_value = 0
required_max_rep_faction = 0
required_max_rep_value = 0
suggested_players = 0
limit_time_secs = 0
char_title_id = 0
prev_quest_id = 0
next_quest_id = 0
exclusive_group = 0
breadcrumb_for_quest_id = 0
next_quest_in_chain = 0
src_item_id = 0
src_item_count = 0
src_spell = 0
details = "Details"
objectives_text = "Objectives"
request_items_text = "Progress"
offer_reward_text = "Reward"
end_text = ""
objective_texts = ["", "", "", ""]
reward_money_copper = 30
reward_money_max_level = 60
reward_spell = 0
reward_spell_cast = 0
reward_mail_template_id = 0
reward_mail_delay_secs = 0
reward_honorable_kills = 0
point_map_id = 0
point_x = 0.0
point_y = 0.0
point_opt = 0
rep_objective_faction = 0
rep_objective_value = 0
giver_creature_entries = [300]
turnin_creature_entries = [300]
giver_gameobject_entries = []
turnin_gameobject_entries = []
event_ids = [10]

[[quest.objective]]
kind = "creature_kill"
slot = 0
entry = 200
count = 3
spell_id = 0
text = ""

[[quest.objective]]
kind = "item"
slot = 1
entry = 1000
count = 1
spell_id = 0
text = ""

[[quest.reward_choice]]
item_entry = 1000
count = 1

[[quest.reward_rep]]
faction = 911
value = 250
max_value = 42999

[[item_template]]
entry = 1000
class = 4
subclass = 1
name = "Objective Item"
display_id = 11
quality = 1
buy_price = 20
sell_price = 4
inventory_type = 0
allowable_class = -1
allowable_race = -1
item_level = 1
stackable = 20
armor = 0
bonding = 0
material = 0
max_durability = 0
dmg_min = 0
dmg_max = 0
delay = 0
'''


class ValidateTbcQuestTests(unittest.TestCase):
    def run_validator(self, text):
        with tempfile.TemporaryDirectory() as directory:
            pack = Path(directory) / "fixture.toml"
            pack.write_text(text, encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(SCRIPT), str(pack)],
                check=False,
                capture_output=True,
                text=True,
            )

    def test_valid_quest_pack_has_deterministic_summary(self):
        result = self.run_validator(VALID_QUEST_PACK)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout,
            "valid TBC quest pack: 1 quests, 1 game events, 1 conditions, 0 gossip rows, "
            "0 greetings, 1 item templates\n",
        )

    def test_rejects_an_item_objective_without_a_template(self):
        result = self.run_validator(VALID_QUEST_PACK.replace("entry = 1000\ncount = 1", "entry = 2000\ncount = 1", 1))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no item_template for 2000", result.stderr)

    def test_rejects_an_unknown_event_and_condition(self):
        event = self.run_validator(VALID_QUEST_PACK.replace("event_ids = [10]", "event_ids = [11]"))
        self.assertNotEqual(event.returncode, 0)
        self.assertIn("no game_event for 11", event.stderr)
        condition = self.run_validator(
            VALID_QUEST_PACK.replace("required_condition = 7", "required_condition = 8")
        )
        self.assertNotEqual(condition.returncode, 0)
        self.assertIn("no condition for 8", condition.stderr)

    def test_rejects_a_descending_quest_id(self):
        pack = VALID_QUEST_PACK.replace("id = 100", "id = 50").replace(
            "[[quest]]\nid = 50", "[[quest]]\nid = 100\n\n[[quest]]\nid = 50", 1
        )
        result = self.run_validator(pack)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unique and ascending", result.stderr)


if __name__ == "__main__":
    unittest.main()
