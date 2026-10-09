import importlib.util
import pathlib
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "import-tbc-progression.py"
SPEC = importlib.util.spec_from_file_location("import_tbc_progression", SCRIPT)
IMPORTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IMPORTER)


DDL = """\
CREATE TABLE `creature_template` (
  `Entry` int NOT NULL,
  `TrainerClass` int NOT NULL,
  `TrainerTemplateId` int NOT NULL
) ENGINE=MyISAM;
CREATE TABLE `npc_trainer` (
  `entry` int NOT NULL,
  `spell` int NOT NULL,
  `spellcost` int NOT NULL,
  `reqskill` int NOT NULL,
  `reqskillvalue` int NOT NULL,
  `reqlevel` int NOT NULL,
  `ReqAbility1` int DEFAULT NULL,
  `ReqAbility2` int DEFAULT NULL,
  `ReqAbility3` int DEFAULT NULL,
  `condition_id` int NOT NULL
) ENGINE=MyISAM;
CREATE TABLE `npc_trainer_template` (
  `entry` int NOT NULL,
  `spell` int NOT NULL,
  `spellcost` int NOT NULL,
  `reqskill` int NOT NULL,
  `reqskillvalue` int NOT NULL,
  `reqlevel` int NOT NULL,
  `ReqAbility1` int DEFAULT NULL,
  `ReqAbility2` int DEFAULT NULL,
  `ReqAbility3` int DEFAULT NULL,
  `condition_id` int NOT NULL
) ENGINE=MyISAM;
CREATE TABLE `playercreateinfo_action` (
  `race` int NOT NULL,
  `class` int NOT NULL,
  `button` int NOT NULL,
  `action` int NOT NULL,
  `type` int NOT NULL
) ENGINE=MyISAM;
CREATE TABLE `playercreateinfo_spell` (
  `race` int NOT NULL,
  `class` int NOT NULL,
  `Spell` int NOT NULL,
  `Note` varchar(255) DEFAULT NULL
) ENGINE=MyISAM;
CREATE TABLE `spell_chain` (
  `spell_id` int NOT NULL,
  `prev_spell` int NOT NULL,
  `first_spell` int NOT NULL,
  `rank` int NOT NULL,
  `req_spell` int NOT NULL
) ENGINE=MyISAM;
"""


def sql_dump(*, starter_note="Blood Elf's starter", bad_prerequisite=False):
    prerequisite = 9999 if bad_prerequisite else 1000
    escaped_note = starter_note.replace("\\", "\\\\").replace("'", "\\'").replace("\n", "\\n")
    return DDL + f"""\
INSERT INTO `creature_template` VALUES (700,8,9000),(701,8,9000),(702,8,0);
INSERT INTO `npc_trainer` VALUES (702,2000,125,0,0,4,{prerequisite},NULL,NULL,0);
INSERT INTO `npc_trainer_template` VALUES (9000,2000,125,0,0,4,{prerequisite},NULL,NULL,0);
INSERT INTO `playercreateinfo_action` VALUES (10,8,0,1000,0),(10,8,1,1001,0),(10,8,72,0,64);
INSERT INTO `playercreateinfo_spell` VALUES (10,8,1000,'{escaped_note}'),(10,8,1001,'Fireball');
INSERT INTO `spell_chain` VALUES (1000,0,1000,1,0),(1001,0,1001,1,0),(2000,0,2000,1,1000);
"""


class ImportProgressionTests(unittest.TestCase):
    def convert_text(self, text):
        with tempfile.TemporaryDirectory() as directory:
            source = pathlib.Path(directory) / "world.sql"
            source.write_text(text, encoding="utf-8")
            return IMPORTER.convert(source)

    def test_extracts_starters_actions_and_trainer_requirements(self):
        content, starter_count, action_count, trainer_count = self.convert_text(sql_dump())
        self.assertEqual((starter_count, action_count, trainer_count), (2, 3, 1))
        self.assertIn('source = "https://github.com/cmangos/tbc-db"', content)
        self.assertIn(f'revision = "{IMPORTER.REVISION}"', content)
        self.assertIn('license = "GPL-3.0"', content)
        self.assertIn('note = "Blood Elf\'s starter"', content)
        self.assertIn("prerequisite_spell_ids = [1000]", content)
        self.assertIn("trainer_entries = [700, 701, 702]", content)
        self.assertIn("trainer_template_ids = [9000]", content)
        self.assertIn("required_level = 4", content)
        self.assertIn("cost_copper = 125", content)

    def test_output_order_is_deterministic(self):
        first, *_ = self.convert_text(sql_dump())
        reordered = sql_dump()
        reordered = reordered.replace(
            "(700,8,9000),(701,8,9000),(702,8,0)",
            "(702,8,0),(701,8,9000),(700,8,9000)",
        )
        reordered = reordered.replace(
            "(10,8,0,1000,0),(10,8,1,1001,0),(10,8,72,0,64)",
            "(10,8,72,0,64),(10,8,1,1001,0),(10,8,0,1000,0)",
        )
        reordered = reordered.replace(
            "(10,8,1000,'Blood Elf\\'s starter'),(10,8,1001,'Fireball')",
            "(10,8,1001,'Fireball'),(10,8,1000,'Blood Elf\\'s starter')",
        )
        second, *_ = self.convert_text(reordered)
        self.assertEqual(first, second)

    def test_escapes_sql_strings_and_toml_strings(self):
        content, *_ = self.convert_text(sql_dump(starter_note="A'B\nC"))
        self.assertIn('note = "A\'B\\nC"', content)

    def test_rejects_missing_or_drifted_columns(self):
        dump = sql_dump().replace("`reqlevel` int NOT NULL,", "`level` int NOT NULL,")
        with self.assertRaisesRegex(IMPORTER.ImportError, "unsupported npc_trainer columns"):
            self.convert_text(dump)

    def test_rejects_unsupported_insert_column_lists(self):
        dump = sql_dump().replace(
            "INSERT INTO `playercreateinfo_spell` VALUES",
            "INSERT INTO `playercreateinfo_spell` (`race`,`class`,`Spell`,`Note`) VALUES",
        )
        with self.assertRaisesRegex(IMPORTER.ImportError, "INSERT column lists are unsupported"):
            self.convert_text(dump)

    def test_rejects_malformed_row_width(self):
        dump = sql_dump().replace("(10,8,1000,'Blood Elf\\'s starter')", "(10,8,1000)")
        with self.assertRaisesRegex(IMPORTER.ImportError, "row has 3 values, expected 4"):
            self.convert_text(dump)

    def test_rejects_unresolved_prerequisite_spell(self):
        with self.assertRaisesRegex(IMPORTER.ImportError, "unresolved prerequisite spell 9999"):
            self.convert_text(sql_dump(bad_prerequisite=True))

    def test_rejects_unresolved_action_spell(self):
        dump = sql_dump().replace("(10,8,1,1001,0)", "(10,8,1,9998,0)")
        with self.assertRaisesRegex(IMPORTER.ImportError, "unresolved action-bar spell 9998"):
            self.convert_text(dump)

    def test_rejects_broken_sql_quoting(self):
        dump = sql_dump().replace("'Fireball'", "'Fireball")
        with self.assertRaisesRegex(IMPORTER.ImportError, "unterminated string"):
            self.convert_text(dump)


if __name__ == "__main__":
    unittest.main()
