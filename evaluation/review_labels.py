#!/usr/bin/env python3
"""Interactive, development-only manual review of provisional evaluation routes."""
import argparse
import hashlib
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

from harness import ROUTES, load_cases

SCHEMA = "disk-steward-manual-route-review-v2"
ROUTE_CONTRACT_VERSION = "disk-steward-routes-v1"
PROVENANCE = "manual reviewer attestation; identity is self-reported and not independently verified"
MAX_LINE_BYTES = 16_384
MAX_REVIEW_RECORDS = 60
MAX_RATIONALE_LENGTH = 2_000
MAX_REVIEWER_LENGTH = 200
ROUTE_DESCRIPTIONS = {
    "keep": "affirmatively retain/protect; no further investigation",
    "inspect_metadata": "gather or corroborate metadata next",
    "run_specialist": "analysis by a specialist for a known category",
    "reasoning_review": "additional agent reasoning; not human review",
    "ask_user": "personal intent or another user decision is needed",
}


def case_digest(case):
    """Bind a review to visible inputs and the route vocabulary, excluding labels."""
    bound = {"case_id": case["id"], "facts": case["facts"],
             "evidence_ids": case["evidence_ids"], "route_contract_version": ROUTE_CONTRACT_VERSION, "route_definitions": ROUTE_DESCRIPTIONS}
    canonical = json.dumps(bound, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def load_review(path, dev_by_id, heldout_ids):
    """Read and validate an explicit prior JSONL review without modifying it."""
    records, seen = [], {}
    with Path(path).open("rb") as stream:
        for line_no, line in enumerate(iter(lambda: stream.readline(MAX_LINE_BYTES + 1), b""), 1):
            if line_no > MAX_REVIEW_RECORDS:
                raise ValueError(f"{path}: review exceeds {MAX_REVIEW_RECORDS} records")
            if len(line) > MAX_LINE_BYTES or (len(line) == MAX_LINE_BYTES and not line.endswith(b"\n")):
                raise ValueError(f"{path}:{line_no}: record exceeds {MAX_LINE_BYTES} bytes")
            if not line.strip():
                raise ValueError(f"{path}:{line_no}: blank lines are not allowed")
            try:
                record = json.loads(line.decode("utf-8"))
            except (json.JSONDecodeError, UnicodeDecodeError, RecursionError) as exc:
                raise ValueError(f"{path}:{line_no}: invalid JSON or UTF-8") from exc
            expected = {"schema", "case_id", "route", "rationale", "reviewer", "reviewed_at", "provenance", "case_sha256"}
            if not isinstance(record, dict) or set(record) != expected:
                raise ValueError(f"{path}:{line_no}: unexpected review schema")
            if record["schema"] != SCHEMA or record["provenance"] != PROVENANCE:
                raise ValueError(f"{path}:{line_no}: unsupported schema or provenance")
            cid = record["case_id"]
            if not isinstance(cid, str):
                raise ValueError(f"{path}:{line_no}: case_id must be a string")
            if cid in heldout_ids:
                raise ValueError(f"{path}:{line_no}: held-out case IDs are not accepted")
            if cid not in dev_by_id:
                raise ValueError(f"{path}:{line_no}: unrecognized development case ID {cid!r}")
            if cid in seen:
                kind = "conflicting" if seen[cid] != record else "duplicate"
                raise ValueError(f"{path}:{line_no}: {kind} review for {cid}")
            if not isinstance(record["route"], str) or record["route"] not in ROUTES:
                raise ValueError(f"{path}:{line_no}: invalid route")
            for field, limit in (("rationale", MAX_RATIONALE_LENGTH), ("reviewer", MAX_REVIEWER_LENGTH)):
                if not isinstance(record[field], str) or not record[field].strip() or len(record[field]) > limit:
                    raise ValueError(f"{path}:{line_no}: {field} must be non-empty and at most {limit} characters")
            try:
                timestamp = datetime.fromisoformat(record["reviewed_at"])
            except (TypeError, ValueError) as exc:
                raise ValueError(f"{path}:{line_no}: reviewed_at must be an ISO timestamp with timezone") from exc
            if timestamp.utcoffset() is None:
                raise ValueError(f"{path}:{line_no}: reviewed_at must include a timezone")
            if not isinstance(record["case_sha256"], str) or record["case_sha256"] != case_digest(dev_by_id[cid]):
                raise ValueError(f"{path}:{line_no}: case facts/evidence or route contract changed")
            seen[cid] = record
            records.append(record)
    return records


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, help="new JSONL path; existing files are never overwritten")
    parser.add_argument("--resume", help="prior JSONL review to read and continue into a new output")
    return parser


