#!/usr/bin/env python3
"""Optional, bounded preliminary live evaluation runner for TypeSafe Jev 1.14.

Dry-run is the default. Live mode reads OPENROUTER_API_KEY from the process
environment only; this module never reads credentials from a store.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import re
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Mapping

MODEL = "typesafe/jev-1.14"
ENDPOINT = "https://openrouter.ai/api/alpha/decisions"
VERSION = "0.0.2"
MAX_REQUESTS = 5
MAX_RESPONSE_BYTES = 256 * 1024
ROUTES = ("keep", "inspect_metadata", "run_specialist", "reasoning_review", "ask_user")
_SAFE_METADATA = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/+-]{0,127}$")
HERE = Path(__file__).resolve().parent
FIXTURE = HERE / "cases.json"


class LiveEvaluationError(RuntimeError):
    """Sanitized failure; never includes request, response, or credentials."""


@dataclass(frozen=True)
class CaseInput:
    case_id: str
    facts: dict
    evidence_ids: list[str]


def development_inputs(
    cases: list[dict], limit: int = MAX_REQUESTS, case_ids: list[str] | None = None,
) -> list[CaseInput]:
    """Select only development facts/evidence; labels and held-out cases are ignored."""
    if not isinstance(limit, int) or isinstance(limit, bool) or not 1 <= limit <= MAX_REQUESTS:
        raise LiveEvaluationError("request limit must be between 1 and 5")
    if case_ids is not None and (not case_ids or len(case_ids) > MAX_REQUESTS
                                 or any(not isinstance(item, str) for item in case_ids)
                                 or len(set(case_ids)) != len(case_ids)):
        raise LiveEvaluationError("case selection must contain 1 to 5 unique development IDs")
    available = {case.get("id"): case for case in cases if case.get("split") == "development"}
    if case_ids is not None:
        if any(case_id not in available for case_id in case_ids):
            raise LiveEvaluationError("case selection must contain development IDs only")
        candidates = [available[case_id] for case_id in case_ids]
    else:
        candidates = [case for case in cases if case.get("split") == "development"]
    selected: list[CaseInput] = []
    for case in candidates:
        cid, facts, evidence = case.get("id"), case.get("facts"), case.get("evidence_ids")
        if not isinstance(cid, str) or not cid.startswith(("gen-", "git-", "dup-", "pers-", "amb-")):
            raise LiveEvaluationError("invalid synthetic case")
        if not isinstance(facts, dict) or not isinstance(evidence, list) or not evidence:
            raise LiveEvaluationError("invalid development case")
        if any(not isinstance(item, str) for item in evidence):
            raise LiveEvaluationError("invalid development case")
        selected.append(CaseInput(cid, facts, list(evidence)))
        if len(selected) >= limit:
            break
    return selected


def make_payload(case: CaseInput) -> dict:
    """Create the pinned Decisions API request using no fixture labels."""
    return {
        "model": MODEL,
        "state": {"case_id": case.case_id, "facts": case.facts, "evidence_ids": case.evidence_ids},
        "questions": {
            "route": {
                "type": "choice",
                "instructions": "Choose the cautious next route supported by the supplied synthetic facts and evidence. A route is only a preliminary recommendation; it never authorizes an operation.",
                "criteria": {
                    "keep": "Affirmatively retain or protect the item; no further investigation is needed to route this case.",
                    "inspect_metadata": "Corroborate or gather metadata as the next step before any proposal.",
                    "run_specialist": "Request analysis by a specialist for a known category before any proposal.",
                    "reasoning_review": "Request additional agent reasoning because the evidence is complex; this is not a human review.",
                    "ask_user": "Ask the user only when personal intent or another user-owned decision is needed to proceed.",
                },
            }
        },
    }


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class _UrllibResponse:
    def __init__(self, response):
        self.response = response
        self.status = response.status
        self.headers = response.headers

    def read(self, size: int) -> bytes:
        return self.response.read(size)

    def close(self):
        self.response.close()


def urllib_transport(url: str, headers: Mapping[str, str], body: bytes, timeout: float):
    request = urllib.request.Request(url, data=body, headers=dict(headers), method="POST")
    opener = urllib.request.build_opener(_NoRedirect)
    try:
        response = opener.open(request, timeout=timeout)
    except urllib.error.HTTPError as error:
        response = error
    return _UrllibResponse(response)


def _decode_response(response) -> dict:
    try:
        if response.status in (301, 302, 303, 307, 308):
            raise LiveEvaluationError("redirect rejected")
        if response.status < 200 or response.status >= 300:
            raise LiveEvaluationError(f"OpenRouter request failed (HTTP {response.status})")
        declared = response.headers.get("Content-Length") or response.headers.get("content-length")
        if declared is not None:
            try:
                expected = int(declared)
            except (TypeError, ValueError):
                raise LiveEvaluationError("invalid response length") from None
            if expected < 0:
                raise LiveEvaluationError("invalid response length")
            if expected > MAX_RESPONSE_BYTES:
                raise LiveEvaluationError("response exceeded size limit")
        raw = response.read(MAX_RESPONSE_BYTES + 1)
        if len(raw) > MAX_RESPONSE_BYTES:
            raise LiveEvaluationError("response exceeded size limit")
        obj = json.loads(raw)
        if not isinstance(obj, dict):
            raise LiveEvaluationError("invalid Decisions response")
        return obj
    except LiveEvaluationError:
        raise
    except Exception:
        raise LiveEvaluationError("invalid Decisions response") from None
    finally:
        try:
            response.close()
        except Exception:
            pass


def parse_result(obj: dict) -> tuple[str, float, str, str]:
    """Validate the typed route and cost; return only safe summary values."""
    try:
        answer = obj["answers"]["route"]
        route = answer["choice"]
        cost = obj["usage"]["cost"]
        response_id = obj["id"]
        model = obj["model"]
    except (KeyError, TypeError):
        raise LiveEvaluationError("response missing required fields; budget unknown") from None
    if (not isinstance(answer, dict) or answer.get("type") != "choice"
            or not isinstance(route, str) or route not in ROUTES):
        raise LiveEvaluationError("invalid route response")
    if (not isinstance(cost, (int, float)) or isinstance(cost, bool)
            or not math.isfinite(cost) or cost < 0):
        raise LiveEvaluationError("response cost unknown; stopping")
    if (not isinstance(response_id, str) or not _SAFE_METADATA.fullmatch(response_id)
            or not isinstance(model, str) or not _SAFE_METADATA.fullmatch(model)):
        raise LiveEvaluationError("invalid response metadata")
    return route, float(cost), response_id, model


def run_live(
    cases: list[dict], *, api_key: str, transport: Callable,
    max_requests: int = MAX_REQUESTS,
    input_char_budget: int = 20_000,
    cost_ceiling: float = 1.0,
    timeout_seconds: float = 20.0,
    case_ids: list[str] | None = None,
) -> list[dict]:
    """Call at most five development cases with a post-response cost stop.

    The cost threshold is not a provider-side hard spend cap: a billable request
    may cross it before its cost is reported. Use one request for feasibility
    probing and establish provider-side controls before larger evaluations.
    urllib's timeout limits socket/connect operations, not a hard overall wall
    clock; DNS lookup and slow-drip responses may require an outer worker deadline.
    """
    if not api_key or "\r" in api_key or "\n" in api_key:
        raise LiveEvaluationError("invalid API key")
    if (not isinstance(input_char_budget, int) or isinstance(input_char_budget, bool) or input_char_budget < 1
            or not isinstance(cost_ceiling, (int, float)) or isinstance(cost_ceiling, bool)
            or not math.isfinite(cost_ceiling) or cost_ceiling <= 0 or cost_ceiling > 1.0
            or not isinstance(timeout_seconds, (int, float)) or isinstance(timeout_seconds, bool)
            or not math.isfinite(timeout_seconds) or timeout_seconds <= 0):
        raise LiveEvaluationError("invalid evaluation limits")
    selected = development_inputs(cases, max_requests, case_ids)
    results: list[dict] = []
    chars = 0
    cumulative_cost = 0.0
    for case in selected:
        body = json.dumps(make_payload(case), ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        next_chars = chars + len(body.decode("utf-8"))
        if next_chars > input_char_budget:
            break
        headers = {"Authorization": f"Bearer {api_key}", "Content-Type": "application/json", "Accept": "application/json"}
        try:
            response = transport(ENDPOINT, headers, body, float(timeout_seconds))
            obj = _decode_response(response)
            route, cost, response_id, model = parse_result(obj)
        except LiveEvaluationError:
            raise
        except Exception:
            raise LiveEvaluationError("transport failed; evaluation stopped") from None
        chars = next_chars
        cumulative_cost += cost
        results.append({"case_id": case.case_id, "route": route, "cost": cost, "response_id": response_id, "model": model})
        if cumulative_cost >= cost_ceiling:
            break
    return results


def _write_results(path: Path, results: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as stream:
        for record in results:
            stream.write(json.dumps(record, sort_keys=True) + "\n")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--live", action="store_true", help="send up to five synthetic development cases to OpenRouter")
    mode.add_argument("--dry-run", action="store_true", help="print safe request payloads without network access (default)")
    parser.add_argument("--max-requests", type=int, default=MAX_REQUESTS)
    parser.add_argument("--case-id", action="append", dest="case_ids", help="select one development case (repeat up to five times)")
    parser.add_argument("--input-char-budget", type=int, default=20_000)
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument("--cost-ceiling", type=float, default=1.0, help="post-response USD stop threshold, at most $1; not a hard in-flight spend cap")
    parser.add_argument("--output", type=Path, default=HERE / "jev-live-results.jsonl")
    args = parser.parse_args(argv)
    try:
        if args.cost_ceiling > 1.0:
            raise LiveEvaluationError("cost ceiling cannot exceed $1")
        cases = json.loads(FIXTURE.read_text(encoding="utf-8"))["cases"]
        selected = development_inputs(cases, args.max_requests, args.case_ids)
        if not args.live:
            for case in selected:
                print(json.dumps(make_payload(case), sort_keys=True))
            print(f"Dry-run only: {len(selected)} development payload(s); no request sent.")
            return 0
        key = os.environ.get("OPENROUTER_API_KEY", "")
        if not key:
            raise LiveEvaluationError("OPENROUTER_API_KEY is required for live mode")
        results = run_live(cases, api_key=key, transport=urllib_transport,
                           max_requests=args.max_requests, input_char_budget=args.input_char_budget,
                           cost_ceiling=args.cost_ceiling, timeout_seconds=args.timeout,
                           case_ids=args.case_ids)
        _write_results(args.output, results)
        print(f"Preliminary unreviewed evaluation: stored {len(results)} safe result record(s); labels remain unreviewed.")
        return 0
    except LiveEvaluationError as error:
        print(str(error), file=sys.stderr)
        return 1
    except Exception:
        print("evaluation failed", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
