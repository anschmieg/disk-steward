"""Crash-boundary reconciliation tests using disposable child processes."""

from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path

from probes.crash.crash_probe import (
    FIXTURE_REL,
    JOURNAL_REL,
    RECEIPT_REL,
    STAGED_REL,
    OwnedRoot,
    _interrupt_case,
    _make_interrupted_fixture,
    reconcile,
    restore,
)


class CrashProbeOwnershipTests(unittest.TestCase):
    def test_root_factory_rejects_caller_supplied_root(self) -> None:
        with self.assertRaises((TypeError, ValueError)):
            OwnedRoot("/tmp/arbitrary-user-path")

    def test_changed_root_identity_refuses_cleanup(self) -> None:
        store = OwnedRoot.create()
        original = store.path
        replacement = original.with_name(original.name + "-replacement")
        original.rename(replacement)
        original.mkdir()
        try:
            with self.assertRaises(ValueError):
                store.cleanup_without_child()
        finally:
            # Test-only cleanup of paths it created after intentionally replacing
            # the root; production cleanup must reject this identity change.
            import shutil

            shutil.rmtree(original)
            shutil.rmtree(replacement)
            from probes.crash import crash_probe

            crash_probe._ACTIVE_ROOTS.pop(store._token, None)


@unittest.skipUnless(hasattr(os, "O_DIRECTORY"), "requires directory fsync support")
class CrashReconciliationTests(unittest.TestCase):
    def test_before_and_after_rename_boundaries_reconcile_without_retry(self) -> None:
        before = _interrupt_case("before_rename")
        after = _interrupt_case("after_rename")
        self.assertEqual(before.recovery.state, "original")
        self.assertIsNotNone(before.original_fingerprint)
        self.assertIsNone(before.staged_fingerprint)
        self.assertEqual(before.journal_phase, "intent")
        self.assertEqual(after.recovery.state, "staged")
        self.assertIsNone(after.original_fingerprint)
        self.assertIsNotNone(after.staged_fingerprint)
        self.assertEqual(after.journal_phase, "intent")
        self.assertTrue(before.cleaned)
        self.assertTrue(after.cleaned)

    def test_explicit_restore_accepts_only_unchanged_staged_fixture(self) -> None:
        store, stopped = _make_interrupted_fixture("after_rename")
        try:
            self.assertEqual(reconcile(store)[0].state, "staged")
            restore(store)
            recovery, _, original, staged = reconcile(store)
            self.assertEqual(recovery.state, "original")
            self.assertIsNotNone(original)
            self.assertIsNone(staged)
        finally:
            store.cleanup(stopped)

    def test_changed_fixture_blocks_restore(self) -> None:
        store, stopped = _make_interrupted_fixture("after_rename")
        try:
            staged = store.path / STAGED_REL
            with staged.open("r+b") as file:
                file.seek(0)
                file.write(b"changed!")
                file.flush()
                os.fsync(file.fileno())
            self.assertEqual(reconcile(store)[0].state, "uncertain")
            with self.assertRaisesRegex(ValueError, "restore blocked"):
                restore(store)
            self.assertTrue(staged.exists())
            self.assertFalse((store.path / FIXTURE_REL).exists())
        finally:
            store.cleanup(stopped)

    def test_original_slot_collision_blocks_restore_without_overwrite(self) -> None:
        store, stopped = _make_interrupted_fixture("after_rename")
        try:
            collision = store.path / FIXTURE_REL
            collision.write_bytes(b"unrelated collision")
            staged = store.path / STAGED_REL
            staged_before = staged.read_bytes()
            self.assertEqual(reconcile(store)[0].state, "uncertain")
            with self.assertRaisesRegex(ValueError, "restore blocked"):
                restore(store)
            self.assertEqual(collision.read_bytes(), b"unrelated collision")
            self.assertEqual(staged.read_bytes(), staged_before)
        finally:
            store.cleanup(stopped)

    def test_tampered_receipt_blocks_restore_and_journal_paths_are_ignored(self) -> None:
        store, stopped = _make_interrupted_fixture("after_rename")
        try:
            receipt_path = store.path / RECEIPT_REL
            receipt = json.loads(receipt_path.read_bytes())
            receipt["destination_slot"] = "../../outside/fixture.bin"
            receipt_path.write_text(json.dumps(receipt), encoding="utf-8")
            self.assertEqual(reconcile(store)[0].state, "uncertain")
            with self.assertRaisesRegex(ValueError, "restore blocked"):
                restore(store)
            self.assertTrue((store.path / STAGED_REL).exists())
            self.assertFalse((store.path.parent / "outside").exists())
            self.assertEqual((store.path / JOURNAL_REL).exists(), True)
        finally:
            store.cleanup(stopped)

    def test_replaced_parent_symlink_blocks_reconcile_restore_and_cleanup(self) -> None:
        store, stopped = _make_interrupted_fixture("after_rename")
        staged_parent = store.path / "staged"
        moved_parent = store.path / "staged-original"
        try:
            with tempfile.TemporaryDirectory(prefix="crash-outside-") as outside_dir:
                outside = Path(outside_dir)
                staged_parent.rename(moved_parent)
                staged_parent.symlink_to(outside, target_is_directory=True)
                self.assertEqual(reconcile(store)[0].state, "uncertain")
                with self.assertRaisesRegex(ValueError, "restore blocked"):
                    restore(store)
                with self.assertRaisesRegex(ValueError, "identity changed"):
                    store.cleanup(stopped)
                self.assertTrue(outside.exists())
                self.assertTrue((outside / "fixture.bin").exists() is False)
                staged_parent.unlink()
                moved_parent.rename(staged_parent)
            store.cleanup(stopped)
        except BaseException:
            if staged_parent.is_symlink():
                staged_parent.unlink()
            if moved_parent.exists() and not staged_parent.exists():
                moved_parent.rename(staged_parent)
            raise


if __name__ == "__main__":
    unittest.main()