def display_payload(case):
    """Return only the development facts/evidence permitted in the review UI."""
    return {"facts": case["facts"], "evidence_ids": case["evidence_ids"]}


def main(argv=None):
    args = build_parser().parse_args(argv)
    if not sys.stdin.isatty() or not sys.stdout.isatty():
        print("Manual review requires an interactive terminal (stdin and stdout).", file=sys.stderr)
        return 2
    cases = load_cases()
    dev = [case for case in cases if case["split"] == "development"]
    dev_by_id = {case["id"]: case for case in dev}
    heldout_ids = {case["id"] for case in cases if case["split"] == "heldout"}
    prior = []
    if args.resume:
        if Path(args.resume).resolve() == Path(args.output).resolve():
            print("Output must be a new path, separate from the read-only resume input.", file=sys.stderr)
            return 2
        try:
            prior = load_review(args.resume, dev_by_id, heldout_ids)
        except (OSError, ValueError) as exc:
            print(f"Cannot resume: {exc}", file=sys.stderr)
            return 2
    reviewed = {record["case_id"] for record in prior}
    try:
        out = Path(args.output).open("x", encoding="utf-8")
    except FileExistsError:
        print("Output already exists; choose a new path.", file=sys.stderr)
        return 2
    except OSError as exc:
        print(f"Cannot create output: {exc}", file=sys.stderr)
        return 2

    with out:
        for record in prior:
            out.write(json.dumps(record, ensure_ascii=False) + "\n")
        out.flush()
        todo = [case for case in dev if case["id"] not in reviewed]
        for index, case in enumerate(todo, 1):
            print(f"Development case {index}/{len(todo)} · {case['id']}")
            payload = display_payload(case)
            print("Facts:", json.dumps(payload["facts"], ensure_ascii=False, sort_keys=True))
            print("Evidence IDs:", json.dumps(payload["evidence_ids"], ensure_ascii=False))
            print("Routes (none authorizes cleanup):")
            for route in sorted(ROUTES):
                print(f"  {route}: {ROUTE_DESCRIPTIONS[route]}")
            while True:
                route = input("Route (or q to stop): ").strip()
                if route == "q":
                    print("Review stopped; completed records are saved.")
                    return 0
                if route in ROUTES:
                    break
                print("Enter one listed route.")
            rationale = input("Rationale: ").strip()
            while not rationale:
                print("Rationale cannot be empty.")
                rationale = input("Rationale: ").strip()
            if len(rationale) > MAX_RATIONALE_LENGTH:
                raise ValueError(f"Rationale exceeds {MAX_RATIONALE_LENGTH} characters.")
            reviewer = input("Reviewer identity (self-reported): ").strip()
            while not reviewer:
                print("Reviewer identity cannot be empty.")
                reviewer = input("Reviewer identity (self-reported): ").strip()
            if len(reviewer) > MAX_REVIEWER_LENGTH:
                raise ValueError(f"Reviewer identity exceeds {MAX_REVIEWER_LENGTH} characters.")
            record = {"schema": SCHEMA, "case_id": case["id"], "route": route,
                      "rationale": rationale, "reviewer": reviewer,
                      "reviewed_at": datetime.now(timezone.utc).isoformat(), "provenance": PROVENANCE,
                      "case_sha256": case_digest(case)}
            encoded = json.dumps(record, ensure_ascii=False) + "\n"
            if len(encoded.encode("utf-8")) > MAX_LINE_BYTES:
                raise ValueError("Review record exceeds the maximum encoded size.")
            out.write(encoded)
            out.flush()
    print(f"Review records written: {len(prior) + len(todo)} development cases. Labels in cases.json were not changed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
