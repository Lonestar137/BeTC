import importlib.util
import pathlib
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "import-tbc-loot.py"
SPEC = importlib.util.spec_from_file_location("import_tbc_loot", SCRIPT)
IMPORTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IMPORTER)


SOURCE = """CREATE TABLE `creature_template` (
  `Entry` int NOT NULL,
  `MinLootGold` int NOT NULL,
  `MaxLootGold` int NOT NULL,
  `LootId` int NOT NULL
) ENGINE=MyISAM;
CREATE TABLE `creature_loot_template` (
  `Entry` int NOT NULL,
  `Item` int NOT NULL,
  `ChanceOrQuestChance` float NOT NULL,
  `GroupId` int NOT NULL,
  `MinCountOrRef` int NOT NULL,
  `MaxCount` int NOT NULL,
  `ConditionId` int NOT NULL,
  `Comment` text NOT NULL
) ENGINE=MyISAM;
CREATE TABLE `reference_loot_template` (
  `Entry` int NOT NULL,
  `Item` int NOT NULL,
  `ChanceOrQuestChance` float NOT NULL,
  `GroupId` int NOT NULL,
  `MinCountOrRef` int NOT NULL,
  `MaxCount` int NOT NULL,
  `ConditionId` int NOT NULL,
  `Comment` text NOT NULL
) ENGINE=MyISAM;
CREATE TABLE `item_template` (
  `entry` int NOT NULL,
  `class` int NOT NULL,
  `subclass` int NOT NULL,
  `name` text NOT NULL,
  `displayid` int NOT NULL,
  `Quality` int NOT NULL,
  `BuyPrice` int NOT NULL,
  `SellPrice` int NOT NULL,
  `InventoryType` int NOT NULL,
  `AllowableClass` int NOT NULL,
  `AllowableRace` int NOT NULL,
  `ItemLevel` int NOT NULL,
  `stackable` int NOT NULL,
  `armor` int NOT NULL,
  `bonding` int NOT NULL,
  `Material` int NOT NULL,
  `MaxDurability` int NOT NULL,
  `dmg_min1` float NOT NULL,
  `dmg_max1` float NOT NULL,
  `delay` int NOT NULL
) ENGINE=MyISAM;
INSERT INTO `creature_template` VALUES (100,2,8,100);
INSERT INTO `creature_loot_template` VALUES (100,1000,25,0,1,2,0,'direct'),(100,500,100,0,-500,1,0,'reference');
INSERT INTO `reference_loot_template` VALUES (500,2000,100,1,1,1,0,'nested item');
INSERT INTO `item_template` VALUES (1000,0,0,'Direct Item',11,1,20,4,0,-1,-1,1,20,0,0,0,0,0,0,0),(2000,0,0,'Nested Item',12,1,30,5,0,-1,-1,1,20,0,0,0,0,0,0,0);
"""


class ImportTbcLootTests(unittest.TestCase):
    def test_imports_creature_tables_references_and_item_templates_deterministically(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "world.sql"
            zone_pack = root / "zones.toml"
            source.write_text(SOURCE, encoding="utf-8")
            zone_pack.write_text('schema_version = 1\n\n[[template]]\nentry = 100\n', encoding="utf-8")

            first = IMPORTER.convert(source, zone_pack)
            second = IMPORTER.convert(source, zone_pack)

        content, creature_count, loot_count, reference_count, item_count = first
        self.assertEqual(first, second)
        self.assertEqual((creature_count, loot_count, reference_count, item_count), (1, 1, 1, 2))
        self.assertIn("loot_id = 100", content)
        self.assertIn("min_gold = 2", content)
        self.assertIn("min_count_or_ref = -500", content)
        self.assertIn('name = "Nested Item"', content)

    def test_missing_nested_reference_is_a_conversion_error(self):
        source = SOURCE.replace(
            "INSERT INTO `reference_loot_template` VALUES (500,2000,100,1,1,1,0,'nested item');\n",
            "",
        )
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source_path = root / "world.sql"
            zone_pack = root / "zones.toml"
            source_path.write_text(source, encoding="utf-8")
            zone_pack.write_text('[[template]]\nentry = 100\n', encoding="utf-8")
            with self.assertRaisesRegex(IMPORTER.ImportError, "missing reference loot tables"):
                IMPORTER.convert(source_path, zone_pack)


if __name__ == "__main__":
    unittest.main()
