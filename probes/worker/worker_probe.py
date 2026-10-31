"""Developer-only wall-clock deadline and byte-budget child-writer probe.

The child is fixed code, writes a fixed name under a freshly-created private
temporary directory, has a fixed fixture cap, and has no command/network input.
This probe measures process supervision and disposable-store accounting only.
"""

from __future__ import annotations

import argparse
import multiprocessing
import os
import shutil
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

CHUNK_BYTES = 4 * 1024
BYTE_BUDGET_BYTES = 64 * 1024
BYTE_FIXTURE_CAP_BYTES = 256 * 1024
TIMEOUT_FIXTURE_CAP_BYTES = 1024 * 1024
TIMEOUT_SECONDS = 0.15
CHILD_DELAY_SECONDS = 0.005
POLL_INTERVAL_SECONDS = 0.002
STARTUP_TIMEOUT_SECONDS = 5.0
STOP_TIMEOUT_SECONDS = 2.0
STORE_NAME = "worker-store"
OUTPUT_NAME = "synthetic-child-output.bin"


class StoppedReceipt:
    """Parent-side proof that join observed this exact child process exit."""

    __slots__ = ("process", "process_id", "exit_code", "_authority")

    def __init__(self, process, process_id: int, exit_code: int, *, _authority) -> None:
        if _authority is not _RECEIPT_AUTHORITY:
            raise ValueError("stopped receipts can only be issued by the process supervisor")
        self.process = process
        self.process_id = process_id
        self.exit_code = exit_code
        self._authority = _authority


@dataclass(frozen=True)
class RunResult:
    scenario: str
    failure: Literal["byte_budget", "deadline", "fixture_cap", "child_error"]
    byte_budget_bytes: int
    fixture_cap_bytes: int
    max_observed_allocated_growth_bytes: int
    byte_budget_overshoot_bytes: int
    deadline_seconds: float | None
    elapsed_seconds: float
    deadline_overshoot_seconds: float
    process_id: int
    exit_code: int
    termination_requested: bool
    store_cleaned: bool


class OwnedStore:
    """Private temporary store with a fixed child output name."""

    __slots__ = ("root", "output", "_token", "_child")

    def __init__(self, root: Path, output: Path, *, _authority) -> None:
        if _authority is not _STORE_AUTHORITY:
            raise ValueError("worker stores must be created by the private store factory")
        self.root = root
        self.output = output
        self._token = object()
        self._child = None

    @classmethod
    def create(cls) -> "OwnedStore":
        private_parent = Path(tempfile.mkdtemp(prefix="disk-steward-worker-"))
        root = private_parent / STORE_NAME
        root.mkdir(mode=0o700)
        store = cls(root=root, output=root / OUTPUT_NAME, _authority=_STORE_AUTHORITY)
        _ACTIVE_STORES[store._token] = _directory_identity(root, private_parent)
        return store

    def bind_child(self, process: multiprocessing.Process) -> None:
        self._verify_identity()
        if self._child is not None or process.pid is None:
            raise ValueError("worker store can be bound to exactly one started child")
        self._child = process

    def _verify_identity(self) -> None:
        identity = _ACTIVE_STORES.get(self._token)
        if identity is None or identity != _directory_identity(self.root, self.root.parent):
            raise ValueError("worker store identity changed or is not registered")

    def validate_output(self, candidate: Path) -> Path:
        self._verify_identity()
        expected = self.root / OUTPUT_NAME
        requested = Path(os.path.abspath(candidate))
        if requested != expected or candidate.name != OUTPUT_NAME:
            raise ValueError("child output must be the fixed file in this private store")
        if self.output.is_symlink():
            raise ValueError("symlinked worker paths are rejected")
        return expected

    def cleanup(self, receipt: StoppedReceipt) -> None:
        if not isinstance(receipt, StoppedReceipt) or receipt._authority is not _RECEIPT_AUTHORITY:
            raise ValueError("store cleanup requires a confirmed stopped-child receipt")
        self._verify_identity()
        if receipt.process is not self._child or receipt.process_id != self._child.pid:
            raise ValueError("stopped receipt does not match this store's child")
        if receipt.process.is_alive() or receipt.process.exitcode != receipt.exit_code:
            raise ValueError("child process is not confirmed stopped")
        if receipt.exit_code is None or not _issued_receipts.pop(id(receipt), None):
            raise ValueError("stopped receipt was not issued by the process supervisor")
        if self.output.parent != self.root:
            raise ValueError("refusing cleanup outside the owned worker store")
        if self.output.is_symlink():
            raise ValueError("refusing cleanup through a symlinked worker output")
        shutil.rmtree(self.root)
        # Remove only the empty, private directory created beside the fixed store.
        self.root.parent.rmdir()
        del _ACTIVE_STORES[self._token]

    def cleanup_without_child(self) -> None:
        """Remove an unused store only when no child process was started."""
        self._verify_identity()
        if self._child is not None:
            raise ValueError("cannot remove a store after a child process has been started")
        if self.output.exists() or self.output.is_symlink():
            raise ValueError("cannot remove a populated store without a stopped-child receipt")
        self.root.rmdir()
        self.root.parent.rmdir()
        del _ACTIVE_STORES[self._token]


