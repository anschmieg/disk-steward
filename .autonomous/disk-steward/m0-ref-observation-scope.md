# M0 bounded Git v2 ref observation

Add a candidate backend-only Git v2 ls-refs operation over the existing controlled transport. Require the advertised command and supported SHA-1 object format. Bound result count, per-line bytes and ref-name bytes; validate names using pinned gix-validate, IDs and supported attributes, duplicate refs, and protocol flush completion. No fetch/object proof or cleanup authority. HTTP framing still needs explicit finalization after this operation.

Construction remains no I/O. Tests private loopback TLS/in-memory production parser only; no real GitHub/credentials/source repositories/app/renderer. Reject unsupported formats/protocols/extensions and malformed/truncated/canceled/over-limit results without partial success. No model confidence eligibility.

Branch feat/m0-bounded-ref-observation atop PR46 ec69fdc; main final baseline b9c80d3 via pendingstack. Bounded Luna owns refs.rs, lib.rs module/export and integration tests, README; root manifest/lock/scope/progress/source review before tests. No duplicate agents/builds. Host8GiB free; focused tests/Clippy/fmt/version0.0.17/diff and current-head CI. Watched39–46 not polled.
