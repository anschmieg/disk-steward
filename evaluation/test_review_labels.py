import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import review_labels
from harness import load_cases


class ReviewLabelsTests(unittest.TestCase):
    def setUp(self):
        self.cases = load_cases()
        self.dev = {c["id"]: c for c in self.cases if c["split"] == "development"}
        self.heldout = {c["id"] for c in self.cases if c["split"] == "heldout"}

    def record(self, cid="gen-cargo-001", route="keep", rationale="reason", reviewer="R"):
        case = self.dev.get(cid)
        return {"schema": review_labels.SCHEMA, "case_id": cid, "route": route,
                "rationale": rationale, "reviewer": reviewer, "reviewed_at": "2026-10-31T00:00:00+00:00",
                "provenance": review_labels.PROVENANCE,
                "case_sha256": review_labels.case_digest(case) if case else "0" * 64}

    def test_valid_review_parses_and_disclaims_identity_verification(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "review.jsonl"
            path.write_text(json.dumps(self.record()) + "\n", encoding="utf-8")
            self.assertEqual(review_labels.load_review(path, self.dev, self.heldout), [self.record()])
            self.assertIn("not independently verified", review_labels.PROVENANCE)

    def test_display_payload_excludes_labels_and_split_metadata(self):
        case = next(iter(self.dev.values()))
        payload = review_labels.display_payload(case)
        self.assertEqual(set(payload), {"facts", "evidence_ids"})
        self.assertNotIn("route", json.dumps(payload))
        self.assertNotIn("heldout", json.dumps(payload))

    def test_rejects_duplicate_conflict_unknown_and_heldout_ids(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "review.jsonl"
            cases = [([self.record(), self.record()], "duplicate"),
                     ([self.record(), self.record(route="ask_user")], "conflicting"),
                     ([self.record(cid="missing")], "unrecognized"),
                     ([self.record(cid=next(iter(self.heldout)))], "held-out")]
            for records, message in cases:
                path.write_text("".join(json.dumps(r) + "\n" for r in records), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, message):
                    review_labels.load_review(path, self.dev, self.heldout)

    def test_rejects_bad_schema_route_and_empty_fields(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "review.jsonl"
            for record in (dict(self.record(), route="delete"), dict(self.record(), rationale=" "),
                           dict(self.record(), schema="other")):
                path.write_text(json.dumps(record) + "\n", encoding="utf-8")
                with self.assertRaises(ValueError):
                    review_labels.load_review(path, self.dev, self.heldout)

    def test_rejects_nonstring_ids_and_routes_and_bad_timestamps(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "review.jsonl"
            for record in (dict(self.record(), case_id=[]), dict(self.record(), route=[]),
                           dict(self.record(), reviewed_at="yesterday"),
                           dict(self.record(), reviewed_at="2026-10-31T00:00:00")):
                path.write_text(json.dumps(record) + "\n", encoding="utf-8")
                with self.assertRaises(ValueError):
                    review_labels.load_review(path, self.dev, self.heldout)

    def test_rejects_changed_facts_or_evidence_digest(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "review.jsonl"
            path.write_text(json.dumps(self.record()) + "\n", encoding="utf-8")
            changed = dict(self.dev)
            changed_case = dict(changed["gen-cargo-001"], facts={"category": "changed"})
            changed["gen-cargo-001"] = changed_case
            with self.assertRaisesRegex(ValueError, "changed"):
                review_labels.load_review(path, changed, self.heldout)

    def test_rejects_oversized_records_and_fields(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "review.jsonl"
            for content in (" " * review_labels.MAX_LINE_BYTES + "\n",
                            json.dumps(self.record(rationale="x" * (review_labels.MAX_RATIONALE_LENGTH + 1))) + "\n",
                            json.dumps(self.record(reviewer="x" * (review_labels.MAX_REVIEWER_LENGTH + 1))) + "\n"):
                path.write_text(content, encoding="utf-8")
                with self.assertRaises(ValueError):
                    review_labels.load_review(path, self.dev, self.heldout)

    def test_invalid_encoding_and_deep_json_are_cleanly_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "review.jsonl"
            for payload in (b"\xff\n", b"[" * 2000 + b"]" * 2000 + b"\n"):
                path.write_bytes(payload)
                with self.assertRaises(ValueError):
                    review_labels.load_review(path, self.dev, self.heldout)

    def test_requires_terminal_before_creating_output(self):
        with tempfile.TemporaryDirectory() as td:
            output = Path(td) / "out.jsonl"
            with patch.object(review_labels.sys.stdin, "isatty", return_value=False):
                self.assertEqual(review_labels.main(["--output", str(output)]), 2)
            self.assertFalse(output.exists())

    def test_output_creation_is_exclusive(self):
        with tempfile.TemporaryDirectory() as td:
            output = Path(td) / "out.jsonl"
            output.write_text("preserve", encoding="utf-8")
            with patch.object(review_labels.sys.stdin, "isatty", return_value=True), \
                 patch.object(review_labels.sys.stdout, "isatty", return_value=True):
                self.assertEqual(review_labels.main(["--output", str(output)]), 2)
            self.assertEqual(output.read_text(encoding="utf-8"), "preserve")


if __name__ == "__main__":
    unittest.main()
