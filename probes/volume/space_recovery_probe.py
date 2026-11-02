"""Opt-in development probe for space recovery on a disposable APFS image.

Dry-run is the default. Live setup and exact-fixture purge require separate
explicit opt-ins. This is feasibility evidence only, not production cleanup.
"""
from __future__ import annotations

import argparse
import json
import os
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Sequence
from pathlib import Path

FIXTURE_BYTES = 256 * 1024 * 1024
IMAGE_BUDGET_BYTES = 768 * 1024 * 1024
HOST_FREE_RESERVE_BYTES = 2 * 1024 * 1024 * 1024
VOLUME_FREE_MARGIN_BYTES = 64 * 1024 * 1024
FIXTURE_OVERHEAD_BYTES = 256 * 1024 * 1024
REQUIRED_HOST_FREE_BYTES = HOST_FREE_RESERVE_BYTES + IMAGE_BUDGET_BYTES + FIXTURE_OVERHEAD_BYTES
COMMAND_TIMEOUT_SECONDS = 120
RECOVERY_OBSERVATION_SECONDS = 30
RECOVERY_POLL_SECONDS = 1
RECOVERY_ABSOLUTE_TOLERANCE_BYTES = 16 * 1024 * 1024
DEVICE_PREFIX = "/dev/disk"


class ProbeError(RuntimeError):
    pass


def _within(root: Path, candidate: Path) -> Path:
    root_abs = root.resolve()
    candidate_abs = candidate.resolve()
    if candidate_abs == root_abs or root_abs not in candidate_abs.parents:
        raise ProbeError("probe receipt/path must remain inside its private temporary root")
    return candidate_abs


def create_args(image: Path) -> list[str]:
    return ["/usr/bin/hdiutil", "create", "-size", "768m",
            "-fs", "APFS", "-volname", "DiskStewardProbe", "-type", "UDIF",
            "-nospotlight", str(image)]


def attach_args(image: Path, mountpoint: Path) -> list[str]:
    return ["/usr/bin/hdiutil", "attach", "-plist", "-nobrowse", "-mountpoint",
            str(mountpoint), str(image)]


def detach_args(device: str) -> list[str]:
    if not re.fullmatch(r"/dev/disk\d+(?:s\d+)?", device):
        raise ProbeError("refusing to detach a device not returned by this probe")
    return ["/usr/bin/hdiutil", "detach", device]


def returned_device(plist_bytes: bytes, expected_mountpoint: Path) -> str:
    try:
        entities = plistlib.loads(plist_bytes)["system-entities"]
    except (ValueError, KeyError, TypeError) as exc:
        raise ProbeError("hdiutil attach returned an unreadable device receipt") from exc
    expected = str(expected_mountpoint.resolve())
    for entity in entities:
        if entity.get("mount-point") == expected:
            device = entity.get("dev-entry", "")
            detach_args(device)  # validate the exact returned device before retaining it
            return device
    raise ProbeError("hdiutil attach did not report this probe's mountpoint")


def _run_unmounted(args: Sequence[str], timeout: int = COMMAND_TIMEOUT_SECONDS) -> subprocess.CompletedProcess[bytes]:
    """Run create only; its timeout cannot interrupt a mounted volume operation."""
    try:
        return subprocess.run(args, check=True, capture_output=True, timeout=timeout)
    except subprocess.CalledProcessError as exc:
        detail = (exc.stderr or b"").decode(errors="replace")[-2048:]
        raise ProbeError(f"private image creation failed: {detail}") from None


def _run_mount_operation(args: Sequence[str], timeout: int = COMMAND_TIMEOUT_SECONDS) -> tuple[int | None, bytes, bytes]:
    """Bound mount/detach waits without killing hdiutil if the wait expires."""
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        process = subprocess.Popen(args, stdout=out, stderr=err)
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            out.seek(0); err.seek(0)
            return None, out.read(), err.read()
        out.seek(0); err.seek(0)
        return code, out.read(), err.read()


def _write_fixture(path: Path) -> int:
    # os.urandom provides incompressible fixture data; write and sync in bounded chunks.
    with path.open("xb") as handle:
        remaining = FIXTURE_BYTES
        while remaining:
            chunk = os.urandom(min(1024 * 1024, remaining))
            handle.write(chunk)
            remaining -= len(chunk)
        handle.flush()
        os.fsync(handle.fileno())
    st = path.stat()
    if st.st_size != FIXTURE_BYTES or st.st_blocks * 512 < FIXTURE_BYTES * 0.99:
        raise ProbeError("synthetic fixture was not fully allocated")
    return st.st_blocks * 512


