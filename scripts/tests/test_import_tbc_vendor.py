import importlib.util
import pathlib
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "import-tbc-vendor.py"
SPEC = importlib.util.spec_from_file_location("import_tbc_vendor", SCRIPT)
IMPORTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IMPORTER)


SOURCE = """CREATE TABLE `npc_vendor` (
  `entry` int NOT NULL,
  `item` int NOT NULL,
  `maxcount` int NOT NULL,
  `incrtime` int NOT NULL,
  `slot` int NOT NULL,
  `ExtendedCost` int NOT NULL,
  `condition_id` int NOT NULL,
  `comments` text
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
  `delay` int NOT NULL,
  `BuyCount` int NOT NULL
) ENGINE=MyISAM;
INSERT INTO `npc_vendor` VALUES (100,1000,2,9000,0,0,0,'limited'),(100,2000,0,0,0,0,0,'unlimited'),(999,3000,0,0,0,0,0,'other zone');
INSERT INTO `item_template` VALUES (1000,0,5,'Spring Water',11,1,25,1,0,-1,-1,5,20,0,0,-1,0,0,0,0,1),(2000,2,7,'Sword',12,2,100,20,21,-1,-1,10,1,0,0,1,40,3.4,6.6,2000,1);
"""


class ImportTbcVendorTests(unittest.TestCase):
    def test_imports_zone_stock_and_item_templates_deterministically(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "world.sql"
            zone_pack = root / "zones.toml"
            source.write_text(SOURCE, encoding="utf-8")
            zone_pack.write_text('[[template]]\nentry = 100\n', encoding="utf-8")

            first = IMPORTER.convert(source, zone_pack)
            second = IMPORTER.convert(source, zone_pack)

        content, vendors, items = first
        self.assertEqual(first, second)
        self.assertEqual((vendors, items), (2, 2))
        self.assertNotIn("other zone", content)
        self.assertIn("maxcount = 2", content)
        self.assertIn("incrtime = 9000", content)
        self.assertIn("buy_count = 1", content)
        self.assertIn('name = "Spring Water"', content)
        self.assertIn("dmg_min = 3", content, "the weapon damage rounds to whole points")

    def test_missing_item_template_is_a_conversion_error(self):
        source = SOURCE.replace(
            "(2000,2,7,'Sword',12,2,100,20,21,-1,-1,10,1,0,0,1,40,3.4,6.6,2000,1)", ""
        ).replace(",;", ";")
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source_path = root / "world.sql"
            zone_pack = root / "zones.toml"
            source_path.write_text(source, encoding="utf-8")
            zone_pack.write_text('[[template]]\nentry = 100\n', encoding="utf-8")
            with self.assertRaisesRegex(IMPORTER.ImportError, "missing item templates"):
                IMPORTER.convert(source_path, zone_pack)

    def test_zone_without_vendor_rows_is_a_conversion_error(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source_path = root / "world.sql"
            zone_pack = root / "zones.toml"
            source_path.write_text(SOURCE, encoding="utf-8")
            zone_pack.write_text('[[template]]\nentry = 555\n', encoding="utf-8")
            with self.assertRaisesRegex(IMPORTER.ImportError, "no npc_vendor rows"):
                IMPORTER.convert(source_path, zone_pack)


if __name__ == "__main__":
    unittest.main()
