import json
import plistlib
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from probes.volume import native_observation_probe as probe
from probes.volume import space_recovery_probe as space


class NativeObservationProbeTests(unittest.TestCase):
    def test_missing_authorizations_fail_before_any_image_command(self):
        calls = []
        with patch.object(probe.sys, "platform", "darwin"), \
             self.assertRaisesRegex(probe.NativeProbeError, "before image creation"):
            probe.run_probe(run_command=lambda args: calls.append(list(args)),
                            purge_authorized=True)
        self.assertEqual(calls, [])

    def test_purge_report_requires_correct_method_and_digest(self):
        valid = {"state": "finished", "selectedLogicalBytes": 123,
                 "durablyUnlinkedLogicalBytes": 123,
                 "volumeObservation": {"method": probe.METHOD,
                                       "observedFreeSpaceDeltaBytes": 99,
                                       "volumeBindingSha256": "a" * 64}}
        probe._validate_purge(valid)
        for key, value in (("method", "other"), ("volumeBindingSha256", "x" * 64)):
            bad = json.loads(json.dumps(valid))
            bad["volumeObservation"][key] = value
            with self.subTest(key=key), self.assertRaises(probe.NativeProbeError):
                probe._validate_purge(bad)
        bad = dict(valid, durablyUnlinkedLogicalBytes=122)
        with self.assertRaises(probe.NativeProbeError):
            probe._validate_purge(bad)

    def test_calibration_requires_expected_size_and_recovery_tolerance(self):
        calibration = {"method": probe.METHOD, "fixtureBytes": 256 * 1024**2,
                       "allocatedBytes": 256 * 1024**2,
                       "observedFreeSpaceDeltaBytes": 256 * 1024**2,
                       "toleranceBytes": 256 * 1024**2 // 10, "elapsedMs": 500}
        probe._validate_calibration(calibration)
        for key, value in (("method", "wrong"), ("fixtureBytes", 1),
                           ("allocatedBytes", space.IMAGE_BUDGET_BYTES + 1),
                           ("toleranceBytes", 16 * 1024**2),
                           ("observedFreeSpaceDeltaBytes", 0), ("elapsedMs", 36_000)):
            bad = dict(calibration, **{key: value})
            with self.subTest(key=key), self.assertRaises(probe.NativeProbeError):
                probe._validate_calibration(bad)

    def test_success_records_evidence_and_detaches_only_receipted_device(self):
        with tempfile.TemporaryDirectory() as base:
            helper_file = Path(base) / "helper"
            helper_file.touch()
            calls = []
            def mount(args):
                calls.append(list(args))
                if args[1] == "attach":
                    mounted = Path(args[args.index("-mountpoint") + 1]).resolve()
                    calls.append(list(args))
                    payload = plistlib.dumps({"system-entities": [
                        {"dev-entry": "/dev/disk91s2", "mount-point": str(mounted)}]})
                    return 0, payload, b""
                # A successful detach makes mounted contents disappear from the host view.
                mounted = Path(calls[0][calls[0].index("-mountpoint") + 1])
                calls.append(list(args))
                marker = mounted / "home" / probe.HOME_MARKER
                marker.unlink()
                (mounted / "home").rmdir()
                return 0, b"", b""

            calibration = {"method": probe.METHOD, "fixtureBytes": 256 * 1024**2,
                           "allocatedBytes": 256 * 1024**2,
                           "observedFreeSpaceDeltaBytes": 256 * 1024**2,
                           "toleranceBytes": 256 * 1024**2 // 10, "elapsedMs": 500}
            product = {"state": "finished", "selectedLogicalBytes": 100,
                       "durablyUnlinkedLogicalBytes": 100,
                       "volumeObservation": {"method": probe.METHOD,
                                             "observedFreeSpaceDeltaBytes": 80,
                                             "volumeBindingSha256": "b" * 64}}
            commands = []
            def helper(home, command):
                commands.append(command)
                if command == "seed":
                    self.assertEqual((home / probe.HOME_MARKER).read_text(), probe.HOME_MARKER_VALUE)
                    return {"status": "seeded"}
                return calibration if command.startswith("calibrate") else product

            with patch.object(probe.sys, "platform", "darwin"), \
                 patch.object(probe, "HELPER", helper_file), \
                 patch.object(space, "_free_bytes", return_value=10 * 1024**3):
                result = probe.run_probe(Path(base),
                    run_command=lambda args: Path(args[-1]).touch(), mount_command=mount,
                    helper_command=helper, purge_authorized=True, calibration_authorized=True)
            self.assertEqual(result["status"], "complete")
            self.assertTrue(result["image_detached"])
            self.assertTrue(result["temporary_root_removed"])
            self.assertEqual(result["platform"], "darwin")
            self.assertEqual(result["method"], probe.METHOD)
            self.assertEqual(result["calibration"], calibration)
            self.assertEqual(result["product"], product)
            self.assertIn("not attributed", result["evidence_scope"])
            self.assertEqual(commands, ["seed", f"calibrate-own-volume {probe.CALIBRATION_CONFIRMATION}",
                                       f"purge-own-staged-demo {probe.PURGE_CONFIRMATION}"])
            self.assertEqual(calls[-1], ["/usr/bin/hdiutil", "detach", "/dev/disk91s2"])
            self.assertFalse(Path(result["image"]).parent.exists())

    def test_detach_failure_retains_image_and_report(self):
        with tempfile.TemporaryDirectory() as base:
            helper_file = Path(base) / "helper"
            helper_file.touch()
            def mount(args):
                if args[1] == "attach":
                    mounted = Path(args[args.index("-mountpoint") + 1]).resolve()
                    return 0, plistlib.dumps({"system-entities": [
                        {"dev-entry": "/dev/disk92s2", "mount-point": str(mounted)}]}), b""
                return 1, b"", b"detach failed"
            calibration = {"method": probe.METHOD, "fixtureBytes": 256 * 1024**2,
                           "allocatedBytes": 256 * 1024**2,
                           "observedFreeSpaceDeltaBytes": 256 * 1024**2,
                           "toleranceBytes": 16 * 1024**2, "elapsedMs": 500}
            product = {"state": "finished", "selectedLogicalBytes": 1,
                       "durablyUnlinkedLogicalBytes": 1,
                       "volumeObservation": {"method": probe.METHOD,
                                             "observedFreeSpaceDeltaBytes": 0,
                                             "volumeBindingSha256": "c" * 64}}
            def helper(home, command):
                return {"status": "seeded"} if command == "seed" else calibration if command.startswith("calibrate") else product
            with patch.object(probe.sys, "platform", "darwin"), \
                 patch.object(probe, "HELPER", helper_file), \
                 patch.object(space, "_free_bytes", return_value=10 * 1024**3):
                result = probe.run_probe(Path(base), run_command=lambda args: Path(args[-1]).touch(),
                    mount_command=mount, helper_command=helper,
                    purge_authorized=True, calibration_authorized=True)
            root = Path(result["retained_root"])
            self.assertEqual(result["status"], "detach_failed")
            self.assertTrue((root / "probe.dmg").exists())
            self.assertTrue((root / "receipt.json").exists())
            self.assertIn("detach failed", result["detach_detail"])

    def test_helper_failure_and_detach_failure_return_retained_report(self):
        with tempfile.TemporaryDirectory() as base:
            helper_file = Path(base) / "helper"
            helper_file.touch()
            def mount(args):
                if args[1] == "attach":
                    mounted = Path(args[args.index("-mountpoint") + 1]).resolve()
                    return 0, plistlib.dumps({"system-entities": [
                        {"dev-entry": "/dev/disk93s2", "mount-point": str(mounted)}]}), b""
                return 1, b"", b"detach denied"
            def helper(home, command):
                raise probe.NativeProbeError("controlled helper failure")
            with patch.object(probe.sys, "platform", "darwin"), \
                 patch.object(probe, "HELPER", helper_file), \
                 patch.object(space, "_free_bytes", return_value=10 * 1024**3):
                result = probe.run_probe(Path(base), run_command=lambda args: Path(args[-1]).touch(),
                    mount_command=mount, helper_command=helper,
                    purge_authorized=True, calibration_authorized=True)
            root = Path(result["retained_root"])
            self.assertEqual(result["status"], "detach_failed")
            self.assertEqual(result["detail"], "controlled helper failure")
            self.assertEqual(result["detach_detail"], "detach denied")
            self.assertTrue((root / "probe.dmg").exists())
            self.assertTrue((root / "receipt.json").exists())
            receipt = json.loads((root / "receipt.json").read_text())
            self.assertEqual(receipt["detail"], "controlled helper failure")


if __name__ == "__main__":
    unittest.main()
