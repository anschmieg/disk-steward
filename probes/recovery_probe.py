"""Development-only recovery mechanics probe for disposable fixtures.

This is not a filesystem adapter. It accepts no caller-supplied paths, uses no
native Trash, and proves nothing about production identity or safety.
"""
from __future__ import annotations

import hashlib
import json
import os
import tempfile
from pathlib import Path
from typing import Any


class ProbeError(RuntimeError):
    """A fixture operation was safely rejected."""


def _fingerprint(path: Path) -> dict[str, Any]:
    data = path.read_bytes()
    st = path.stat(follow_symlinks=False)
    return {"sha256": hashlib.sha256(data).hexdigest(), "size": len(data),
            "mode": st.st_mode, "device": st.st_dev, "inode": st.st_ino}


class DisposableRecoveryProbe:
    """Creates only internally named fixtures under one TemporaryDirectory."""

    def __init__(self) -> None:
        self._temporary = tempfile.TemporaryDirectory(prefix="disk-steward-recovery-probe-")
        self.root = Path(self._temporary.name)
        self.source = self.root / "source"
        self.holding = self.root / "holding"
        self.source.mkdir()
        self.holding.mkdir()
        self.manifest_path = self.root / "manifest.json"
        self.records: dict[str, dict[str, Any]] = {}
        self._created_names: set[str] = set()

    def create_fixture(self, name: str, contents: bytes) -> Path:
        if Path(name).name != name or name in {"", ".", ".."}:
            raise ProbeError("fixture names must be simple names")
        path = self.source / name
        try:
            with path.open("xb") as handle:
                handle.write(contents)
        except FileExistsError as exc:
            raise ProbeError("fixture destination already exists") from exc
        self._created_names.add(name)
        return path

    def stage(self, name: str) -> str:
        if Path(name).name != name or name not in self._created_names:
            raise ProbeError("only internally created fixture names may be staged")
        original = self.source / name
        if not original.is_file() or original.is_symlink():
            raise ProbeError("fixture missing or not a regular file")
        receipt = f"fixture-{len(self.records) + 1}"
        held = self.holding / receipt
        identity = _fingerprint(original)
        os.replace(original, held)
        self.records[receipt] = {
            "name": name,
            "held_name": receipt,
            "identity": identity,
            "state": "staged",
        }
        self._save()
        return receipt

    def restore(self, receipt: str) -> Path:
        record = self._record(receipt)
        held = self.holding / record["held_name"]
        target = self.source / record["name"]
        if target.exists() or target.is_symlink():
            raise ProbeError("restore destination collision")
        self._verify(held, record["identity"])
        os.replace(held, target)
        record["state"] = "restored"
        self._save()
        return target

    def purge(self, receipt: str) -> None:
        record = self._record(receipt)
        held = self.holding / record["held_name"]
        self._verify(held, record["identity"])
        held.unlink()
        record["state"] = "purged"
        self._save()

    def reconcile(self) -> dict[str, str]:
        """Report interrupted/changed states; does not mutate fixture files."""
        result: dict[str, str] = {}
        for receipt, record in self.records.items():
            held = self.holding / record["held_name"]
            original = self.source / record["name"]
            if record["state"] == "staged" and held.exists():
                try:
                    self._verify(held, record["identity"])
                    result[receipt] = "staged"
                except ProbeError:
                    result[receipt] = "changed"
            elif record["state"] == "staged" and original.exists():
                result[receipt] = "restored-before-record"
            elif record["state"] == "staged":
                result[receipt] = "missing"
            else:
                result[receipt] = record["state"]
        return result

    def _record(self, receipt: str) -> dict[str, Any]:
        try:
            record = self.records[receipt]
        except KeyError as exc:
            raise ProbeError("unknown fixture receipt") from exc
        if record["state"] != "staged":
            raise ProbeError("receipt is not staged")
        return record

    @staticmethod
    def _verify(path: Path, expected: dict[str, Any]) -> None:
        if not path.is_file() or path.is_symlink() or _fingerprint(path) != expected:
            raise ProbeError("staged fixture identity changed")

    def _save(self) -> None:
        self.manifest_path.write_text(json.dumps(self.records, sort_keys=True), encoding="utf-8")

    def close(self) -> None:
        self._temporary.cleanup()

    def __enter__(self) -> "DisposableRecoveryProbe":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()