_STORE_AUTHORITY = object()
_RECEIPT_AUTHORITY = object()
_ACTIVE_STORES: dict[object, tuple[int, int, int, int]] = {}
_issued_receipts: dict[int, StoppedReceipt] = {}


def _directory_identity(root: Path, parent: Path) -> tuple[int, int, int, int]:
    root_info = root.lstat()
    parent_info = parent.lstat()
    if root.is_symlink() or parent.is_symlink() or not root.is_dir() or not parent.is_dir():
        raise ValueError("worker store directories must be real directories")
    return root_info.st_dev, root_info.st_ino, parent_info.st_dev, parent_info.st_ino


def allocated_bytes(path: Path) -> int:
    """Return observed allocated bytes (or logical bytes if blocks are absent)."""
    try:
        stat = path.stat()
    except FileNotFoundError:
        return 0
    blocks = getattr(stat, "st_blocks", None)
    return int(blocks * 512 if blocks is not None else stat.st_size)


def _fixed_child_writer(
    output_path: str,
    ready,
    fixture_cap_bytes: int,
    delay_seconds: float,
    hold_event,
) -> None:
    """Only child target: exclusive-create one synthetic file, write fixed chunks."""
    written = 0
    with open(output_path, "xb", buffering=0) as output:
        ready.set()
        chunk = b"W" * CHUNK_BYTES
        while written + CHUNK_BYTES <= fixture_cap_bytes:
            output.write(chunk)
            written += CHUNK_BYTES
            if delay_seconds:
                time.sleep(delay_seconds)
    # Keep the child alive so the parent must explicitly stop and join it.
    hold_event.wait()


def _stop_and_join(process: multiprocessing.Process) -> tuple[StoppedReceipt, bool]:
    """Stop if needed, then return only after the OS reports the child exited."""
    termination_requested = process.is_alive()
    if termination_requested:
        process.terminate()
        process.join(STOP_TIMEOUT_SECONDS)
    else:
        process.join()
    if process.is_alive() and hasattr(process, "kill"):
        termination_requested = True
        process.kill()
        process.join(STOP_TIMEOUT_SECONDS)
    if process.is_alive() or process.pid is None or process.exitcode is None:
        raise RuntimeError("child stop was not confirmed; worker store must be retained")
    receipt = StoppedReceipt(
        process,
        process.pid,
        process.exitcode,
        _authority=_RECEIPT_AUTHORITY,
    )
    _issued_receipts[id(receipt)] = receipt
    return receipt, termination_requested


