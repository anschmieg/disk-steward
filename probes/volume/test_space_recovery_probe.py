import io
import plistlib
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

from probes.volume import space_recovery_probe as probe_module
from probes.volume.space_recovery_probe import (
    ProbeError, attach_args, create_args, detach_args, returned_device, _within,
)


class VolumeProbeSafetyTests(unittest.TestCase):
    @staticmethod
    def _fake_fixture(path):
        allocated = 32 * 1024 * 1024
        path.write_bytes(b"x" * allocated)
        return allocated

    @staticmethod
    def _attach_result(args, device):
        mounted = Path(args[args.index("-mountpoint") + 1]).resolve()
        payload = plistlib.dumps({"system-entities": [
            {"dev-entry": device, "mount-point": str(mounted)}
        ]})
        return 0, payload, b""

    def test_hdiutil_command_arguments_and_device_safety(self):
        image = Path("/tmp/own-probe/probe.dmg")
        mount = Path("/tmp/own-probe/mounted")
        self.assertEqual(create_args(image)[-1], str(image))
        self.assertEqual(attach_args(image, mount)[-2:], [str(mount), str(image)])
        self.assertEqual(detach_args("/dev/disk7s2"), ["/usr/bin/hdiutil", "detach", "/dev/disk7s2"])
        for unsafe in ("/dev/disk7; arbitrary-command", "/dev/disk7s2s3"):
            with self.assertRaises(ProbeError):
                detach_args(unsafe)

    def test_device_receipt_must_name_our_mountpoint(self):
        mount = Path("/tmp/owned-volume").resolve()
        payload = plistlib.dumps({"system-entities": [
            {"dev-entry": "/dev/disk8s2", "mount-point": str(mount)}
        ]})
        self.assertEqual(returned_device(payload, mount), "/dev/disk8s2")
        with self.assertRaises(ProbeError):
            returned_device(payload, Path("/tmp/other-volume"))

    def test_receipt_containment_rejects_sibling_and_parent(self):
        root = Path("/tmp/volume-probe-root")
        self.assertEqual(_within(root, root / "receipt.json"), (root / "receipt.json").resolve())
        for candidate in (Path("/tmp/other/receipt.json"), Path("/tmp/volume-probe-root-parent/file")):
            with self.assertRaises(ProbeError):
                _within(root, candidate)

    def _run_fake(self, base, device, recovered, detach_code=0):
        calls = []

        def fake_mount(args):
            calls.append(list(args))
            if args[1] == "attach":
                return self._attach_result(args, device)
            return detach_code, b"", b"detach denied" if detach_code else b""

        volume_reads = iter([4 * 1024**3] * 5 + [4 * 1024**3 + recovered])

        def fake_free(path):
            return next(volume_reads) if path.name == "mounted" else 10 * 1024**3

        with patch.object(probe_module.sys, "platform", "darwin"), \
             patch.object(probe_module, "_free_bytes", side_effect=fake_free), \
             patch.object(probe_module, "_write_fixture", side_effect=self._fake_fixture):
            result = probe_module.run_probe(
                Path(base), run_command=lambda args: Path(args[-1]).touch(),
                mount_command=fake_mount, recovery_timeout_seconds=0,
            )
        return result, calls

    def test_success_detaches_only_returned_device_then_removes_private_root(self):
        with tempfile.TemporaryDirectory() as base:
            result, calls = self._run_fake(base, "/dev/disk12s2", 32 * 1024**2)
            self.assertEqual(result["status"], "complete")
            self.assertTrue(result["recovery_meets_tolerance"])
            self.assertEqual(result["fixture_allocated_bytes"], 32 * 1024**2)
            self.assertEqual(calls[-1], ["/usr/bin/hdiutil", "detach", "/dev/disk12s2"])
            self.assertFalse(Path(result["image"]).parent.exists())

    def test_failed_detach_retains_image_and_never_reports_success(self):
        with tempfile.TemporaryDirectory() as base:
            result, _ = self._run_fake(base, "/dev/disk13s2", 32 * 1024**2, detach_code=1)
            root = Path(result["retained_root"])
            self.assertEqual(result["status"], "detach_failed")
            self.assertTrue((root / "probe.dmg").exists())
            self.assertTrue((root / "receipt.json").exists())
            self.assertEqual(result["detach_detail"], "detach denied")

    def test_zero_recovery_is_failed_and_cli_returns_nonzero(self):
        with tempfile.TemporaryDirectory() as base:
            result, _ = self._run_fake(base, "/dev/disk14s2", 0)
            self.assertEqual(result["status"], "recovery_not_observed")
            self.assertFalse(result["recovery_meets_tolerance"])

        output = io.StringIO()
        with patch.object(probe_module, "run_probe", return_value={
            "status": "recovery_not_observed", "recovery_meets_tolerance": False,
        }), redirect_stdout(output):
            self.assertEqual(probe_module.main(["--run"]), 1)


if __name__ == "__main__":
    unittest.main()
