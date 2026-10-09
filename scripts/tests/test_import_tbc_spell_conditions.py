import importlib.util
import pathlib
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "import-tbc-spell-conditions.py"
SPEC = importlib.util.spec_from_file_location("import_tbc_spell_conditions", SCRIPT)
IMPORTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IMPORTER)


DDL = """CREATE TABLE `conditions` (
  `condition_entry` int NOT NULL,
  `type` tinyint NOT NULL,
  `value1` int NOT NULL,
  `value2` int NOT NULL,
  `value3` int NOT NULL,
  `value4` int NOT NULL,
  `flags` tinyint NOT NULL,
  `comments` varchar(500)
) ENGINE=MyISAM;
INSERT INTO `conditions` VALUES (1,14,512,0,0,0,0,'Blood Elf'),(2,-2,1,3,0,0,0,'either'),(3,17,133,0,0,0,0,'Fireball known');
"""

PROGRESSION = """schema_version = 1
[[trainer_spell]]
condition_id = 2
"""


class ImportTbcSpellConditionsTests(unittest.TestCase):
    def convert(self, source=DDL, progression=PROGRESSION):
        with tempfile.TemporaryDirectory() as directory:
            source_path = pathlib.Path(directory) / "world.sql"
            progression_path = pathlib.Path(directory) / "progression.toml"
            source_path.write_text(source, encoding="utf-8")
            progression_path.write_text(progression, encoding="utf-8")
            return IMPORTER.convert(source_path, progression_path)

    def test_imports_only_trainer_conditions_and_recursive_boolean_children(self):
        first, count = self.convert()
        second, _ = self.convert()
        self.assertEqual(count, 3)
        self.assertEqual(first, second)
        self.assertIn("id = 1", first)
        self.assertIn("id = 2", first)
        self.assertIn("id = 3", first)
        self.assertIn('comment = "Blood Elf"', first)

    def test_rejects_a_missing_boolean_child(self):
        source = DDL.replace("(1,14,512,0,0,0,0,'Blood Elf'),", "")
        with self.assertRaisesRegex(IMPORTER.ImportError, "condition 1 is unresolved"):
            self.convert(source)

    def test_rejects_condition_expression_cycles(self):
        source = DDL.replace(
            "(1,14,512,0,0,0,0,'Blood Elf'),",
            "(1,-1,2,0,0,0,0,'cycle'),",
        ).replace("(2,-2,1,3,0,0,0,'either')", "(2,-2,1,1,0,0,0,'cycle')")
        with self.assertRaisesRegex(IMPORTER.ImportError, "cycle includes id 2"):
            self.convert(source)

    def test_refuses_a_progression_pack_without_source_condition_ids(self):
        progression = "schema_version = 1\n[[trainer_spell]]\ncondition_id = 0\n"
        with self.assertRaisesRegex(IMPORTER.ImportError, "no trainer condition references"):
            self.convert(progression=progression)


if __name__ == "__main__":
    unittest.main()
