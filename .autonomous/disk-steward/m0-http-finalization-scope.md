# M0 explicit HTTP response finalization

Purpose: gix may return at a Git packet flush before HTTP framing ends. Add a consuming backend-only explicit finalization operation that releases the gix reader, drains retained tails through the existing bounded Body/TLS accounting/cancellation/deadline path, and reports immutable framing metadata plus final ingress and any drain error kind. No verification receipt, Git object/ref proof, cleanup or worker authority.

Response/PostState Drop only transfer owned bodies into a private bounded queue; they never read, drain or reconnect. Pending retained bodies remain incomplete until explicit finalization; dropping the transport without finalization drops them as abandoned. Queue and ledger are capped at64, no allocation from declared body length, release locks before draining, no ownership cycles. Cancellation, malformed/truncated tails, expired deadline, poison and exhausted ledger must never yield completed evidence. Preserve failed-attempt byte observations in final report.

Tests private loopback TLS/production decoder only, review concrete code before execution. Cover valid framing after early Git flush, truncated/malformed tails, canceled/expired finalization, ordinary no-I/O drop, zero attempts and poisoned metadata. No remote/userfiles/app/renderer access.

Branch feat/m0-http-response-finalization atop PR45 f042b10; main integrationbaseline b9c80d3 via pendingstack. Bounded Luna owns lib.rs/README, root scope/progress/review/integration; no duplicate agents/builds. Host9.3GiB free, no usercachecleanup. Focused tests/strictClippy/fmt/version0.0.17/diff and current-headCI. Watched39–45 not polled.
