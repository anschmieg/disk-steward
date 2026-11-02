# M0 HTTP framing completion evidence

Purpose: distinguish Git parser completion from observing the end of the HTTP body. Add bounded, read-only per-attempt response-state metadata to the candidate. Register a response attempt after validated scope/headers and before connecting. Track Pending, Completed (successful body EOF observed), Failed (setup/read/framing/deadline error) and Abandoned (body dropped before EOF). Preserve terminal states and never perform I/O in Drop.

Expose only snapshot counts and an all-completed predicate that is false for zero responses or any unresolved/error state. Private ledger has at most64 attempts, and poisoned metadata access fails closed. No caller can mutate ledger evidence. Do not equate this metadata with Git refs/object proof, cleanup, worker-stop, approval or milestone completion. Do not automatically drain responses.

Tests use private loopback TLS or the production decoder; public/unit connector cannot fall back to remote DNS. Cover EOF, early packet flush without HTTP termination, malformed tails, sticky/Drop behavior, setup errors, no-attempt status and ledger saturation without unbounded allocation/networking.

Branch atop PR41 b3613f7; main final baseline. Bounded Luna owns candidate lib.rs/README only; root owns scope/progress and concrete review before execution. Version0.0.17, focused tests/strictClippy/fmt/version/diff/current-headCI. Existing38/39/40/41watches notpolled. Host13GiB observed before work. Full-response draining/finalization and broader Git/worker/store/private-auth gates remain open.
