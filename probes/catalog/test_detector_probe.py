from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

from probes.catalog.detector_probe import FixtureRejected, regenerate, run_fixture_probe, validate_bytecode


class DetectorFixtureTests(unittest.TestCase):
    def test_cpython_fixture_positive_and_negative_cases(self) -> None:
        result = run_fixture_probe()
        cpython = result["cpython"]
        for key in (
            "compatible_source_pair", "checked_hash_source_match",
            "regeneration_exact_match", "changed_cache_refused",
            "missing_source_refused", "symlink_refused",
            "unexpected_file_refused", "source_not_executed",
        ):
            with self.subTest(evidence=key):
                self.assertTrue(cpython[key])
        self.assertEqual(result["scope"], "synthetic fixtures only; no user paths, no global installation")

    def test_unexpected_file_is_refused_and_preserved(self) -> None:
        with tempfile.TemporaryDirectory(prefix="disk-steward-catalog-test-") as temp:
            root = Path(temp)
            source = root / "module.py"
            source.write_text("VALUE = 1\n", encoding="utf-8")
            cache, baseline = regenerate(source)
            unknown = cache.parent / "notes.txt"
            unknown.write_text("keep this unknown file", encoding="utf-8")

            with self.assertRaises(FixtureRejected):
                validate_bytecode(root, source, unknown, baseline)

            self.assertEqual(unknown.read_text(encoding="utf-8"), "keep this unknown file")

    def test_pytest_probe_never_installs_missing_dependency(self) -> None:
        # The main probe itself decides whether it can run pytest. This verifies
        # the availability check has no installation side effect or path input.
        before = importlib.util.find_spec("pytest") is not None
        result = run_fixture_probe()["pytest"]
        after = importlib.util.find_spec("pytest") is not None
        self.assertEqual(before, after)
        if not before:
            self.assertEqual(result["status"], "unavailable")
            self.assertIn("no install attempted", result["reason"])
        else:
            self.assertIn(result["status"], {"fixture-passed", "inconclusive"})


if __name__ == "__main__":
    unittest.main()
