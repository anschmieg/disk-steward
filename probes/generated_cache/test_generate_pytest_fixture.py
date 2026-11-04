"""Safety/refusal tests; the real pinned manager run is a separate explicit probe."""

import importlib.util
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "generated_cache_fixture", Path(__file__).with_name("generate_pytest_fixture.py")
)
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class FixtureSafety(unittest.TestCase):
    def test_existing_destination_is_preserved_before_any_process(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            interpreter = root / "python"
            interpreter.write_bytes(b"placeholder")
            destination = root / "existing"
            destination.mkdir()
            sentinel = destination / "sentinel"
            sentinel.write_bytes(b"preserve")
            with patch.object(probe.subprocess, "run") as run:
                with self.assertRaises(ValueError):
                    probe.generate(interpreter, destination)
                run.assert_not_called()
            self.assertEqual(sentinel.read_bytes(), b"preserve")

    def test_low_reserve_refuses_without_creating_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            interpreter = root / "python"
            interpreter.touch()
            with (
                patch.object(
                    probe.shutil,
                    "disk_usage",
                    return_value=SimpleNamespace(free=probe.RESERVE - 1),
                ),
                self.assertRaises(ValueError),
            ):
                probe.generate(interpreter, root / "new")
            self.assertFalse((root / "new").exists())

    def test_unknown_version_refuses_before_running_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            interpreter = root / "python"
            interpreter.touch()
            with (
                patch.object(
                    probe.shutil,
                    "disk_usage",
                    return_value=SimpleNamespace(free=probe.RESERVE),
                ),
                patch.object(
                    probe.subprocess,
                    "run",
                    return_value=SimpleNamespace(stdout='["99.0", "3.14"]'),
                ) as run,
            ):
                with self.assertRaises(ValueError):
                    probe.generate(interpreter, root / "new")
                self.assertEqual(run.call_count, 1)
                env = run.call_args.kwargs["env"]
                self.assertEqual(env["PYTEST_DISABLE_PLUGIN_AUTOLOAD"], "1")
            self.assertFalse((root / "new" / "records").exists())
            self.assertTrue((root / "new" / "project" / "test_generated.py").is_file())


if __name__ == "__main__":
    unittest.main()
