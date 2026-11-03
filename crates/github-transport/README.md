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

`check_sha1_pack_checksum(reader, expected_len, cancellation, deadline)` is a
separate inactive backend stage for already-transferred legacy SHA-1 packs.
Following the [Git pack format](https://git-scm.com/docs/gitformat-pack), it
checks the 12-byte PACK/version/count header, streams the header and compressed
payload through pinned `sha1-checked` 0.10.0, rejects detected collisions, and
compares the trailing 20-byte checksum. It accepts declared versions 2 and 3,
lengths from 32 bytes through 2 GiB, and probes at most one extra byte to refuse
appended data. It uses a fixed 8 KiB buffer, bounds repeated interrupted reads,
and checks cancellation/deadlines around reads and hashing. A blocking reader
still requires an outer worker; no files, network endpoints or objects are
opened by this function.

The returned `PackChecksumReport` contains checksum-match evidence and header
claims only. It does not inflate objects or establish the declared object count,
object integrity, completeness, authenticity, reachability, retention or cleanup
eligibility. A self-consistent malformed object stream can pass this stage and
must still fail independent object verification. Tests cover that boundary,
an independent empty-pack digest vector, fragmented/adversarial readers, and
the real-Git fixture's pack plus checksum corruption and truncation. Independent
strict Git receiver verification remains in place.

### Entry header parsing

`parse_sha1_pack_entry_header` is a finite, allocation-free slice parser for one
SHA-1 pack entry. It returns the kind, declared inflated representation size,
header length, and any delta base. Checked arithmetic, ten-byte integer limits,
and a caller size ceiling no greater than 2 GiB reject malformed/oversized
headers. The parser consumes at most 30 bytes and leaves the compressed tail
untouched. Tests include every two-byte prefix, overflow/truncation cases, the
special offset-delta encoding, and the real Git fixture's first entry size
compared with Git's own object metadata.

For deltas, the size is the inflated instruction stream, not reconstructed object
size. A backward offset is not proof of an entry boundary, and a reference base
may be external. No decompression, base resolution, object identity, object count,
graph completeness or cleanup decision is established by this stage. Callers
must enforce those checks and separate reconstructed-size/aggregate budgets.

### Single-delta reconstruction

`reconstruct_git_delta` applies one already-inflated Git delta to supplied base
bytes. It validates both declared sizes, copy ranges, literal lengths, reserved
opcodes, and exact result length. Each input and the result are limited to
16 MiB; the caller can lower the result ceiling. Allocation is fallible and
occurs only after size validation. At most 1,048,576 instructions are accepted,
with cancellation/deadline checks between instructions and 8 KiB copy chunks.
This implementation limit is experimental and does not define product support
for large objects.

The function neither inflates a pack entry nor resolves or authenticates its
base. Callers still need verified base/type identity, bounded inflation and
chain depth, aggregate memory accounting, object hashing, and graph checks.
No cleanup eligibility follows from successful reconstruction. Tests compare a
fixed mixed copy/insert vector with an independent strict Git import of a
Python zlib/hashlib-generated two-object REF_DELTA pack, and cover malformed
instructions, overflow, truncation, sparse offsets, implicit 64 KiB copies,
cancellation, deadlines, and the instruction ceiling.

### One-stream zlib inflation

`inflate_pack_representation` uses pinned flate2 1.1.10 with its Rust backend to
inflate one zlib stream from an in-memory slice. It requires StreamEnd, including
zlib checksum validation, and an exact declared output length. The returned
compressed byte count identifies the next entry or trailer; concatenated streams
are not implicitly consumed. The compressed input slice and output are each
limited to 16 MiB, with a caller-lowered output ceiling validated before fallible
reservation. These are experimental limits rather than accepted product scope.

Decoder calls use at most 8 KiB input/output chunks, a one-byte overshoot probe,
checked progress counters, a finite call limit, and cooperative cancellation and
deadline checks. The decoder's own internal state allocation remains library
controlled. An outer worker is still required for hard interruption; callers
must account for aggregate memory. Successful inflation does not validate object
type/ID/count, delta bases or chains, graph completeness, or cleanup eligibility.
Tests use independently encoded zlib/raw/gzip/dictionary vectors and compare
inflated real Git objects plus a reconstructed REF_DELTA with strict Git import.

### Canonical SHA-1 object identity

`git_sha1_object_id` hashes one caller-supplied commit, tree, blob or tag using
Git's canonical type, decimal length, NUL and content encoding. It reuses the
pinned collision-checking SHA-1 implementation, refuses detected collisions,
bounds content to 16 MiB, builds its header on the stack, and checks cancellation
and deadlines around 8 KiB hash steps. This is an experimental implementation
limit, not a product object-size commitment.

The ID binds the supplied type and exact bytes. It does not validate commit,
tree or tag syntax, authenticate the supplier, resolve delta chains, verify pack
counts or graph completeness, or authorize cleanup. Callers must compare it with
required identities and perform the remaining checks. Tests cover independent
hash vectors and size/chunk boundaries, plus Git's own IDs for all four kinds
and the reconstructed REF_DELTA result and its base reference.

### Bounded pack decoder

`decode_sha1_pack` composes the checksum, header, inflation, reconstruction and
identity stages for a complete in-memory SHA-1 pack. It returns decoded objects
only after the declared count, stream boundaries and checksum trailer agree.
Each object includes its canonical ID, kind, content, entry offset and selected
delta depth. Offset bases must be exact earlier entry boundaries; reference
bases must match an earlier decoded object's computed ID. Missing, forward and
external bases are explicitly unsupported in this experimental implementation.

Limits are 16 MiB input, 1,024 entries, depth 32, and at most 16 MiB cumulative
decoded work, with a caller-lowered work ceiling. Work includes each inflated
representation and each reconstructed delta result, so discarded instruction
streams still count. This is logical byte accounting, not total process RSS;
input storage, decoder state, object metadata and allocation overhead remain
separate. Controls are cooperative and require an outer worker for hard limits.

Successful decoding still does not validate commit/tree/tag syntax, bind all
required remote tips, prove graph completeness or authenticity, persist evidence,
or authorize cleanup. These limits and backward-only resolution do not establish
product Git support. Tests cover real Git object correspondence, independent
REF/OFS vectors, depth and cumulative limits, unsupported bases, exact object
counts, corruption, truncation and extra data.

### Structural tree links

`parse_sha1_tree` parses one raw SHA-1 tree body into borrowed raw names,
canonical mode classes and nonzero object IDs. It checks exact entry framing,
Git's directory-aware ordering and duplicate basenames, including duplicates
separated by other names. Mode and name searches are bounded; metadata allocation
is fallible. Experimental limits are 16 MiB input, 4,096 entries and 1,024 bytes
per name, with a caller-lowered entry ceiling and cooperative attempt controls.

Names are neither converted into paths nor extracted. Slash, dot, dot-dot and
ASCII-case-insensitive `.git` are refused, but this is not comprehensive platform
path validation or full Git fsck. Gitlink mode is reported as external commit
metadata and never followed; complex-layout product gates remain. Callers must
bind the body to its tree type and object ID, verify referenced object types and
presence, and validate relevant graphs before making any product decision.
Tests cover raw non-UTF-8 names, every canonical mode, malformed/truncated entries,
ordering/duplicate traps, limits, and Git-generated tree correspondence.

### Structural commit links

`parse_sha1_commit` extracts a tree ID and ordered parent IDs from one supplied
SHA-1 commit body. It requires canonical link headers, nonempty author/committer
headers in the supported order, optional-header framing, and a blank separator.
Optional signature and mergetag continuations remain uninterpreted; the message
after the separator is opaque. Duplicate/misplaced core headers, malformed IDs,
NUL/CR header bytes and orphan continuations are refused.

Experimental limits are 16 MiB input, 64 KiB headers, 8,192 bytes per line,
2,048 physical header lines and 1,024 parents; callers may lower the parent cap.
Searches and parent allocation are bounded with cooperative attempt controls.
The parser does not bind object type/hash, validate identity or timestamp syntax,
verify signatures, perform full fsck, or prove linked-object presence or graph
completeness. It may reject historical/noncanonical commits. No cleanup authority
follows from its result. Tests include root and merge objects produced by Git,
ordered parent correspondence, optional continuations, malformed/core-order
cases, limits, interruption and opaque binary message boundaries.

### Structural annotated-tag links

`parse_sha1_tag` extracts a canonical SHA-1 target ID, declared target kind and
borrowed raw tag name from a supported annotated-tag header. It accepts the
object/type/tag order, an optional nonempty tagger line and a blank separator.
Extra headers and continuations are refused. Names use the existing pinned tag
validator; the message and any signature after the separator remain opaque.

Experimental limits are 16 MiB input, 64 KiB header, 8,192-byte lines and a
1,024-byte name, with cooperative attempt checks. This parser does not bind the
tag object's hash/type, verify target presence or type, recursively peel tags,
validate tagger identity/timestamps, verify signatures, perform full fsck, prove
graph completeness, or authorize cleanup. Tests compare all four declared target
kinds with Git mktag and canonical IDs, and cover raw names, tagger-less framing,
opaque signature bytes, malformed headers, truncation, limits and controls.

### Selected-root pack closure

`check_pack_graph_closure` inspects a decoded pack from explicit typed roots. It
requires every reachable commit tree/parent, tree entry and annotated-tag target
to resolve to an object of the expected kind in that pack. Reachable gitlinks
are unsupported. An iterative traversal visits each object ID once, while every
repeated edge still consumes the 16,384-edge limit. Roots are limited to 64;
existing decoder and per-object parser bounds also apply. Index, queue and visit
storage use fallible bounded allocation. Identical duplicate pack entries share
one visit; conflicting entries for an ID are refused.

Only selected-root presence and type closure is established. Unreachable entries
are not syntax-checked. This is not full Git fsck, complete semantic validation,
platform path validation, signature verification, fresh remote-ref binding or
local-work preservation evidence. The caller supplies root types; this stage does
not infer which roots are required. It remains inactive and grants no cleanup
eligibility. Logical work limits do not establish a total process-memory bound;
cooperative controls still require an outer supervised worker.

### Advertised-root decode and closure

`decode_advertised_pack` composes a completed pack-transfer report with decoding
and graph closure of caller-supplied bytes. It requires an exact accepted-byte
count and SHA-256 digest match, resolves every advertised tip, requires commit objects for HEAD and branch
refs, and uses decoded kinds for other namespaces. Symref targets must appear in
the same advertisement with the same ID, and symbolic-reference chains must be
acyclic. A bounded iterative traversal rejects self-loops and longer cycles before
decoding, while accepting shared chain tails. Peeled tag claims must match the terminal
non-tag object reached through typed tag links. Repeated peel claims share a
16,384-step budget. Existing decoder, graph and parser caps remain in force;
there are at most 1,024 advertised refs and 64 distinct roots.

The SHA-256 digest binds supplied bytes to the stream the sink reported accepting.
It does not prove physical persistence or authenticate a remote source. The operation remains inactive
and does not establish an atomic remote snapshot, required local-state coverage,
full fsck, signatures, practical private authentication, production worker/RSS
limits or cleanup eligibility. Strict supported-subset refusals remain incomplete
verification. Tests use bounded private TLS fixtures and the isolated real-Git
fetch/strict receiver fixture; no user repository is accessed.

### Required-object reachability

`AdvertisedPackReport::check_required_objects` checks caller-supplied typed IDs
against the report's privately retained advertised roots. Each requested object
must be present with the expected kind and visited by the bounded root traversal;
an unrelated extra pack entry does not count as coverage. The set must contain
1–1,024 nonzero IDs. Duplicates consume the request count and every supplied kind
is checked. The result reports request and graph counts, with no cleanup flag.

This relation does not discover or attest local repository state. Callers still
need a complete, stable local inventory, local ref-name preservation/disposition,
receipt freshness and revalidation, and treatment of working files, hooks,
configuration, stashes and recoverable state. The earlier persistence,
full-fsck, authentication, worker/RSS and cleanup limits remain. Traversal reuses
existing fallible allocation and cooperative controls without cloning payloads.
Tests distinguish an unreachable packed object from a reachable ancestor and
check all four fixture object kinds through the real-Git private TLS flow.

### Outer supervision of the composed fixture

On Linux/macOS the real-Git private TLS verification flow runs in an exactly
selected ignored child test of the current test executable. The parent clears
the child environment and requires a successful exit, a unique completion marker,
no more than 1,024 stdout bytes and a joined collector within a 30-second fixture
deadline. The marker follows the entire existing reserved-file fetch, decode,
advertised closure, required-object coverage and independent strict receiver
assertions. Four negative child fixtures exercise deadline, output overflow,
missing-marker and unsuccessful-exit handling. Collection uses a fixed buffer
and bounded interruption handling; cleanup precedes evidence assertions.

This is test supervision of that disposable flow, not the production worker or
an OS-level wait/kill latency, descendant-process, memory or crash-cleanup proof.
A forcibly interrupted child may retain its private fixture for reconciliation;
the parent does not infer deletion authority or remove paths from child output.
Private GitHub credentials, stable local inventory, preservation, receipt expiry
and integrated store lifecycle gates remain open. Production app/renderer and
worker-probe interfaces are unchanged.

### Accepted-byte stream binding

The transfer report exposes `accepted_pack_sha256` only after the pack operation
and HTTP finalization both succeed. The constant-memory digest observes exactly
the prefix each sink write reports accepting, including short writes; interrupted
or failed calls contribute no bytes. Partial failures retain accepted byte counts
but expose no digest. This uses pinned `sha2` 0.10.9, already present in the lockfile.

Before decoding, the advertised-pack operation checks length and compares this
digest against the supplied bytes in cooperative 8 KiB chunks. The existing
16 MiB experimental input cap is checked before hashing. A valid same-size pack
with the same roots and different unrelated contents is rejected. Digest equality
binds content under SHA-256; it does not validate a sink's persistence claims,
replace native file identity/revalidation, authenticate a server or authorize
cleanup. Tests include an independent Python hashlib vector, short/interrupted
writes, withheld failure digests and same-size valid-pack substitution.

### Identity-bound private-file readback fixture

The Linux/macOS private pack-file fixture now composes sync and allocation
observation, native reservation/root/file identity checks, bounded descriptor
readback, accepted-stream digest matching and decoding. It compares length,
identity, permissions, link count and modification/change timestamps around
readback and decode. A one-byte EOF probe rejects growth beyond the sampled
length; allocation is fallible and interrupted reads are bounded. Raw bounded
contents remain inspectable after a partial attempt is canceled.

Tests change only task-owned files: growth, truncation, equal-length timestamp
change, content tampering, hard links, symlink replacement, malformed read counts,
read interruptions and deadline refusal. The supervised real-Git flow uses the
composed operation before required-object and independent strict receiver checks.
This remains test-only and detects observable changes, not an atomic snapshot or
all races. Later mutation still requires revalidation. Sync success is not a
power-loss guarantee; production worker, crash/reconciliation, persistence and
cleanup acceptance remain open.

### Repository context and in-process expiry

Pack transfer and decoded reports retain the exact validated repository URL used
to construct the transport and its original monotonic and wall-clock start times.
`decode_advertised_pack` and `check_required_objects` require an explicit expected
repository; both check context before and after their work. URL aliases, case or
`.git` suffix differences are refused rather than normalized. Cloning or decoding
an old report does not renew its lifetime.

Context checks refuse at 15 minutes on either clock, invalid/backward readings,
or a repository mismatch. An observed expiry or invalid-clock result is terminal
for that attempt and shared by existing and future clones; a later clock correction
cannot revive it. The first terminal clock failure is retained under concurrent
checks. A wrong expected repository is a per-call mismatch and does not invalidate
the correctly bound attempt. `is_complete_transfer` still describes historical
transfer completion; it does not mean evidence is currently usable. Counts-only
coverage results likewise do not replace a fresh context check at later use.
The original attempt begins at transport construction, conservatively before ref
observation, rather than at decoding completion.

This is an in-process guard, not a serialized receipt or a full platform-clock
acceptance result. Rust does not guarantee consistent suspend accounting for
`Instant`; the wall-clock guard adds conservative elapsed-time checking but is
not a clock-tampering guarantee. Persisted receipts, local manifest revalidation,
ref-name preservation, authentication, remote retention, worker/store lifecycle
and cleanup eligibility remain separate gates. Tests exercise clock boundaries
without sleeps and require the expected endpoint through the private TLS flow.

### Required ref names and manifest identity

`RequiredRefManifest` captures caller-supplied raw ref names, typed IDs, optional symbolic targets and a
canonical SHA-256 digest. It preserves distinct names pointing to the same ID,
rejects duplicate names and validates the supported Git name subset. Names are
copied with fallible allocation, sorted byte-for-byte and never converted into
paths or commands. Bounds are 1–1,024 entries, 1,024 bytes per name and 64 KiB of
combined name and symbolic-target bytes in total. Symbolic targets use the same
1,024-byte name cap, must be full `refs/` names, and cannot point to themselves.
`None` explicitly claims a direct ref. The digest uses the v2 domain and encodes
target presence and length. `check_unchanged` compares the full canonical entry sequence;
changed names, targets, kinds or membership require renewed verification.

`check_required_refs` requires a current expected-repository context and the same
transfer attempt as the decoded report. It checks typed object reachability and
returns manifest-backed entries whose names are absent remotely or whose remote
names point elsewhere, including a separate symbolic-target mismatch. A local branch can need preservation even when its commit
is covered by another remote branch. Matching remote names do not promise future
retention. The report borrows the immutable manifest and exposes no cleanup flag.

The manifest is a caller claim, not discovery of all local refs or complete HEAD state, working files, configuration, stashes or recoverable objects. It is
not a backup or an explicit discard decision, and it does not replace native
source revalidation or a persisted receipt. Existing clock, worker/store,
authentication, platform and cleanup gates remain. Tests include an independent
Python hashlib digest, canonical ordering and raw names, limits, changed claims,
same-attempt binding and an actual Git local alias whose history is covered but
whose name was not advertised.

Named coverage also retains its original context for `recheck_manifest`. This
compares a later caller manifest with the immutable verified claims under the
same repository/expiry checks. Once that named result observes changed claims,
it stays stale even if an older manifest is later supplied; new analysis and
evidence are required. Repository mismatch, cancellation and deadline errors
refuse the call without falsely recording a source change. Getters remain
historical evidence, not a current-validity flag. The real-Git fixture recaptures
native refs, accepts an unchanged observation, then rejects a name-only rename
that leaves the covered object IDs unchanged. These observations are not atomic
and do not establish a comprehensive user-repository inventory.

The disposable Git fixture also captures HEAD explicitly and detects switching its
symbolic branch or detaching it at the same commit ID. This does not prove native
capture completeness, atomicity, unborn-HEAD handling or absence of symref cycles.

Manifest capture also checks links whose targets are present in the same snapshot.
Their object IDs and kinds must agree, and a bounded walk rejects cycles. Missing
targets remain external caller claims. This uses fixed state/path arrays (9 KiB on 64-bit targets) without heap graph
allocation. Each of at most 1,024 entries and present links is processed once,
with binary target lookup and cancellation/deadline checks during traversal
and completion.
It cannot establish consistency of refs omitted by the caller or native atomicity.

The closed private pack-file fixture reconciles the completed transfer's
cumulative raw HTTP ingress into the same `BudgetController` job that charges
file allocation, before checking cancellation or decoding. Repeating a total
charges no duplicate bytes. An incomplete transfer still contributes observed
ingress; a failed/non-running job cannot yield decoded evidence even if late
accounting itself is accepted. Counter regressions cancel the fixture without
reducing prior usage. Tests retain the durable reservation and require
reconciliation on reopen after refusal, including a synthetic over-limit counter.
Transport-bound fixture constructors also sample that attempt's cumulative raw
ingress before each payload write and require the same attempt context before
accounting a final report. Same-byte reports from another attempt are refused
without charging their counter or cancelling that other attempt. The counter
mutex is released before controller accounting or filesystem work.

These are write checkpoints and completion-time accounting in a task-owned
fixture, not a per-read controller callback, durable usage receipt, production
worker/store integration or multi-gigabyte network enforcement demonstration.
Unbound constructors remain for file-only tests; this is not a public
report-adoption API.

`GitHubTransport::ingress_observer()` returns a cloneable, read-only
`TransportIngressObserver` for that attempt. `observed_bytes()` reads cumulative
raw socket bytes, including after cancellation or transport consumption;
`matches_transfer()` compares historical attempt identity with a pack report.
The observer retains only the shared counter and context, without the full
transport budget, credentials or mutation controls. The private file fixture
now uses this interface instead of accessing budget/context internals.

These are observations, not transfer completion, evidence freshness, resource
admission or cleanup approval. Counter snapshots may wait on the in-flight read
mutex; they are not nonblocking and provide no OS latency guarantee. Poisoned
locks return a sanitized error. This backend API adds no renderer command,
network execution, filesystem access, worker handoff or persisted receipt.

For supervisor sampling, `try_observed_bytes()` uses a single nonblocking mutex
attempt: `Some(total)` is an observation, `None` means busy, and a poisoned lock
is an error. Busy is neither zero usage nor completion. The method does not
retry, change counters, or suppress observations after cancellation. It provides
no operating-system scheduling guarantee; callers still own attempt deadlines
and final accounting.
