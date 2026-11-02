import copy
import io
import json
import unittest
from email.message import Message
from unittest.mock import patch
import urllib.error

from evaluation import harness
from evaluation import live_jev


class FakeResponse:
    def __init__(self, obj=None, status=200, raw=None, headers=None):
        self.status = status
        self.raw = raw if raw is not None else json.dumps(obj).encode()
        self.headers = headers or {}
        self.closed = False
        self.read_sizes = []

    def read(self, size):
        self.read_sizes.append(size)
        return self.raw[:size]

    def close(self):
        self.closed = True


def response_for(case_id="gen-cargo-001", route="keep", cost=0.001):
    return {
        "id": "resp-synthetic",
        "model": live_jev.MODEL,
        "answers": {"route": {"type": "choice", "choice": route}},
        "usage": {"cost": cost},
    }


class LiveJevRunnerTests(unittest.TestCase):
    def test_every_development_category_can_be_selected(self):
        for case in self.cases:
            if case["split"] == "development":
                with self.subTest(case=case["id"]):
                    selected = live_jev.development_inputs(self.cases, 1, [case["id"]])
                    self.assertEqual(selected[0].case_id, case["id"])

    @classmethod
    def setUpClass(cls):
        cls.cases = harness.load_cases()

    def test_payload_contains_only_synthetic_facts_and_evidence(self):
        case = live_jev.development_inputs(self.cases, 1)[0]
        payload = live_jev.make_payload(case)
        self.assertEqual(payload["model"], "typesafe/jev-1.14")
        self.assertEqual(set(payload["state"]), {"case_id", "facts", "evidence_ids"})
        self.assertNotIn("label", json.dumps(payload))
        self.assertNotIn("split", json.dumps(payload))

    def test_heldout_cases_are_never_selected(self):
        altered = copy.deepcopy(self.cases)
        for item in altered:
            if item["split"] == "heldout":
                item["id"] = "must-never-send"
                item["facts"] = {"heldout_secret": "never-send"}
        selected = live_jev.development_inputs(altered, 5)
        self.assertTrue(all(case.case_id != "must-never-send" for case in selected))
        self.assertNotIn("never-send", json.dumps([live_jev.make_payload(c) for c in selected]))

    def test_hard_five_request_limit(self):
        with self.assertRaisesRegex(live_jev.LiveEvaluationError, "between 1 and 5"):
            live_jev.development_inputs(self.cases, 6)

    def test_explicit_case_ids_select_only_development_in_requested_order(self):
        ids = ["git-private-001", "gen-cargo-001"]
        selected = live_jev.development_inputs(self.cases, 5, ids)
        self.assertEqual([item.case_id for item in selected], ids)
        for heldout in (case["id"] for case in self.cases if case["split"] == "heldout"):
            with self.subTest(heldout=heldout), self.assertRaisesRegex(live_jev.LiveEvaluationError, "development IDs only"):
                live_jev.development_inputs(self.cases, 5, [heldout])
        with self.assertRaisesRegex(live_jev.LiveEvaluationError, "development IDs only"):
            live_jev.development_inputs(self.cases, 5, ["missing-case"])

    def test_explicit_selection_has_separate_hard_five_cap(self):
        ids = [case["id"] for case in self.cases if case["split"] == "development"][:6]
        with self.assertRaisesRegex(live_jev.LiveEvaluationError, "1 to 5 unique"):
            live_jev.development_inputs(self.cases, 5, ids)

    def test_live_request_uses_auth_header_and_never_returns_it(self):
        seen = []

        def fake(url, headers, body, timeout):
            seen.append((url, headers, json.loads(body), timeout))
            return FakeResponse(response_for())

        results = live_jev.run_live(self.cases, api_key="secret", transport=fake, max_requests=1)
        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0][0], live_jev.ENDPOINT)
        self.assertEqual(seen[0][1]["Authorization"], "Bearer secret")
        self.assertEqual(seen[0][2]["model"], live_jev.MODEL)
        self.assertNotIn("secret", json.dumps(results))
        # This deliberate disagreement is retained as a result, not adjusted to the label.
        self.assertEqual(self.cases[0]["label"]["route"], "inspect_metadata")
        self.assertEqual(results[0]["route"], "keep")
        self.assertEqual(set(results[0]), {"case_id", "route", "cost", "response_id", "model"})

    def test_input_char_budget_prevents_request(self):
        calls = []

        def fake(*args):
            calls.append(args)
            return FakeResponse(response_for())

        result = live_jev.run_live(self.cases, api_key="x", transport=fake, input_char_budget=1)
        self.assertEqual(result, [])
        self.assertEqual(calls, [])

    def test_cumulative_cost_stops_further_requests(self):
        calls = []

        def fake(*args):
            calls.append(args)
            return FakeResponse(response_for(cost=0.06))

        result = live_jev.run_live(self.cases, api_key="x", transport=fake,
                                   max_requests=5, cost_ceiling=0.1)
        self.assertEqual(len(calls), 2)
        self.assertEqual(len(result), 2)

    def test_unknown_cost_stops_without_storing_a_record(self):
        calls = []

        def fake(*args):
            calls.append(args)
            return FakeResponse({"id": "r", "model": live_jev.MODEL,
                                 "answers": {"route": {"type": "choice", "choice": "ask_user"}},
                                 "usage": {}})

        with self.assertRaisesRegex(live_jev.LiveEvaluationError, "budget unknown"):
            live_jev.run_live(self.cases, api_key="x", transport=fake, max_requests=5)
        self.assertEqual(len(calls), 1)

    def test_invalid_routes_and_answer_shapes_rejected(self):
        for answers in ({}, {"route": {"type": "noul", "choice": "ask_user"}},
                        {"route": {"type": "choice", "choice": "delete"}}):
            obj = response_for()
            obj["answers"] = answers
            with self.subTest(answers=answers), self.assertRaises(live_jev.LiveEvaluationError):
                live_jev.parse_result(obj)

    def test_invalid_or_missing_cost_rejected(self):
        for cost in (None, "0.01", -1, float("nan"), float("inf"), True):
            obj = response_for(cost=cost)
            with self.subTest(cost=cost), self.assertRaises(live_jev.LiveEvaluationError):
                live_jev.parse_result(obj)

    def test_malformed_usage_containers_rejected_as_unknown_budget(self):
        for usage in (None, [], "bad", {"cost": {"amount": 0.1}}):
            obj = response_for()
            obj["usage"] = usage
            with self.subTest(usage=usage), self.assertRaises(live_jev.LiveEvaluationError):
                live_jev.parse_result(obj)

    def test_metadata_must_be_safe_bounded_tokens(self):
        for field, value in (("id", "provider response\nforged"), ("id", "x" * 129),
                             ("model", "provider model\nsecret")):
            obj = response_for()
            obj[field] = value
            with self.subTest(field=field, value=value[:20]), self.assertRaisesRegex(live_jev.LiveEvaluationError, "metadata"):
                live_jev.parse_result(obj)

    def test_route_definitions_express_nonmutating_distinctions(self):
        criteria = live_jev.make_payload(live_jev.development_inputs(self.cases, 1)[0])["questions"]["route"]["criteria"]
        self.assertIn("Affirmatively retain", criteria["keep"])
        self.assertIn("Corroborate or gather metadata", criteria["inspect_metadata"])
        self.assertIn("known category", criteria["run_specialist"])
        self.assertIn("not a human review", criteria["reasoning_review"])
        self.assertIn("personal intent", criteria["ask_user"])

    def test_http_error_is_sanitized_and_response_closed(self):
        response = FakeResponse(raw=b"secret response payload", status=401)
        with self.assertRaisesRegex(live_jev.LiveEvaluationError, "request failed") as caught:
            live_jev._decode_response(response)
        self.assertNotIn("secret", str(caught.exception))
        self.assertTrue(response.closed)

    def test_declared_oversize_response_rejected_before_body_read(self):
        response = FakeResponse(raw=b"body", headers={"Content-Length": str(live_jev.MAX_RESPONSE_BYTES + 1)})
        with self.assertRaisesRegex(live_jev.LiveEvaluationError, "size limit"):
            live_jev._decode_response(response)
        self.assertEqual(response.read_sizes, [])
        self.assertTrue(response.closed)

    def test_malformed_response_length_is_rejected(self):
        for value in ("not-a-number", "-1"):
            response = FakeResponse(obj={}, headers={"Content-Length": value})
            with self.subTest(value=value), self.assertRaisesRegex(live_jev.LiveEvaluationError, "response length"):
                live_jev._decode_response(response)

    def test_auth_redirect_is_not_followed_or_disclosed(self):
        secret = "never-print-this-key"
        captured = []

        class FakeOpener:
            def open(self, request, timeout):
                captured.append((request, timeout))
                headers = Message()
                headers["Location"] = "https://attacker.example/" + secret
                raise urllib.error.HTTPError(request.full_url, 302, "redirect", headers, io.BytesIO(b""))

        with patch.object(live_jev.urllib.request, "build_opener", return_value=FakeOpener()) as builder:
            response = live_jev.urllib_transport(live_jev.ENDPOINT, {"Authorization": f"Bearer {secret}"}, b"{}", 1.0)
        with self.assertRaisesRegex(live_jev.LiveEvaluationError, "redirect rejected") as caught:
            live_jev._decode_response(response)
        self.assertEqual(len(captured), 1)
        self.assertEqual(captured[0][0].get_header("Authorization"), f"Bearer {secret}")
        self.assertIn(live_jev._NoRedirect, builder.call_args.args)
        self.assertNotIn(secret, str(caught.exception))

    def test_transport_errors_are_sanitized(self):
        def fail(*_):
            raise RuntimeError("Authorization: very-secret payload")

        with self.assertRaises(live_jev.LiveEvaluationError) as caught:
            live_jev.run_live(self.cases, api_key="very-secret", transport=fail, max_requests=1)
        self.assertEqual(str(caught.exception), "transport failed; evaluation stopped")
        self.assertNotIn("very-secret", str(caught.exception))

    def test_limits_are_finite_positive_and_cost_cap_is_one_dollar(self):
        for kwargs in ({"input_char_budget": 0}, {"cost_ceiling": float("nan")},
                       {"cost_ceiling": 1.01}, {"timeout_seconds": float("inf")}):
            with self.subTest(kwargs=kwargs), self.assertRaises(live_jev.LiveEvaluationError):
                live_jev.run_live(self.cases, api_key="x", transport=lambda *_: None, **kwargs)


if __name__ == "__main__":
    unittest.main()