def _free_bytes(path: Path) -> int:
    return shutil.disk_usage(path).free


def _remove_private_artifacts(image: Path, mountpoint: Path, holding: Path,
                              receipt: Path, root: Path) -> None:
    """Remove only named probe artifacts after a successful detach; no tree walk."""
    for path in (image, receipt):
        try:
            path.unlink()
        except FileNotFoundError:
            pass
    for directory in (holding, mountpoint, root):
        try:
            directory.rmdir()
        except FileNotFoundError:
            # The holding directory disappears with the detached filesystem.
            continue
        except OSError:
            # Unexpected contents are evidence. Leave them for inspection.
            break


def _observe_recovery(path: Path, baseline_free: int, allocated_bytes: int,
                      timeout_seconds: int = RECOVERY_OBSERVATION_SECONDS) -> tuple[int, int, bool]:
    tolerance = max(RECOVERY_ABSOLUTE_TOLERANCE_BYTES, int(allocated_bytes * 0.10))
    deadline = time.monotonic() + timeout_seconds
    while True:
        observed_free = _free_bytes(path)
        recovered = observed_free - baseline_free
        if abs(recovered - allocated_bytes) <= tolerance:
            return observed_free, recovered, True
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return observed_free, recovered, False
        time.sleep(min(RECOVERY_POLL_SECONDS, remaining))


PURGE_CONFIRMATION = "PURGE ONLY THIS PROBE'S FIXTURE"


