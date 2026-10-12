import gzip
import pathlib
import subprocess
import sys
import tempfile
import tomllib
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "import-tbc-content.py"

SOURCE = """CREATE TABLE `creature_template` (
  `Entry` int NOT NULL,
  `Name` text NOT NULL,
  `SubName` text,
  `MinLevel` int NOT NULL,
  `DisplayId1` int NOT NULL,
  `Faction` int NOT NULL,
  `Scale` float NOT NULL,
  `CreatureType` int NOT NULL,
  `NpcFlags` int NOT NULL
) ENGINE=MyISAM;
CREATE TABLE `game_event_creature` (
  `guid` int NOT NULL,
  `event` smallint NOT NULL
) ENGINE=MyISAM;
INSERT INTO `creature` VALUES (100,10,0,1,0,0,0,0,30,30,0,0),(101,20,0,1,1,1,1,0,30,30,0,0),(102,30,0,1,2,2,2,0,30,30,0,0),(103,40,0,1,3,3,3,0,30,30,0,0);
INSERT INTO `creature_template` VALUES (10,'Kobold Vermin','',1,100,25,1,7,0),(20,'Midsummer Celebrant','Festival NPC',1,200,35,1,7,1),(30,'Village Vendor','',5,300,12,1,7,128),(40,'Event Baseline NPC','',5,400,12,1,7,1);
INSERT INTO `game_event_creature` VALUES (101,1),(102,-12),(103,1),(103,-2);
"""


class ImportTbcContentTests(unittest.TestCase):
    def test_gzip_import_keeps_base_spawns_and_filters_positive_event_spawns(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "world.sql.gz"
            first_pack = root / "first.toml"
            second_pack = root / "second.toml"
            with gzip.open(source, "wt", encoding="utf-8") as compressed:
                compressed.write(SOURCE)

            for output in (first_pack, second_pack):
                subprocess.run(
                    [sys.executable, str(SCRIPT), str(source), str(output)],
                    check=True,
                    capture_output=True,
                    text=True,
                )

            first_text = first_pack.read_text(encoding="utf-8")
            self.assertEqual(first_text, second_pack.read_text(encoding="utf-8"))
            pack = tomllib.loads(first_text)

        self.assertEqual(pack["schema_version"], 3)
        self.assertEqual(pack["event_policy"], "base-world-only")
        self.assertEqual(
            {spawn["source_guid"] for spawn in pack["spawn"]},
            {100, 102, 103},
            "ordinary and negative-only associations are base-world spawns",
        )
        self.assertEqual(
            {template["entry"] for template in pack["template"]},
            {10, 30, 40},
            "event-only templates are omitted when they have no remaining spawns",
        )
        vendor = next(template for template in pack["template"] if template["entry"] == 30)
        self.assertEqual(vendor["name"], "Village Vendor")
        self.assertEqual(vendor["npc_flags"], 128, "TBC's vendor flag is preserved from NpcFlags")
        self.assertEqual(pack["template"][0]["name"], "Kobold Vermin")


if __name__ == "__main__":
    unittest.main()
