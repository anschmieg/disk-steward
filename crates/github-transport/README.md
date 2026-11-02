# GitHub HTTPS transport

An isolated candidate gix HTTP transport adapter for GitHub smart HTTP. The
factory validates a repository URL and returns an unconnected gix transport;
network activity begins only when the caller starts its handshake. It accepts
only `https://github.com/{owner}/{repo}` or the same path ending in `.git`,
with no userinfo, port, query, fragment, escapes, or additional path segments.
The exact accepted path is preserved.

The adapter has a shared raw socket ingress ceiling and monotonic attempt
deadline across the gix GET and POST. It uses rustls with the bundled WebPKI
root set and hostname verification. The root set is compiled into the binary;
updating Mozilla roots and matching packaged-app trust policy remain release
gates, and this is not the operating system trust store. Redirects, non-2xx
responses, compression, malformed/ambiguous headers, and oversized POST
requests fail closed. Responses may use either a single `Content-Length` or one
`Transfer-Encoding: chunked` coding; chains, duplicates, or both framing modes
are rejected. Chunk payloads stream through a bounded decoder above the raw
TLS byte budget. Chunk control lines and the aggregate trailer section are
limited to 8 KiB, with at most 32 trailer fields. Unknown syntactically valid
chunk extensions are ignored; forbidden, malformed, or control-bearing trailer
fields fail closed and trailers are never exposed as response headers. POST
request data is buffered only up to its fixed cap and sent after gix closes the
request writer.

Credentials are optional and explicit. This crate does not access Keychain,
credential helpers, environment variables, proxies, or renderer APIs. Callers
must supply credentials through their own app-owned broker. Errors are
sanitized and do not include input URLs, headers, bodies, or credentials.

The synchronous system DNS lookup happens outside a cancellable socket read;
checking the deadline before and after it does not make DNS itself bounded. A
caller can obtain an attempt-scoped, cloneable `CancellationHandle` from
`GitHubTransport::cancellation_handle()` and cancel the attempt from another
thread. Cancellation is one-way and affects only that transport attempt. It is
checked around setup, reads, writes, flushes, buffered response payload access,
and the lazy POST body wait. Socket I/O and POST waits use timeout slices of at
most 100 ms to recheck the signal; this is not a hard wall-clock latency bound.
The synchronous resolver and operating-system connect call are not cancellable
and have no cancellation latency guarantee. A production caller still needs an
outer worker with a hard deadline. Cancellation errors use
`ErrorKind::ConnectionAborted` so standard `read_exact` implementations do not
silently retry them as transient `Interrupted` errors.
This crate is an implementation candidate, not a completed Git fetch, M0
validation, or production-readiness claim. Its fixture endpoint and trust roots
are private test-only seams.

An ignored exact-child unit test can be launched by
`tests::outer_deadline_stops_actual_candidate_tls_handshake`. Its child runs the
actual candidate transport against the private loopback TLS fixture, which
validates the complete GET target before printing a bounded readiness sentinel
and then withholds the HTTP response. The parent uses a three-second monotonic
outer deadline against a 30-second candidate attempt, captures at most 1 KiB
of child stdout, and kills, waits for, and joins the bounded output reader
before checking the evidence. This demonstrates process-level termination of
this blocked handshake on the platform where run. It does not establish hard
OS kill latency, descendant-tree termination, storage cleanup, worker
integration, or M0 completion. The ignored child is intentionally excluded
from ordinary test runs and must be invoked through its exact parent test.

`observed_ingress_bytes()` reports socket bytes charged beneath TLS, including
failed attempts, without exposing the socket or credentials. The public gix
wrapper forces request tracing off and rejects later identity changes and
transport configuration. Tokens, Basic Authorization strings and gix's internal
copies are not proven zeroized; no secure-memory or credential-broker gate is
claimed. The per-request buffered upload cap is 1 MiB, and framing restrictions
may reject otherwise legitimate GitHub responses. Such outcomes are incomplete
verification, never grounds to relax an approval or removal check.

The ingress count measures bytes returned by socket reads beneath TLS; it does
not measure NIC traffic, bytes buffered by the operating system, or bytes that
arrive after a read returns. Deterministic unit coverage of the shared budget
admission and charging path can establish how returned bytes interact with
cancellation and the configured cap. It cannot establish operating-system
read latency or a hard cancellation-latency bound. That evidence alone is not
a full M0 validation claim.

`GitHubTransport::http_framing_snapshot()` returns bounded per-attempt response
counts: pending, completed, failed, and abandoned. At most 64 responses are
tracked per transport; an over-limit attempt fails before connecting and
permanently makes `all_responses_completed()` false. Completion means the
adapter observed the HTTP framing terminator while reading the response body.
A gix packet flush alone does not complete that framing. Dropping a body before
its terminator records it as abandoned without draining or performing other I/O;
setup and read errors record failure. A snapshot is point-in-time metadata only.
Even when all tracked bodies completed, it is not evidence of a complete Git
fetch, repository verification, cleanup authorization, or worker termination.

Chunk extensions and trailer values use this candidate's bounded ASCII policy.
Framing validation applies as the response body is consumed; callers must not
infer whole-message validation from an early Git packet flush. Full-fetch
integration still needs to establish response completion before issuing a
verification receipt. The decoder's fixed 8 KiB payload buffer is independent
of declared chunk sizes, and its framing bytes remain charged by the socket
counter beneath TLS.
