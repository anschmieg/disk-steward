"""Unit coverage for private-store ownership and hard-stop accounting."""

from __future__ import annotations

import unittest
from pathlib import Path
import tempfile

from probes.worker.worker_probe import (
    BYTE_BUDGET_BYTES,
    BYTE_FIXTURE_CAP_BYTES,
    TIMEOUT_SECONDS,
    OwnedStore,
    StoppedReceipt,
    _run_case,
)


class WorkerProbeTests(unittest.TestCase):
    def test_rejects_paths_outside_the_owned_store(self) -> None:
        store = OwnedStore.create()
        outside = store.root.parent / "unrelated-file.bin"
        with self.assertRaises(ValueError):
            store.validate_output(outside)
        store.cleanup_without_child()

    def test_rejects_forged_store_and_cleanup_without_supervisor_receipt(self) -> None:
        store = OwnedStore.create()
        with self.assertRaises((TypeError, ValueError)):
            OwnedStore(store.root, store.output)
        with self.assertRaises(ValueError):
            StoppedReceipt(None, 12345, -15, _authority=None)
        with self.assertRaises(ValueError):
            store.cleanup(None)
        store.cleanup_without_child()

    def test_rejects_symlinked_root_without_touching_target(self) -> None:
        store = OwnedStore.create()
        with tempfile.TemporaryDirectory(prefix="worker-outside-") as outside_dir:
            outside = Path(outside_dir)
            store.root.rmdir()
            store.root.symlink_to(outside, target_is_directory=True)
            with self.assertRaises(ValueError):
                store.cleanup_without_child()
            self.assertTrue(outside.exists())
            self.assertTrue(store.root.is_symlink())
            store.root.unlink()
        # This test intentionally replaced its own owned directory. Remove only
        # the empty test parent and discard the stale registration.
        store.root.parent.rmdir()
        from probes.worker import worker_probe

        worker_probe._ACTIVE_STORES.pop(store._token, None)

    def test_byte_budget_failure_is_bounded_and_child_is_stopped_before_cleanup(self) -> None:
        result = _run_case("byte_budget")
        self.assertEqual(result.failure, "byte_budget")
        self.assertGreater(result.max_observed_allocated_growth_bytes, BYTE_BUDGET_BYTES)
        self.assertLessEqual(
            result.max_observed_allocated_growth_bytes,
            BYTE_FIXTURE_CAP_BYTES,
        )
        self.assertEqual(
            result.byte_budget_overshoot_bytes,
            result.max_observed_allocated_growth_bytes - BYTE_BUDGET_BYTES,
        )
        self.assertTrue(result.termination_requested)
        self.assertIsNotNone(result.exit_code)
        self.assertTrue(result.store_cleaned)

    def test_deadline_failure_reports_measured_overshoot_and_cleans_after_join(self) -> None:
        result = _run_case("deadline")
        self.assertEqual(result.failure, "deadline")
        self.assertGreaterEqual(result.elapsed_seconds, TIMEOUT_SECONDS)
        self.assertGreaterEqual(
            result.deadline_overshoot_seconds,
            result.elapsed_seconds - TIMEOUT_SECONDS,
        )
        self.assertTrue(result.termination_requested)
        self.assertIsNotNone(result.exit_code)
        self.assertTrue(result.store_cleaned)


if __name__ == "__main__":
    unittest.main()
