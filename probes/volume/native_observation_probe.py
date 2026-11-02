"""Development-only native volume observation probe on an owned APFS image."""
from __future__ import annotations

import argparse
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
from collections.abc import Callable, Sequence
from pathlib import Path

from probes.volume import space_recovery_probe as space

HELPER = Path(__file__).resolve().parents[1] / "fixture-migration/target/debug/desktop_migration_harness"
HOME_MARKER = ".disk-steward-disposable-probe"
HOME_MARKER_VALUE = "disk-steward-migration-v1\n"
CALIBRATION_CONFIRMATION = "--confirm-own-calibration-unlink"
PURGE_CONFIRMATION = "--confirm-own-demo-purge"
METHOD = "fstatvfs_bavail_v1"


class NativeProbeError(space.ProbeError):
    pass


def _run_helper(home: Path, command: str) -> dict[str, object]:
    args = [str(HELPER), *command.split()]
    env = os.environ.copy()
    env["HOME"] = str(home)
    try:
        completed = subprocess.run(args, check=True, capture_output=True,
                                   timeout=space.COMMAND_TIMEOUT_SECONDS, env=env)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        stderr = getattr(exc, "stderr", b"") or b""
        detail = stderr.decode(errors="replace")[-2048:]
        raise NativeProbeError(f"native helper {command.split()[0]} failed: {detail}") from None
    if command == "seed":
        # The harness seed operation reports human-readable status, not JSON.
        return {"stdout": completed.stdout.decode(errors="replace")[-2048:]}
    try:
        value = json.loads(completed.stdout)
    except (ValueError, TypeError) as exc:
        raise NativeProbeError(f"native helper {command.split()[0]} returned malformed JSON") from exc
    if not isinstance(value, dict):
        raise NativeProbeError(f"native helper {command.split()[0]} returned a non-object JSON value")
    return value