def _run_case(
    scenario: Literal["byte_budget", "deadline"],
    *,
    context=None,
) -> RunResult:
    if scenario not in ("byte_budget", "deadline"):
        raise ValueError("unknown fixed worker scenario")
    multiprocessing_context = context or multiprocessing.get_context("spawn")
    store = OwnedStore.create()
    process = None
    try:
        store.validate_output(store.output)
        ready = multiprocessing_context.Event()
        hold_event = multiprocessing_context.Event()

        if scenario == "byte_budget":
            budget = BYTE_BUDGET_BYTES
            fixture_cap = BYTE_FIXTURE_CAP_BYTES
            deadline = None
            child_delay = 0.0
        else:
            budget = TIMEOUT_FIXTURE_CAP_BYTES
            fixture_cap = TIMEOUT_FIXTURE_CAP_BYTES
            deadline = TIMEOUT_SECONDS
            child_delay = CHILD_DELAY_SECONDS

        process = multiprocessing_context.Process(
            target=_fixed_child_writer,
            args=(str(store.output), ready, fixture_cap, child_delay, hold_event),
            name=f"disk-steward-worker-probe-{scenario}",
            daemon=True,
        )
        process.start()
        if process.pid is None:
            raise RuntimeError("child process did not report a process ID")
        store.bind_child(process)
    except BaseException as start_error:
        if process is None:
            try:
                store.cleanup_without_child()
            except BaseException as cleanup_error:
                raise RuntimeError(
                    f"worker setup failed ({start_error}); empty-store cleanup failed "
                    f"({cleanup_error}); retained worker store: {store.root}"
                ) from cleanup_error
            raise
        if process.pid is None:
            try:
                store.cleanup_without_child()
            except BaseException as cleanup_error:
                raise RuntimeError(
                    f"child startup failed ({start_error}); empty-store cleanup failed "
                    f"({cleanup_error}); retained worker store: {store.root}"
                ) from cleanup_error
            raise

        # Process creation may fail after the OS has started the child. In that
        # case, always wait for exit; clean only if store ownership was bound.
        bound_store = store._child is process
        if not bound_store:
            try:
                store.bind_child(process)
                bound_store = True
            except BaseException:
                pass
        try:
            receipt, _ = _stop_and_join(process)
            if bound_store:
                store.cleanup(receipt)
        except BaseException as stop_error:
            raise RuntimeError(
                f"child startup failed ({start_error}); stop/cleanup was not confirmed "
                f"({stop_error}); retained worker store: {store.root}"
            ) from stop_error
        raise RuntimeError(
            f"child startup failed after process creation ({start_error}); "
            f"child stopped and store {'cleaned' if bound_store else 'retained'} at {store.root}"
        ) from start_error

    try:
        startup_deadline = time.monotonic() + STARTUP_TIMEOUT_SECONDS
        while not ready.is_set():
            if not process.is_alive():
                raise RuntimeError(
                    f"fixed child exited before readiness (exit code {process.exitcode})"
                )
            remaining = startup_deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError("fixed child did not become ready before the startup deadline")
            ready.wait(min(POLL_INTERVAL_SECONDS, remaining))
        if not ready.is_set():
            raise RuntimeError("fixed child did not become ready before the startup deadline")
        start = time.monotonic()
        failure: Literal["byte_budget", "deadline", "fixture_cap", "child_error"] = "fixture_cap"
        max_observed_growth = 0
        timed_out = False

        while True:
            if not process.is_alive() and process.exitcode not in (None, 0):
                failure = "child_error"
                break
            observed_growth = allocated_bytes(store.output)
            max_observed_growth = max(max_observed_growth, observed_growth)
            elapsed = time.monotonic() - start
            if scenario == "byte_budget" and observed_growth > budget:
                failure = "byte_budget"
                break
            if deadline is not None and elapsed >= deadline:
                failure = "deadline"
                timed_out = True
                break
            if not process.is_alive():
                if scenario == "byte_budget" and observed_growth > budget:
                    failure = "byte_budget"
                break
            time.sleep(POLL_INTERVAL_SECONDS)

        receipt, termination_requested = _stop_and_join(process)
        elapsed = time.monotonic() - start
        max_observed_growth = max(max_observed_growth, allocated_bytes(store.output))
        if not termination_requested and receipt.exit_code != 0:
            failure = "child_error"
        elif failure == "child_error":
            pass
        elif scenario == "byte_budget" and max_observed_growth > budget:
            failure = "byte_budget"
        elif timed_out:
            failure = "deadline"
        else:
            failure = "fixture_cap"

        store.cleanup(receipt)
        return RunResult(
            scenario=scenario,
            failure=failure,
            byte_budget_bytes=budget,
            fixture_cap_bytes=fixture_cap,
            max_observed_allocated_growth_bytes=max_observed_growth,
            byte_budget_overshoot_bytes=max(0, max_observed_growth - budget),
            deadline_seconds=deadline,
            elapsed_seconds=elapsed,
            deadline_overshoot_seconds=(
                max(0.0, elapsed - deadline) if deadline is not None else 0.0
            ),
            process_id=receipt.process_id,
            exit_code=receipt.exit_code,
            termination_requested=termination_requested,
            store_cleaned=True,
        )
    except BaseException as original_error:
        # Never delete the fixture until join has confirmed the child stopped.
        try:
            receipt, _ = _stop_and_join(process)
            store.cleanup(receipt)
        except BaseException as stop_error:
            # Preserve a possibly-live child's store for manual investigation.
            raise RuntimeError(
                f"{original_error}; child stop/cleanup was not confirmed "
                f"({stop_error}); retained worker store: {store.root}"
            ) from stop_error
        raise


def _print_dry_run() -> None:
    print("Dry run: no child process or temporary files created.")
    print(
        "--run starts two fixed disposable writers: "
        f"byte budget {BYTE_BUDGET_BYTES} B, fixture cap {BYTE_FIXTURE_CAP_BYTES} B; "
        f"deadline {TIMEOUT_SECONDS:.2f} s, fixture cap {TIMEOUT_FIXTURE_CAP_BYTES} B."
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--run",
        action="store_true",
        help="run only the fixed synthetic child-writer probes in private temporary stores",
    )
    args = parser.parse_args(argv)
    if not args.run:
        _print_dry_run()
        return 0

    try:
        results = [_run_case("byte_budget"), _run_case("deadline")]
    except Exception as error:
        print(f"Probe failed: {error}")
        return 1
    all_passed = True
    for result in results:
        print(
            f"{result.scenario}: failure={result.failure}; "
            f"observed_allocated_growth={result.max_observed_allocated_growth_bytes} B; "
            f"budget={result.byte_budget_bytes} B; "
            f"byte_overshoot={result.byte_budget_overshoot_bytes} B; "
            f"elapsed={result.elapsed_seconds:.3f} s; "
            f"deadline_overshoot={result.deadline_overshoot_seconds:.3f} s; "
            f"child_exit={result.exit_code}; termination_requested={result.termination_requested}; "
            f"store_cleaned={result.store_cleaned}"
        )
        expected = "byte_budget" if result.scenario == "byte_budget" else "deadline"
        all_passed &= result.failure == expected and result.store_cleaned
    return 0 if all_passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