def run_probe(base: Path | None = None,
              run_command: Callable[[Sequence[str]], subprocess.CompletedProcess[bytes]] = _run_unmounted,
              mount_command: Callable[[Sequence[str]], tuple[int | None, bytes, bytes]] = _run_mount_operation,
              recovery_timeout_seconds: int = RECOVERY_OBSERVATION_SECONDS,
              purge_authorized: bool = False) -> dict[str, object]:
    if not purge_authorized:
        raise ProbeError("live run requires separate purge authorization before image creation")
    if sys.platform != "darwin":
        raise ProbeError("live APFS volume probe is available only on macOS")
    parent = (base or Path(tempfile.gettempdir())).resolve()
    if _free_bytes(parent) < REQUIRED_HOST_FREE_BYTES:
        raise ProbeError("insufficient host free space for the image budget plus 2 GiB reserve")
    root = Path(tempfile.mkdtemp(prefix="disk-steward-volume-probe-", dir=parent))
    image = _within(root, root / "probe.dmg")
    mountpoint = _within(root, root / "mounted")
    holding = _within(mountpoint, mountpoint / "probe-holding")
    mountpoint.mkdir()
    receipt = _within(root, root / "receipt.json")
    device: str | None = None
    attach_attempted = False
    report: dict[str, object] = {"status": "started", "image": str(image), "mountpoint": str(mountpoint)}
    try:
        run_command(create_args(image))
        attach_attempted = True
        code, stdout, stderr = mount_command(attach_args(image, mountpoint))
        if code is None:
            report.update(status="attach_wait_timed_out", retained_root=str(root), detail=stderr.decode(errors="replace"))
            receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
            return report  # Keep image/root; do not detach an unknown device or delete a possibly mounted path.
        if code != 0:
            report.update(status="attach_failed_unknown_mount_state", retained_root=str(root),
                          detail=stderr.decode(errors="replace"))
            receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
            return report
        device = returned_device(stdout, mountpoint)
        report["device"] = device
        receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")

        host_free = _free_bytes(parent)
        volume_free_before = _free_bytes(mountpoint)
        minimum_before_fixture = FIXTURE_BYTES + VOLUME_FREE_MARGIN_BYTES
        if volume_free_before < minimum_before_fixture:
            raise ProbeError("mounted experiment volume lacks fixture capacity plus its 64 MiB margin")
        source = _within(mountpoint, mountpoint / "fixture.bin")
        holding.mkdir()
        allocated_bytes = _write_fixture(source)
        fixture_identity = source.stat(follow_symlinks=False)
        after_fixture = _free_bytes(mountpoint)
        if after_fixture < VOLUME_FREE_MARGIN_BYTES:
            raise ProbeError("fixture growth would violate the experiment volume's 64 MiB margin")
        held = _within(holding, holding / "fixture.bin")
        os.replace(source, held)  # simulated stage
        after_stage = _free_bytes(mountpoint)
        os.replace(held, source)  # simulated restore
        after_restore = _free_bytes(mountpoint)
        os.replace(source, held)  # simulated stage again
        held_identity = held.stat(follow_symlinks=False)
        if (not stat.S_ISREG(held_identity.st_mode) or held_identity.st_nlink != 1 or
                (held_identity.st_dev, held_identity.st_ino, held_identity.st_size) !=
                (fixture_identity.st_dev, fixture_identity.st_ino, FIXTURE_BYTES)):
            raise ProbeError("staged fixture identity changed; refusing purge")
        after_restage = _free_bytes(mountpoint)
        held.unlink()  # purge this exact probe-created file only
        after_purge, recovered_bytes, recovered = _observe_recovery(
            mountpoint, after_restage, allocated_bytes, recovery_timeout_seconds)
        report.update(status="complete" if recovered else "recovery_not_observed",
                      host_free_bytes_before=host_free,
                      volume_free_bytes={"before": volume_free_before, "after_fixture": after_fixture,
                                         "after_stage": after_stage, "after_restore": after_restore,
                                         "after_restage": after_restage, "after_purge": after_purge},
                      signed_recovered_bytes=recovered_bytes, fixture_bytes=FIXTURE_BYTES,
                      fixture_allocated_bytes=allocated_bytes,
                      recovery_tolerance_bytes=max(RECOVERY_ABSOLUTE_TOLERANCE_BYTES,
                                                   int(allocated_bytes * 0.10)),
                      recovery_observation_seconds=recovery_timeout_seconds,
                      recovery_meets_tolerance=recovered,
                      signed_free_space_delta_bytes={
                          "after_fixture": after_fixture - volume_free_before,
                          "after_stage": after_stage - after_fixture,
                          "after_restore": after_restore - after_stage,
                          "after_restage": after_restage - after_restore,
                          "after_purge": after_purge - after_restage,
                          "total_from_initial": after_purge - volume_free_before,
                      })
        receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
        return report
    except (ProbeError, OSError, subprocess.SubprocessError) as exc:
        if attach_attempted and device is None:
            report.update(status="unknown_mount_state", retained_root=str(root), detail=str(exc))
            receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
            return report
        raise
    finally:
        if device is not None:
            code, _, stderr = mount_command(detach_args(device))
            if code == 0:
                _remove_private_artifacts(image, mountpoint, holding, receipt, root)
            else:
                report.update(status="detach_failed", retained_root=str(root),
                              detach_detail=stderr.decode(errors="replace"))
                # The root and image are deliberately retained; never remove a mounted volume.
                receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
        elif not attach_attempted:
            _remove_private_artifacts(image, mountpoint, root / "probe-holding", receipt, root)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="create, mount, and measure a private APFS image")
    parser.add_argument("--purge-own-fixture", action="store_true",
                        help="separately authorize unlink of this run's exact staged fixture")
    parser.add_argument("--confirm-purge", help="must exactly equal the displayed purge confirmation phrase")
    args = parser.parse_args(argv)
    if not args.run:
        if args.purge_own_fixture or args.confirm_purge:
            parser.error("purge confirmation flags require --run")
        print(json.dumps({"status": "dry_run", "would_create": IMAGE_BUDGET_BYTES,
                          "fixture_bytes": FIXTURE_BYTES, "minimum_host_free_bytes": REQUIRED_HOST_FREE_BYTES,
                          "purge_confirmation_phrase": PURGE_CONFIRMATION,
                          "actions": ["create APFS image", "mount private image", "stage/restore/restage own fixture",
                                      "purge exact fixture only with --purge-own-fixture and --confirm-purge",
                                      "detach only returned device"]}, indent=2))
        return 0
    if not args.purge_own_fixture or args.confirm_purge != PURGE_CONFIRMATION:
        parser.error("live image creation requires --purge-own-fixture and the exact --confirm-purge phrase")
    try:
        report = run_probe(purge_authorized=args.purge_own_fixture)
        print(json.dumps(report, indent=2))
        return 0 if report.get("status") == "complete" and report.get("recovery_meets_tolerance") is True else 1
    except (ProbeError, OSError, subprocess.SubprocessError) as exc:
        print(json.dumps({"status": "unsupported_or_failed", "reason": str(exc)}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
