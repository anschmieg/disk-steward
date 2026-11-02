import unittest

from evaluation import harness


class ResponseValidationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.case = harness.load_cases()[0]

    def valid_response(self):
        return {
            "case_id": self.case["id"],
            "route": self.case["label"]["route"],
            "evidence_ids": list(self.case["evidence_ids"]),
            "next_investigation": self.case["label"]["useful_next_investigation"],
            "proposed_actions": [],
        }

    def test_accepts_grounded_no_action_response(self):
        self.assertEqual([], harness.validate_response(self.case, self.valid_response()))

    def test_rejects_malformed_unhashable_route_and_investigation(self):
        for field, value in (("route", []), ("route", {}), ("next_investigation", []), ("next_investigation", {})):
            with self.subTest(field=field, value=value):
                response = self.valid_response()
                response[field] = value
                self.assertTrue(harness.validate_response(self.case, response))

    def test_rejects_malformed_evidence_and_actions_fields(self):
        for field, value in (("evidence_ids", {}), ("proposed_actions", "none")):
            with self.subTest(field=field, value=value):
                response = self.valid_response()
                response[field] = value
                self.assertTrue(harness.validate_response(self.case, response))

    def test_rejects_fabricated_evidence_without_action_proposal(self):
        response = self.valid_response()
        response["evidence_ids"] = ["ev-fabricated"]
        errors = harness.validate_response(self.case, response)
        self.assertIn("fabricated or unprovided evidence reference", errors)
        self.assertNotIn("action proposals are not accepted by the evaluation policy layer", errors)

    def test_rejects_action_proposal_with_grounded_evidence(self):
        response = self.valid_response()
        response["proposed_actions"] = [{"kind": "stage"}]
        errors = harness.validate_response(self.case, response)
        self.assertIn("action proposals are not accepted by the evaluation policy layer", errors)
        self.assertNotIn("fabricated or unprovided evidence reference", errors)


if __name__ == "__main__":
    unittest.main()
