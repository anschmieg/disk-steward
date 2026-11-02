"""Developer-only interruption/reconciliation probe for disposable staging.

The only worker is fixed code operating on one internally-created synthetic
fixture. This checks local process interruption and recovery bookkeeping; it is
not production cleanup authority and does not test native Trash or power loss.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import multiprocessing
import os
import shutil
import stat
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

FIXTURE_ID = "synthetic-crash-fixture-v1"
FIXTURE_REL = Path("original") / "fixture.bin"
STAGED_REL = Path("staged") / "fixture.bin"
JOURNAL_REL = Path("journal.json")
RECEIPT_REL = Path("stage-receipt.json")
FIXTURE_SIZE = 128 * 1024
CONTROL_MAX_BYTES = 16 * 1024
START_TIMEOUT_SECONDS = 5.0
JOIN_TIMEOUT_SECONDS = 3.0
STORE_PREFIX = "disk-steward-crash-"

_STORE_AUTHORITY = object()
_STOP_AUTHORITY = object()
_ACTIVE_ROOTS: dict[object, tuple[Path, RootIdentity]] = {}
_ISSUED_STOPS: dict[int, "StoppedChild"] = {}


class StoppedChild:
    """Supervisor-issued proof that the exact child was joined and stopped."""

    __slots__ = ("process", "pid", "exit_code", "_authority")

    def __init__(self, process, *, _authority) -> None:
        if _authority is not _STOP_AUTHORITY:
            raise ValueError("stop receipts can only be issued by the supervisor")
        if process.pid is None or process.exitcode is None or process.is_alive():
            raise ValueError("child process has not been confirmed stopped")
        self.process = process
        self.pid = process.pid
        self.exit_code = process.exitcode
        self._authority = _authority


@dataclass(frozen=True)
class Recovery:
    state: Literal["original", "staged", "uncertain"]
    detail: str


@dataclass(frozen=True)
class CaseResult:
    boundary: Literal["before_rename", "after_rename"]
    recovery: Recovery
    journal_phase: str
    original_fingerprint: dict[str, object] | None
    staged_fingerprint: dict[str, object] | None
    child_pid: int
    child_exit_code: int
    cleaned: bool


@dataclass(frozen=True)
class RootIdentity:
    root_device: int
    root_inode: int
    parent_device: int
    parent_inode: int
    original_device: int
    original_inode: int
    staged_device: int
    staged_inode: int


class OwnedRoot:
    """A fresh, inode-registered root whose path is never journal-controlled."""

    __slots__ = ("path", "_token", "_child")

    def __init__(self, path: Path, *, _authority) -> None:
        if _authority is not _STORE_AUTHORITY:
            raise ValueError("crash stores must be created by the private factory")
        self.path = path
        self._token = object()
        self._child = None

    @classmethod
    def create(cls) -> "OwnedRoot":
        path = Path(tempfile.mkdtemp(prefix=STORE_PREFIX))
        (path / "original").mkdir()
        (path / "staged").mkdir()
        store = cls(path, _authority=_STORE_AUTHORITY)
        _ACTIVE_ROOTS[store._token] = (path, _layout_identity(path))
        return store

    def _verify(self) -> None:
        registered = _ACTIVE_ROOTS.get(self._token)
        if registered is None:
            raise ValueError("crash store is not registered")
        expected_path, expected_identity = registered
        try:
            current_identity = _layout_identity(self.path)
        except (OSError, ValueError) as error:
            raise ValueError(f"crash store identity changed: {error}") from error
        if self.path != expected_path or current_identity != expected_identity:
            raise ValueError("crash store identity changed")

    @property
    def _identity(self) -> RootIdentity:
        registered = _ACTIVE_ROOTS.get(self._token)
        if registered is None:
            raise ValueError("crash store is not registered")
        return registered[1]

    def bind_child(self, process: multiprocessing.Process) -> None:
        self._verify()
        if self._child is not None or process.pid is None:
            raise ValueError("crash store can bind to one started child")
        self._child = process

    def cleanup(self, receipt: StoppedChild) -> None:
        self._verify()
        if not isinstance(receipt, StoppedChild) or receipt._authority is not _STOP_AUTHORITY:
            raise ValueError("cleanup requires a supervisor stop receipt")
        if (
            receipt.process is not self._child
            or receipt.pid != self._child.pid
            or receipt.process.is_alive()
            or receipt.process.exitcode != receipt.exit_code
            or _ISSUED_STOPS.pop(id(receipt), None) is not receipt
        ):
            raise ValueError("stop receipt does not prove this child exited")
        shutil.rmtree(self.path)
        del _ACTIVE_ROOTS[self._token]

    def cleanup_without_child(self) -> None:
        self._verify()
        if self._child is not None:
            raise ValueError("cannot remove a crash store while its child may be active")
        shutil.rmtree(self.path)
        del _ACTIVE_ROOTS[self._token]


def _directory_identity(path: Path) -> tuple[int, int]:
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode):
        raise ValueError(f"expected a real directory at fixed worker path: {path.name}")
    return info.st_dev, info.st_ino


def _layout_identity(root: Path) -> RootIdentity:
    root_device, root_inode = _directory_identity(root)
    parent_device, parent_inode = _directory_identity(root.parent)
    original_device, original_inode = _directory_identity(root / "original")
    staged_device, staged_inode = _directory_identity(root / "staged")
    devices = {root_device, parent_device, original_device, staged_device}
    if len(devices) != 1:
        raise ValueError("crash fixture layout spans unexpected filesystems")
    return RootIdentity(
        root_device,
        root_inode,
        parent_device,
        parent_inode,
        original_device,
        original_inode,
        staged_device,
        staged_inode,
    )


def _verify_layout(root: Path, expected: RootIdentity) -> None:
    if _layout_identity(root) != expected:
        raise ValueError("registered crash fixture directory identity changed")


def _fsync_directory(directory: Path) -> None:
    if not hasattr(os, "O_DIRECTORY"):
        raise OSError("directory fsync is unavailable on this platform")
    descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _durable_create(path: Path, data: bytes) -> None:
    with path.open("xb") as file:
        file.write(data)
        file.flush()
        os.fsync(file.fileno())
    _fsync_directory(path.parent)


def _canonical_json(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def _fingerprint(path: Path) -> dict[str, object] | None:
    try:
        before = path.lstat()
    except FileNotFoundError:
        return None
    if path.is_symlink() or not path.is_file():
        return {"kind": "unsupported"}
    if before.st_size > FIXTURE_SIZE:
        return {"kind": "oversize"}

    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            return {"kind": "changed-during-read"}
        digest = hashlib.sha256()
        size = 0
        while chunk := os.read(descriptor, 16 * 1024):
            digest.update(chunk)
            size += len(chunk)
            if size > FIXTURE_SIZE:
                return {"kind": "oversize"}
        after = path.lstat()
        if (after.st_dev, after.st_ino, after.st_size) != (
            opened.st_dev,
            opened.st_ino,
            size,
        ):
            return {"kind": "changed-during-read"}
        return {
            "kind": "file",
            "device": opened.st_dev,
            "inode": opened.st_ino,
            "size": size,
            "sha256": digest.hexdigest(),
        }
    finally:
        os.close(descriptor)


def _make_receipt(expected: dict[str, object]) -> dict[str, object]:
    content: dict[str, object] = {
        "schema": 1,
        "fixture_id": FIXTURE_ID,
        "source_slot": FIXTURE_REL.as_posix(),
        "destination_slot": STAGED_REL.as_posix(),
        "expected": expected,
    }
    return {**content, "receipt_sha256": hashlib.sha256(_canonical_json(content)).hexdigest()}


def _valid_receipt(receipt: object, journal: object) -> bool:
    if not isinstance(receipt, dict) or not isinstance(journal, dict):
        return False
    if set(journal) != {
        "schema",
        "phase",
        "fixture_id",
        "source_slot",
        "destination_slot",
        "expected",
    } or set(receipt) != {
        "schema",
        "fixture_id",
        "source_slot",
        "destination_slot",
        "expected",
        "receipt_sha256",
    }:
        return False
    expected = journal.get("expected")
    if not isinstance(expected, dict) or set(expected) != {
        "kind",
        "device",
        "inode",
        "size",
        "sha256",
    }:
        return False
    fields = {
        "schema": receipt.get("schema"),
        "fixture_id": receipt.get("fixture_id"),
        "source_slot": receipt.get("source_slot"),
        "destination_slot": receipt.get("destination_slot"),
        "expected": receipt.get("expected"),
    }
    expected_digest = hashlib.sha256(_canonical_json(fields)).hexdigest()
    return (
        receipt.get("receipt_sha256") == expected_digest
        and receipt.get("schema") == 1
        and journal.get("schema") == 1
        and receipt.get("fixture_id") == FIXTURE_ID
        and receipt.get("source_slot") == FIXTURE_REL.as_posix()
        and receipt.get("destination_slot") == STAGED_REL.as_posix()
        and receipt.get("expected") == journal.get("expected")
        and journal.get("fixture_id") == FIXTURE_ID
        and journal.get("source_slot") == FIXTURE_REL.as_posix()
        and journal.get("destination_slot") == STAGED_REL.as_posix()
        and journal.get("phase") == "intent"
    )


def _read_fixed_json(root: Path, relative: Path) -> object:
    path = root / relative
    if path.parent != root or path.is_symlink() or not path.is_file():
        raise ValueError("fixed control file is missing or not a regular file")
    before = path.lstat()
    if before.st_size > CONTROL_MAX_BYTES:
        raise ValueError("control file exceeds the fixed size limit")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise ValueError("control file changed during open")
        chunks = bytearray()
        while chunk := os.read(descriptor, 8192):
            chunks.extend(chunk)
        after = path.lstat()
        if (after.st_dev, after.st_ino, after.st_size) != (
            opened.st_dev,
            opened.st_ino,
            len(chunks),
        ):
            raise ValueError("control file changed during read")
        if len(chunks) > CONTROL_MAX_BYTES:
            raise ValueError("control file exceeds the fixed size limit")
        return json.loads(chunks)
    finally:
        os.close(descriptor)


def reconcile(
    store: OwnedRoot,
) -> tuple[Recovery, str, dict[str, object] | None, dict[str, object] | None]:
    """Read fixed journal/receipt slots and fingerprint fixed fixture paths only."""
    try:
        store._verify()
    except (OSError, ValueError) as error:
        return Recovery("uncertain", f"owned directory identity changed: {error}"), "invalid", None, None
    root = store.path
    try:
        journal = _read_fixed_json(root, JOURNAL_REL)
        receipt = _read_fixed_json(root, RECEIPT_REL)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        return Recovery("uncertain", f"control record invalid: {error}"), "invalid", None, None

    phase = journal.get("phase", "invalid") if isinstance(journal, dict) else "invalid"
    if not _valid_receipt(receipt, journal):
        return Recovery("uncertain", "receipt and intent do not match"), str(phase), None, None

    expected = journal.get("expected")
    if not isinstance(expected, dict):
        return Recovery("uncertain", "intent fingerprint is invalid"), str(phase), None, None
    try:
        _verify_layout(root, store._identity)
        original = _fingerprint(root / FIXTURE_REL)
        staged = _fingerprint(root / STAGED_REL)
    except (OSError, ValueError) as error:
        return Recovery("uncertain", f"fixture parent identity changed: {error}"), str(phase), None, None
    if original == expected and staged is None:
        return Recovery("original", "intent was durable; rename did not occur"), str(phase), original, staged
    if original is None and staged == expected:
        return Recovery("staged", "rename is present; completion record is absent"), str(phase), original, staged
    return Recovery("uncertain", "fixture locations or contents differ from the intent"), str(phase), original, staged


def restore(store: OwnedRoot) -> None:
    """Explicit fixture-only restore; never called by reconciliation itself."""
    recovery, _, _, _ = reconcile(store)
    if recovery.state != "staged":
        raise ValueError(f"restore blocked: {recovery.detail}")
    store._verify()
    root = store.path
    _verify_layout(root, store._identity)
    source = root / STAGED_REL
    destination = root / FIXTURE_REL
    if destination.exists() or destination.is_symlink():
        raise ValueError("restore blocked: original-slot collision")
    if source.is_symlink() or not source.is_file():
        raise ValueError("restore blocked: staged fixture is not a regular file")
    journal = _read_fixed_json(root, JOURNAL_REL)
    expected = journal.get("expected") if isinstance(journal, dict) else None
    if _fingerprint(source) != expected or _fingerprint(destination) is not None:
        raise ValueError("restore blocked: fixture changed or original-slot collision")
    _verify_layout(root, store._identity)
    os.rename(source, destination)
    _fsync_directory(source.parent)
    _fsync_directory(destination.parent)


def _fixture_bytes() -> bytes:
    return (b"Disk Steward disposable crash-recovery fixture\n" * 4096)[:FIXTURE_SIZE]


def _prepare_store(store: OwnedRoot) -> dict[str, object]:
    store._verify()
    original = store.path / FIXTURE_REL
    data = _fixture_bytes()
    _durable_create(original, data)
    expected = _fingerprint(original)
    if expected is None or expected.get("kind") != "file":
        raise RuntimeError("could not fingerprint the synthetic fixture")
    store._verify()
    return expected


def _stage_child(
    root_arg: str,
    expected_layout: RootIdentity,
    intent_checkpoint,
    allow_rename,
    rename_checkpoint,
    allow_outcome,
) -> None:
    """Fixed child; root argument comes only from the internal root factory."""
    root = Path(root_arg)
    _verify_layout(root, expected_layout)
    source = root / FIXTURE_REL
    destination = root / STAGED_REL
    expected = _fingerprint(source)
    if expected is None or expected.get("kind") != "file":
        raise RuntimeError("synthetic source fixture is missing or changed")
    _verify_layout(root, expected_layout)
    if destination.exists() or destination.is_symlink():
        raise FileExistsError("staged fixture slot already exists")

    intent = {
        "schema": 1,
        "phase": "intent",
        "fixture_id": FIXTURE_ID,
        "source_slot": FIXTURE_REL.as_posix(),
        "destination_slot": STAGED_REL.as_posix(),
        "expected": expected,
    }
    receipt = _make_receipt(expected)
    _durable_create(root / JOURNAL_REL, _canonical_json(intent))
    _durable_create(root / RECEIPT_REL, _canonical_json(receipt))
    intent_checkpoint.set()
    if not allow_rename.wait(START_TIMEOUT_SECONDS):
        raise TimeoutError("parent did not release the rename checkpoint")

    # This local fixture check prevents known collisions. It is not a race-free
    # no-replace primitive and does not establish production path safety.
    if destination.exists() or destination.is_symlink():
        raise FileExistsError("staged fixture slot collided before rename")
    _verify_layout(root, expected_layout)
    os.replace(source, destination)
    _fsync_directory(source.parent)
    _fsync_directory(destination.parent)
    _verify_layout(root, expected_layout)
    rename_checkpoint.set()
    if not allow_outcome.wait(START_TIMEOUT_SECONDS):
        raise TimeoutError("parent did not release the outcome checkpoint")
    _verify_layout(root, expected_layout)
    outcome = {**intent, "phase": "staged"}
    temporary = root / "journal.outcome.tmp"
    _durable_create(temporary, _canonical_json(outcome))
    os.replace(temporary, root / JOURNAL_REL)
    _fsync_directory(root)


def _stop_child(process: multiprocessing.Process) -> StoppedChild:
    if process.pid is None:
        raise RuntimeError("child never started")
    if process.is_alive():
        process.terminate()
    process.join(JOIN_TIMEOUT_SECONDS)
    if process.is_alive() and hasattr(process, "kill"):
        process.kill()
        process.join(JOIN_TIMEOUT_SECONDS)
    if process.is_alive() or process.exitcode is None:
        raise RuntimeError("child stop was not confirmed; fixture root must be retained")
    receipt = StoppedChild(process, _authority=_STOP_AUTHORITY)
    _ISSUED_STOPS[id(receipt)] = receipt
    return receipt


def _make_interrupted_fixture(
    boundary: Literal["before_rename", "after_rename"],
) -> tuple[OwnedRoot, StoppedChild]:
    context = multiprocessing.get_context("spawn")
    store = OwnedRoot.create()
    process = None
    try:
        _prepare_store(store)
        intent_checkpoint = context.Event()
        allow_rename = context.Event()
        rename_checkpoint = context.Event()
        allow_outcome = context.Event()
        process = context.Process(
            target=_stage_child,
            args=(
                str(store.path),
                store._identity,
                intent_checkpoint,
                allow_rename,
                rename_checkpoint,
                allow_outcome,
            ),
            name=f"disk-steward-crash-{boundary}",
            daemon=True,
        )
        process.start()
        store.bind_child(process)
        if not intent_checkpoint.wait(START_TIMEOUT_SECONDS):
            raise RuntimeError(f"child failed before durable intent checkpoint (exit={process.exitcode})")
        if boundary == "after_rename":
            allow_rename.set()
            if not rename_checkpoint.wait(START_TIMEOUT_SECONDS):
                raise RuntimeError(f"child failed before rename checkpoint (exit={process.exitcode})")

        stopped = _stop_child(process)
        return store, stopped
    except BaseException as error:
        try:
            if process is None or process.pid is None:
                store.cleanup_without_child()
                raise
            stopped = _stop_child(process)
            store.cleanup(stopped)
        except BaseException as stop_error:
            raise RuntimeError(
                f"{error}; child stop/cleanup unconfirmed ({stop_error}); "
                f"retained fixture root: {store.path}"
            ) from stop_error
        raise


def _interrupt_case(boundary: Literal["before_rename", "after_rename"]) -> CaseResult:
    store, stopped = _make_interrupted_fixture(boundary)
    try:
        recovery, phase, original, staged = reconcile(store)
        expected = "original" if boundary == "before_rename" else "staged"
        if recovery.state != expected:
            raise RuntimeError(f"unexpected recovery state at {boundary}: {recovery.detail}")
        result = CaseResult(
            boundary,
            recovery,
            phase,
            original,
            staged,
            stopped.pid,
            stopped.exit_code,
            cleaned=True,
        )
        store.cleanup(stopped)
        return result
    except BaseException:
        if stopped._authority is _STOP_AUTHORITY:
            try:
                store.cleanup(stopped)
            except BaseException:
                pass
        raise


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--run",
        action="store_true",
        help="run the two fixed synthetic child-interruption fixtures",
    )
    args = parser.parse_args(argv)
    if not args.run:
        print("Dry run: no temporary fixtures or child processes created.")
        print("--run demonstrates interruption before rename and after rename before outcome journal.")
        return 0
    if not hasattr(os, "O_DIRECTORY"):
        print("Probe unsupported: this platform cannot fsync directories with Python stdlib.")
        return 2

    try:
        results = [_interrupt_case("before_rename"), _interrupt_case("after_rename")]
    except Exception as error:
        print(f"Crash probe failed: {error}")
        return 1
    for result in results:
        print(
            f"boundary={result.boundary}; recovered={result.recovery.state}; "
            f"detail={result.recovery.detail}; journal_phase={result.journal_phase}; "
            f"child_exit={result.child_exit_code}; cleaned={result.cleaned}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
