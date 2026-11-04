"""Generate D3 reference bytes using pinned pytest in a fresh task-owned directory.

The interpreter must already contain pytest 9.2.0. No install, user project,
plugin discovery, cleanup, or source-file execution outside this fixed fixture.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path

TEST_SOURCE = """def test_pass():
    assert 2 + 2 == 4


def test_fail():
    assert False, 'intentional fixture failure'
"""
RECORDS = ("v/cache/nodeids", "v/cache/lastfailed")
RESERVE = 2 * 1024**3 + 64 * 1024**2


def generate(python: Path, destination: Path) -> dict:
    python = python.absolute()
    destination = destination.absolute()
    if not python.is_file():
        raise ValueError("An existing interpreter is required")
    if not destination.parent.is_dir() or destination.exists():
        raise ValueError("Destination must be new under an existing task-owned parent")
    if shutil.disk_usage(destination.parent).free < RESERVE:
        raise ValueError("Free-space reserve would not be preserved")
    # Exclusive creation; partial failures are preserved for inspection.
    destination.mkdir(mode=0o700)
    project = destination / "project"
    project.mkdir(mode=0o700)
    (project / "test_generated.py").write_text(TEST_SOURCE, encoding="utf-8")
    (project / "pytest.ini").write_text("[pytest]\n", encoding="utf-8")
    env = {
        "PATH": str(python.parent) + os.pathsep + "/usr/bin:/bin",
        "PYTEST_DISABLE_PLUGIN_AUTOLOAD": "1",
        "PYTHONNOUSERSITE": "1",
        "HOME": str(destination),
    }
    version_output = subprocess.run(
        [
            str(python),
            "-I",
            "-c",
            "import json, pytest, platform; print(json.dumps([pytest.__version__, platform.python_version()]))",
        ],
        cwd=project,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
        check=True,
    ).stdout.strip()
    version, python_version = json.loads(version_output)
    if version != "9.2.0":
        raise ValueError("Only the pinned pytest 9.2.0 reference is accepted")
    result = subprocess.run(
        [
            str(python),
            "-I",
            "-m",
            "pytest",
            "-q",
            "--noconftest",
            "-c",
            "pytest.ini",
            "test_generated.py",
        ],
        cwd=project,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )
    (destination / "pytest-output.txt").write_text(
        result.stdout + result.stderr, encoding="utf-8"
    )
    if result.returncode != 1:
        raise ValueError(
            "Expected exactly the fixed fixture's intentional test failure"
        )
    cache = project / ".pytest_cache"
    output = destination / "records"
    output.mkdir(mode=0o700)
    hashes = {}
    for record in RECORDS:
        with (cache / record).open("rb") as stream:
            data = stream.read(65537)
        if len(data) > 65536:
            raise ValueError("Generated fixture exceeds reference budget")
        name = Path(record).name + ".json"
        (output / name).write_bytes(data)
        hashes[name] = hashlib.sha256(data).hexdigest()
    nodeids = json.loads((output / "nodeids.json").read_bytes())
    lastfailed = json.loads((output / "lastfailed.json").read_bytes())
    if nodeids != ["test_generated.py::test_fail", "test_generated.py::test_pass"]:
        raise ValueError("Unexpected manager-generated node IDs")
    if lastfailed != {"test_generated.py::test_fail": True}:
        raise ValueError("Unexpected manager-generated failure record")
    evidence = {
        "pytestVersion": version,
        "pythonVersion": python_version,
        "expectedExitCode": 1,
        "records": hashes,
        "cleanupEligibility": False,
        "lifecycleValidated": False,
    }
    (destination / "evidence.json").write_text(
        json.dumps(evidence, indent=2) + "\n", encoding="utf-8"
    )
    return evidence


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(generate(args.python, args.destination), indent=2))
