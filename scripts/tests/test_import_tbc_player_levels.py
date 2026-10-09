import importlib.util
import pathlib
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "import-tbc-player-levels.py"
SPEC = importlib.util.spec_from_file_location("import_tbc_player_levels", SCRIPT)
IMPORTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IMPORTER)


DDL = """CREATE TABLE `player_xp_for_level` (
  `lvl` int unsigned NOT NULL,
  `xp_for_next_level` int unsigned NOT NULL,
  PRIMARY KEY (`lvl`)
) ENGINE=MyISAM;
"""


def source_with_rows(rows):
    values = ",".join(f"({level},{xp})" for level, xp in rows)
    return DDL + f"INSERT INTO `player_xp_for_level` VALUES {values};\n"


class ImportTbcPlayerLevelsTests(unittest.TestCase):
    def convert(self, source):
        with tempfile.TemporaryDirectory() as directory:
            source_path = pathlib.Path(directory) / "world.sql"
            source_path.write_text(source, encoding="utf-8")
            return IMPORTER.convert(source_path)

    def test_imports_all_tbc_level_thresholds_deterministically(self):
        rows = [(level, level * 100) for level in range(1, 70)]
        first, count = self.convert(source_with_rows(rows))
        second, _ = self.convert(source_with_rows(rows))
        self.assertEqual(count, 69)
        self.assertEqual(first, second)
        self.assertIn("level = 1\nxp_to_next = 100", first)
        self.assertIn("level = 69\nxp_to_next = 6900", first)

    def test_rejects_missing_or_duplicate_levels(self):
        rows = [(level, level * 100) for level in range(1, 70)]
        with self.assertRaisesRegex(IMPORTER.ImportError, "exactly the 69 thresholds"):
            self.convert(source_with_rows(rows[:-1]))
        rows[-1] = rows[-2]
        with self.assertRaisesRegex(IMPORTER.ImportError, "duplicate XP threshold"):
            self.convert(source_with_rows(rows))


if __name__ == "__main__":
    unittest.main()