def _validate_calibration(value: dict[str, object]) -> None:
    fixture_bytes = 256 * 1024 * 1024
    allocated = value.get("allocatedBytes")
    if (value.get("method") != METHOD or value.get("fixtureBytes") != fixture_bytes or
            isinstance(allocated, bool) or not isinstance(allocated, int) or
            not fixture_bytes <= allocated <= space.IMAGE_BUDGET_BYTES):
        raise NativeProbeError("native calibration report has an unexpected method or allocation")
    delta = value.get("observedFreeSpaceDeltaBytes")
    tolerance = value.get("toleranceBytes")
    elapsed = value.get("elapsedMs")
    if (isinstance(delta, bool) or not isinstance(delta, int) or
            isinstance(tolerance, bool) or not isinstance(tolerance, int) or
            tolerance != max(16 * 1024 * 1024, allocated // 10) or
            abs(delta - allocated) > tolerance or isinstance(elapsed, bool) or
            not isinstance(elapsed, int) or not 0 <= elapsed <= 35_000):
        raise NativeProbeError("native calibration recovery is outside its reported tolerance or time limit")


def _validate_purge(value: dict[str, object]) -> None:
    observation = value.get("volumeObservation")
    if value.get("state") != "finished":
        raise NativeProbeError("product purge result did not finish")
    selected = value.get("selectedLogicalBytes")
    unlinked = value.get("durablyUnlinkedLogicalBytes")
    if isinstance(selected, bool) or not isinstance(selected, int) or selected <= 0:
        raise NativeProbeError("product purge result selected no positive logical bytes")
    if (isinstance(unlinked, bool) or not isinstance(unlinked, int) or
            unlinked <= 0 or unlinked != selected):
        raise NativeProbeError("product purge result did not durably unlink the selected logical bytes")
    if not isinstance(observation, dict) or observation.get("method") != METHOD:
        raise NativeProbeError("product purge result lacks the expected native volume observation")
    delta = observation.get("observedFreeSpaceDeltaBytes")
    if isinstance(delta, bool) or not isinstance(delta, int) or abs(delta) > 9_007_199_254_740_991:
        raise NativeProbeError("product volume observation has an invalid signed delta")
    digest = observation.get("volumeBindingSha256")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise NativeProbeError("product volume observation has an invalid binding digest")


def run_probe(base: Path | None = None,
              run_command: Callable[[Sequence[str]], subprocess.CompletedProcess[bytes]] = space._run_unmounted,
              mount_command: Callable[[Sequence[str]], tuple[int | None, bytes, bytes]] = space._run_mount_operation,
              helper_command: Callable[[Path, str], dict[str, object]] = _run_helper,
              purge_authorized: bool = False,
              calibration_authorized: bool = False) -> dict[str, object]:
    if not (purge_authorized and calibration_authorized):
        raise NativeProbeError("live run requires separate purge and calibration authorization before image creation")
    if sys.platform != "darwin":
        raise NativeProbeError("live native volume probe is available only on macOS")
    if not HELPER.is_file():
        raise NativeProbeError("fixed native migration helper is missing; build it before running this probe")
    parent = (base or Path(tempfile.gettempdir())).resolve()
    if space._free_bytes(parent) < space.REQUIRED_HOST_FREE_BYTES:
        raise NativeProbeError("insufficient host free space for the image budget plus 2 GiB reserve")
    root = Path(tempfile.mkdtemp(prefix="disk-steward-native-volume-probe-", dir=parent))
    image = space._within(root, root / "probe.dmg")
    mountpoint = space._within(root, root / "mounted")
    home = space._within(mountpoint, mountpoint / "home")
    receipt = space._within(root, root / "receipt.json")
    mountpoint.mkdir()
    device: str | None = None
    attach_attempted = False
    report: dict[str, object] = {"status": "started", "platform": sys.platform,
                                 "macos_version": platform.mac_ver()[0],
                                 "machine": platform.machine(),
                                 "image": str(image), "mountpoint": str(mountpoint)}
    try:
        run_command(space.create_args(image))
        attach_attempted = True
        code, stdout, stderr = mount_command(space.attach_args(image, mountpoint))
        if code is None or code != 0:
            report.update(status="attach_failed_unknown_mount_state", retained_root=str(root),
                          detail=stderr.decode(errors="replace")[-2048:])
            receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
            return report
        device = space.returned_device(stdout, mountpoint)
        report["device"] = device
        home.mkdir(mode=0o700)
        (home / HOME_MARKER).write_text(HOME_MARKER_VALUE, encoding="utf-8")
        if (home / HOME_MARKER).read_text(encoding="utf-8") != HOME_MARKER_VALUE:
            raise NativeProbeError("owned HOME marker failed verification")
        helper_command(home, "seed")
        calibration = helper_command(home, f"calibrate-own-volume {CALIBRATION_CONFIRMATION}")
        _validate_calibration(calibration)
        product = helper_command(home, f"purge-own-staged-demo {PURGE_CONFIRMATION}")
        _validate_purge(product)
        report.update(status="complete", method=METHOD, calibration=calibration, product=product,
                      evidence_scope="controlled owned volume; observed delta is not attributed to this purge")
        receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
        return report
    except (NativeProbeError, space.ProbeError, OSError, subprocess.SubprocessError) as exc:
        if attach_attempted and device is None:
            report.update(status="unknown_mount_state", retained_root=str(root), detail=str(exc))
            receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
            return report
        report.update(status="failed", retained_root=str(root), detail=str(exc))
        receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
        return report
    finally:
        if device is not None:
            code, _, stderr = mount_command(space.detach_args(device))
            if code == 0:
                report["image_detached"] = True
                if report.get("status") == "complete":
                    space._remove_private_artifacts(image, mountpoint, mountpoint / "probe-holding", receipt, root)
                    report["temporary_root_removed"] = not root.exists()
                    if root.exists():
                        report.update(status="cleanup_incomplete", retained_root=str(root))
                        receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
                else:
                    # Keep failed-run evidence after the exact owned mount is detached.
                    receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
            else:
                report.update(status="detach_failed", retained_root=str(root),
                              detach_detail=stderr.decode(errors="replace")[-2048:])
                receipt.write_text(json.dumps(report, indent=2), encoding="utf-8")
        elif not attach_attempted:
            space._remove_private_artifacts(image, mountpoint, root / "probe-holding", receipt, root)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="run against a fresh private APFS image")
    parser.add_argument("--confirm-own-demo-purge", action="store_true")
    parser.add_argument("--confirm-own-calibration-unlink", action="store_true")
    args = parser.parse_args(argv)
    if not args.run:
        print(json.dumps({"status": "dry_run", "image_bytes": space.IMAGE_BUDGET_BYTES,
                          "required_host_free_bytes": space.REQUIRED_HOST_FREE_BYTES,
                          "helper": str(HELPER), "mutations": []}, indent=2))
        return 0
    try:
        result = run_probe(purge_authorized=args.confirm_own_demo_purge,
                           calibration_authorized=args.confirm_own_calibration_unlink)
    except (NativeProbeError, space.ProbeError) as exc:
        parser.error(str(exc))
    print(json.dumps(result, indent=2))
    return 0 if result.get("status") == "complete" else 1


if __name__ == "__main__":
    raise SystemExit(main())
