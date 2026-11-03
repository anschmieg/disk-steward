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
requests fail closed. Only HTTP `200` is accepted as successful; `401` and
`403` map to `PermissionDenied`, while all other statuses (including other 2xx
responses and `304`) fail closed. This candidate has no cache or conditional
revalidation support. See the [Git HTTP protocol](https://git-scm.com/docs/http-protocol)
for discovery and access-denial semantics. Socket and ref-observation errors
preserve I/O kinds through bounded cause-metadata inspection, while public
messages discard response/header/credential diagnostic context. Responses may
use either a single `Content-Length` or one
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

`GitHubTransport::read_advertised_refs()` performs an upload-pack handshake and
issues protocol v2 `ls-refs` only when the server actually selected v2,
advertised exactly one `ls-refs` capability, and advertised absent (default
SHA-1) or exactly one `object-format=sha1`. It requests `peel` and `symrefs` and
returns at most 1,024 parsed refs. The v2 GET advertisement is capped at 64 KiB
of decoded body data before gix parses capabilities. Each ls-refs packet line is limited to 4 KiB and
each refname/target to 1 KiB. It preserves refname and symref target bytes and
returns fixed-width SHA-1 IDs. This operation observes remote claims only: it
does not fetch objects, prove object existence, or authorize removal. Call
`finalize_http_framing()` separately; the ls-refs flush is not an HTTP framing
receipt.

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
A gix packet flush alone does not complete that framing. When gix drops a
response reader, the adapter retains its body without reading it, so its ledger
entry remains pending while the transport lives. Uninitialized lazy POSTs are
marked failed when their state is dropped; finalization never sends an
unwritten request. Dropping the transport without finalizing releases retained
bodies without I/O and records them as abandoned. Setup/read errors record
failure. A snapshot is point-in-time metadata only.

Call `GitHubTransport::finalize_http_framing(self)` to explicitly consume the
transport and drain retained response tails. It first drops gix's reader, then
drains retained bodies through the same chunk decoder, raw socket ingress
budget, deadline, and cancellation signal. Each response keeps its existing
fixed-size decoder buffer; no response payload is accumulated by the drain.
At most 64 response bodies/connections can be retained. The returned
`HttpFinalizationReport` contains the final framing snapshot, observed ingress
bytes, and an optional static `io::ErrorKind` for a drain/cancellation/deadline
failure. A failed drain returns a report with incomplete framing where
observable; remaining bodies are abandoned when released. Its `is_complete()`
predicate requires no drain error and at least one response with every tracked
response completed. Historical `HttpFramingSnapshot::all_responses_completed`
does not include a later finalizer error, so use the report predicate for this
operation.

Finalization may read/discard bytes that gix has not consumed. It checks HTTP
framing only: it does not parse the discarded Git payload, prove a complete Git
fetch, establish repository integrity, authorize cleanup, or prove worker
termination. Even a complete report is transport metadata, not a Git
correctness receipt.

Chunk extensions and trailer values use this candidate's bounded ASCII policy.
Framing validation applies as the response body is consumed; callers must not
infer whole-message validation from an early Git packet flush. Full-fetch
integration still needs to establish response completion before issuing a
verification receipt. The decoder's fixed 8 KiB payload buffer is independent
of declared chunk sizes, and its framing bytes remain charged by the socket
counter beneath TLS.

Prefer `GitHubTransport::observe_refs(self)` for a combined observation. It
performs bounded `ls-refs` parsing and then explicitly finalizes HTTP framing,
including after a protocol error. The immutable `RefObservationReport` exposes
refs only when both stages complete; a valid empty list is `Some(&[])`, while
incomplete observations have no exposed refs. Protocol error kinds and HTTP
framing/ingress/error evidence remain separately available. Poisoned evidence
returns an error. Even a complete ref observation is not object verification,
a guarantee of future remote retention, or cleanup authority. The low-level
stages remain available for callers that explicitly assess both outcomes.

The shared budget controller accepts cumulative ingress with
`record_ingress_total(job, total)`. Its caller must bind the report to the exact
job and counter domain. Repeated totals do not double charge; a regressed
counter blocks a running job and never reduces usage. Candidate tests apply
actual TLS totals before and after finalization, including to a modeled canceled
job, without acknowledging worker stop or cleanup. This is accounting
integration evidence, not production job cancellation routing, durable worker
reconciliation, or verification completion.

On Unix, a private interoperability test routes the candidate's actual v2 GET
and `ls-refs` POST through loopback TLS to `/usr/bin/git upload-pack`. It creates
only a new private bare SHA-1 fixture with a commit and annotated tag. Git runs
with a cleared environment, isolated configuration, fixed commands, bounded
stdin/stdout, and deadline/reap handling. Fixture identity is checked before
commands and cleanup. The test checks real HEAD/main/tag and peeled metadata
and both HTTP finalizations. This is local ref-protocol evidence on the Git
version/platform where run, not fetching, object completeness, a broad version
matrix, real GitHub/private authentication, or cleanup eligibility.

The closed Unix interoperability fixture also requests a real v2 `fetch` for
its advertised commit and annotated tag. It accepts only the expected packfile
section and bounded sideband packets, retains at most 512 pack bytes, and
requires all three discovery/ls-refs/fetch HTTP responses to finalize. In a separate fresh bare receiver,
fixed Git commands run `index-pack --strict`, `fsck --strict --full`, and check
that the expected blob, tree, commit and tag exist. Separate fresh receivers
must reject a damaged checksum and a truncated pack. The subprocess input
ceiling remains 512 bytes. This is a small local protocol experiment using
system Git, not a production fetch API or durable budgeted verification store;
it does not establish source-state reachability, real private GitHub access,
large-pack behavior, or removal eligibility. The request and sideband layout
follow the [Git v2 protocol](https://git-scm.com/docs/gitprotocol-v2).

`download_advertised_pack(&mut sink)` is an inactive backend streaming candidate.
It reads fresh refs, refuses empty advertisements or more than 64 distinct tips,
and requests all those SHA-1 tips without shallow, filter, thin-pack or URI
options. It accepts only the expected v2 packfile section and sideband channels,
checks the `PACK` signature, bounds decoded pack/progress/packet counts, and
finalizes HTTP even after an operation error. `PackTransferReport` records
accepted sink bytes and sanitized error kinds. `is_complete_transfer()` means
only that the transfer framing completed; even malformed object data with a
`PACK` prefix can pass this predicate. Object integrity and completeness still
require an independent verifier in an isolated store.

Partial bytes remain in the caller's sink on failure. The caller must enforce
owned-store allocation, aggregate use and free-space admission before writes,
and supervise blocking sink operations in an outer worker; this API provides
neither filesystem authority nor a bound on arbitrary sink latency. It does not
flush or sync the sink, promise a remote ref snapshot, or authorize cleanup.
The app and renderer do not call this candidate.
An operation failure cancels the attempt before HTTP finalization, so a sink's
space refusal cannot trigger a download of the remaining pack merely to drain
its response. Finalization still records incomplete/failed framing metadata;
it cannot turn the operation failure into success.

The real-Git Unix fixture now streams the candidate pack into an exclusively
created private file in its task-owned root. A test-only sink checks held/named
file and root identities, samples volume free space, asks the shared budget
controller for admission, and retains observed allocation high-water charges
around writes and synchronization. The logical fixture limit is 512 bytes;
reported allocation may exceed it. Independent strict Git import checks still
use the bounded captured contents. A separate candidate exchange reaches a
simulated five-byte sink limit and must remain incomplete with its partial
file bytes and budget charge retained.

This connects transfer and file accounting for one local attempt. It does not
establish durable aggregate enforcement across stores, directory/journal/index
overhead, continuously reserved free space or production worker shutdown.
The five-byte refusal is injected evidence, distinct from the native volume
reserve sample in the successful fixture. No live GitHub endpoint is used.

A separate exact-child test exercises the outer deadline after the actual
candidate reaches a blocking sink during a private TLS pack transfer. The sink
accepts the four-byte `PACK` prefix, cancels the cooperative handle, then emits
a fixed readiness marker only when the next expected write enters its deliberate
block. The parent requires the child to remain blocked through its three-second
deadline, then kills and reaps that exact process and joins its bounded stdout
reader before asserting evidence. This uses an in-memory sink and no files or
child subprocesses. It demonstrates why arbitrary blocking `Write` calls still
need an outer worker; it does not establish production worker/store integration,
OS kill latency, flush durability or a milestone acceptance gate.

On Linux/macOS, the closed file sink also holds a leased store registry and
commits a fixed 128 KiB reservation before exclusively creating its payload.
It samples volume reserve again after registry creation and checks the stored
reservation/identity binding during sink operations. Observed payload usage
above that admission cancels the attempt while retaining its actual charge.
Both the real-Git success case and five-byte refusal then close the sink and
registry, reopen the record, and require exact reservation retention plus
reconciliation-based refusal of further admission. A pre-existing unreconciled
registry must refuse before any payload file is created.

This is a one-store test integration with in-process close/reopen. The fixed
reservation is not a measured whole-store footprint or reserved filesystem
capacity; registry/control overhead, shared app-wide admission, process death,
power loss and production worker lifecycle remain separate gates. The fixture
is gated to the registry adapter's Linux/macOS targets. No application or
renderer calls this test sink.

The fixed system-Git fixture supervises repository setup and local receiver
verification with a ten-second per-command budget, while upload-pack RPC
commands retain a three-second budget. Shared macOS CI exceeded the earlier
three-second setup budget; the failing child was reaped. These are test-fixture
limits, not changes to transport or product attempt deadlines.

A completed `PackTransferReport` also exposes `advertised_refs()`: the bounded,
syntax-validated ref claims read by that download's own `ls-refs` operation and
used to derive its sorted, deduplicated fetch wants. Claims are withheld if
Git parsing, sink I/O, cancellation, deadlines or HTTP finalization fail, even
when partial payload bytes were accepted. Downstream verification can therefore
use the claims paired with the transferred bytes rather than a separate ref
observation. These remain unverified remote claims: object integrity, required
object completeness, relationships, reachability, freshness/retention and an
atomic remote snapshot still require independent evidence. The real-Git fixture
checks the reported tip set against its owned source and isolated receiver.
