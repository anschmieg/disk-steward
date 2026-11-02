# Purge process-death boundary evidence — 2026-11-02

Baseline: main f326a1b plus PR17 c4b14eb (verification probes, CI pending at start). Branch feat/m1c-purge-boundary-evidence keeps the next change separate. The product remains a storage investigation/review tool with only app-created disposable fixture operations enabled; no user-file cleanup or release.

Gap: fixture-purge subprocess tests prove prepared/claimed/terminal batch persistence, while the frozen purge contract also calls for real process termination around intent, unlink, directory sync and outcome persistence. In-process state fabrication is not equivalent evidence.

Scope: private cfg(test) hooks at actual lifecycle purge checkpoints and subprocess tests invoking the real purge method on parent-owned fresh fixtures. No environment-controlled production fault injection, public hook/API, renderer capability or cleanup scope change. Splitting the existing unlink/sync short-circuit into equivalent individual checks is permitted only to expose those success boundaries in tests. Keep existing uncertainty/error behavior.

Acceptance: abrupt child exits preserve durable intent and conservative uncertain outcomes until terminal journaling; no automatic retry or restoration after uncertainty; pre-unlink fixture stays present; post-unlink fixture remains absent; terminal outcomes remain terminal; unrelated fixture/sentinel bytes survive. Validate meaningful assertions on Linux/macOS CI, formatting and strict Clippy, then record exact remaining untested boundaries. No milestone-complete claim from tests alone.
