import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "validate-tbc-content.py"

VALID_PACK = '''\
schema_version = 2
source = "https://example.invalid/tbc-db"
revision = "0123456789abcdef0123456789abcdef01234567"
license = "GPL-3.0"

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
        for movement_type in (0, 1, 2, 4):
            with self.subTest(movement_type=movement_type):
                result = self.run_validator(
                    VALID_PACK.replace("movement_type = 2", f"movement_type = {movement_type}")
                )
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_schema_and_provenance_errors(self):
        cases = (
            (VALID_PACK.replace("schema_version = 2", "schema_version = 3"), "schema_version"),
            (VALID_PACK.replace('source = "https://example.invalid/tbc-db"\n', ""), "source"),
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
            (VALID_PACK.replace("movement_type = 2", "movement_type = 3"), "unsupported value 3"),
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


if __name__ == "__main__":
    unittest.main()
