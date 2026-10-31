import unittest

from recovery_probe import DisposableRecoveryProbe, ProbeError


class RecoveryProbeTests(unittest.TestCase):
    def test_stage_rejects_traversal_and_unregistered_files(self):
        with DisposableRecoveryProbe() as probe:
            outside = probe.root / "outside.bin"
            outside.write_bytes(b"preserve")
            unrelated = probe.source / "unregistered.bin"
            unrelated.write_bytes(b"preserve")
            for name in ("../outside.bin", "unregistered.bin", str(outside)):
                with self.assertRaises(ProbeError):
                    probe.stage(name)
            self.assertEqual(outside.read_bytes(), b"preserve")
            self.assertEqual(unrelated.read_bytes(), b"preserve")

    def test_stage_restore_rejects_collision_and_leaves_unrelated_files(self):
        with DisposableRecoveryProbe() as probe:
            staged = probe.create_fixture("sample.bin", b"fixture")
            unrelated = probe.source / "unrelated.txt"
            unrelated.write_bytes(b"keep")
            receipt = probe.stage("sample.bin")
            collision = probe.source / "sample.bin"
            collision.write_bytes(b"new user content")
            with self.assertRaisesRegex(ProbeError, "collision"):
                probe.restore(receipt)
            self.assertEqual(collision.read_bytes(), b"new user content")
            collision.unlink()
            restored = probe.restore(receipt)
            self.assertEqual(restored.read_bytes(), b"fixture")
            self.assertEqual(unrelated.read_bytes(), b"keep")
            self.assertFalse((probe.holding / receipt).exists())
            self.assertEqual(staged, restored)

    def test_changed_staged_file_cannot_be_purged(self):
        with DisposableRecoveryProbe() as probe:
            probe.create_fixture("sample.bin", b"original")
            receipt = probe.stage("sample.bin")
            held = probe.holding / receipt
            held.write_bytes(b"changed")
            self.assertEqual(probe.reconcile()[receipt], "changed")
            with self.assertRaisesRegex(ProbeError, "identity changed"):
                probe.purge(receipt)
            self.assertEqual(held.read_bytes(), b"changed")

    def test_selective_purge_touches_only_recorded_fixture(self):
        with DisposableRecoveryProbe() as probe:
            probe.create_fixture("target.bin", b"discard fixture")
            receipt = probe.stage("target.bin")
            unrelated = probe.holding / "unrelated"
            unrelated.write_bytes(b"preserve")
            probe.purge(receipt)
            self.assertFalse((probe.holding / receipt).exists())
            self.assertEqual(unrelated.read_bytes(), b"preserve")
            self.assertEqual(probe.reconcile()[receipt], "purged")

    def test_reconcile_reports_interrupted_move_without_deleting(self):
        with DisposableRecoveryProbe() as probe:
            original = probe.create_fixture("sample.bin", b"fixture")
            receipt = probe.stage("sample.bin")
            held = probe.holding / receipt
            held.rename(original)
            # Simulate a crash after movement and before durable state update.
            probe.records[receipt]["state"] = "staged"
            self.assertEqual(probe.reconcile()[receipt], "restored-before-record")
            self.assertEqual(original.read_bytes(), b"fixture")


if __name__ == "__main__":
    unittest.main()
