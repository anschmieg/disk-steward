# gix bounded-reader seam probe (0.0.3)

Standalone Cargo workspace for an offline feasibility experiment. It implements
the real `gix_transport::client::blocking_io::Transport` trait and routes a
synthetic in-memory packet stream through `gix_packetline::StreamingPeekableIter`
with `BoundedRead` placed directly underneath gix's packet decoder. The strict
wrapper errors when another read would cross its configured byte ceiling and
never returns or reads a lookahead byte past that ceiling. Tests use only the
fixture bytes `0009\x01PACK0000`; no Git executable, concrete HTTP client
backend, URL connection, credential, or source clone is present.

Run from this directory:

```sh
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

## Exact API selection

The pinned implementation source was checked in the official
[`gix-transport` 0.60.0 HTTP trait source](https://docs.rs/crate/gix-transport/0.60.0/source/src/client/blocking_io/http/traits.rs)
and its [generic HTTP transport](https://docs.rs/crate/gix-transport/0.60.0/source/src/client/blocking_io/http/mod.rs).
That trait makes response body type an associated `BufRead` type, so this probe
decorates the body at that seam and delegates request, status, and redirect
policy to its backend.

- Crate package version: `0.0.3`.
- `gix-transport = 0.60.0`, pinned exactly, with `default-features = false`
  and features `blocking-client` plus `http-client`. This exposes gix's
  transport-independent HTTP backend trait; neither curl nor reqwest is enabled.
- `gix-error = 0.4.0` and `gix-url = 0.39.0`, both pinned exactly.
- `gix_packetline` is used through the public `gix_transport::packetline`
  re-export (lockfile resolves `0.23.0`).
- `TransportWithoutIO` supplies the destination URL/configuration seam;
  `Transport` supplies handshake and request. `RequestWriter::new_from_bufread`
  receives an `ExtendedBufRead` backed by `StreamingPeekableIter<BoundedRead<_>>`.
- `gix_transport::client::blocking_io::http::Http` is decorated by
  `BudgetedHttp<H>`. Its associated `ResponseBody` is changed to
  `BoundedBody<H::ResponseBody>`, which shares one `ByteBudget` across each
  `get()` and `post()`. The test fixture installs this decorator into gix's
  actual generic HTTP `Transport<H>` and runs its upload-pack handshake plus
  response request. The budget counts bytes when its `BufRead::fill_buf()`
  exposes them, so no over-limit slice is returned to gix.

## What this proves and does not prove

The compiled fixture proves a single body-byte budget can span the gix HTTP
`GET info/refs` and `POST git-upload-pack` response bodies, and that the gix
HTTP transport sees a failed response if the next body byte would cross that
budget. The reservation is atomic across concurrently read response bodies;
the synchronized two-body test proves their combined exposed bytes cannot
exceed the shared ceiling. In-memory fixture cases also keep a supplied 302,
401 authentication-required, server errors, and incorrect smart-HTTP content
types as failures. This proves only that `FixtureHttp` rejects those fixture
statuses. `BudgetedHttp` forwards the inner backend's redirect state and does
not itself prevent an arbitrary backend from following redirects or consulting
credentials.

The count is **not** whole-wire accounting: it excludes TLS records, HTTP
headers, and any buffering/prefetch below the `Http::ResponseBody` seam. The
fixture's `Cursor` has no read-ahead, but a concrete HTTP backend may have
socket, TLS, decompression, or body buffers that fetch more bytes before this
wrapper sees them. The strict ceiling applies to bytes exposed to gix; an inner
`BufRead::fill_buf()` may prefetch bytes before the budget reservation, so this
wrapper does not bound that lower-layer read-ahead. At the exact ceiling, a
non-empty subsequent read fails conservatively; an empty EOF read remains EOF.
The test-only `loopback_http_tests` module also runs the actual gix HTTP
transport against an ephemeral `127.0.0.1` TCP fixture. It counts each socket
byte read, including status/header bytes and bodies across both requests, caps
before the next socket read, enforces one monotonic attempt deadline across
partial reads, rejects redirects without contacting the `Location` listener,
and rejects malformed framing, unsupported encodings, 401, and server errors.
This is a local socket seam proof; it is not TLS/wire accounting and it does
not yet demonstrate the 2 GiB wire-ingress budget,
five-minute cancellation deadline, Keychain credential behavior, pack/store
growth, or any live remote operation. The deadline test covers response
reads; production DNS and full connection setup still need worker-level
cancellation in the selected backend.

The next proof must wrap the selected production HTTP backend below any
socket/TLS/decompression read-ahead buffer or measure bounded cancellation
overshoot explicitly, using a local adversarial HTTP fixture. A worker-level
deadline is still needed for DNS, connection establishment, and reads.

The test-only `outer_worker_deadline_kills_blocked_gix_handshake` runs the exact
current test executable with one fixed ignored helper selected. That helper
owns a loopback server, confirms the actual gix upload-pack GET, and withholds
its response. Its socket-read and transport deadlines are 30 seconds; the
parent's two-second attempt deadline includes process startup. The parent
receives readiness through bounded output, kills and waits for the child,
and joins the output reader before asserting the result. The helper is ignored
in the ordinary test list because it must run under its parent supervisor.
An early handshake return fails rather than being hidden by a server join.
This proves outer termination of this blocked local gix read. It does not
integrate the separate store worker, exercise DNS/TLS or credentials, establish
an OS kill/wait latency guarantee, or complete the production transport gate.

Local credential-injection fixtures exercise gix's `set_identity` directly:
GET and POST must carry exactly one expected Authorization header for an
explicit synthetic account, anonymous requests must carry none, and a 401 must
remain a failed handshake. Assertions expose only boolean observations, not
account/header contents. A separate test requires gix to refuse credentials
on an `http://` logical URL before invoking its backend. The authenticated
fixture uses an HTTPS-labelled logical URL routed by the test backend to plain
IPv4 loopback; this tests gix header construction and policy, not TLS. No
credential helper, environment credential, Keychain item or real token is
consulted by these fixtures. This does not prove production credential broker
integration, secure memory lifetime/zeroization, private GitHub access or
revocation behavior.

The private `tls_ingress_tests` module places a socket reader beneath
`rustls::StreamOwned` and shares its ingress budget across the actual gix GET
and POST fixture connections. Generated localhost certificates are explicitly
trusted for the success fixture; the negative cases use an empty trust store
or a mismatched hostname. Certificate verification is not disabled. A small
cap interrupts encrypted handshake traffic before another socket byte can be
returned, and a non-responsive local TLS peer exercises the attempt deadline.
The local adapter ignores caller URLs in favor of its fixed loopback endpoint
and is not a production HTTP client. It does not implement remote DNS,
credentials, general request bodies, or store writes.

This count includes encrypted TLS records returned from socket reads, including
handshake traffic and HTTP headers. It excludes bytes buffered in the operating
system or arriving at the network interface before the program reads them. The
small fixture limits do not calibrate the 2 GiB production budget or establish
a five-minute production timeout. Production backend selection, worker
composition, private GitHub/Keychain authentication and full M0 remain open.
The fixture accepts POST request data into an in-memory sink, as the earlier
HTTP fixture does; it proves incoming response accounting, not upload-pack
request-body transmission or a complete fetch protocol implementation.
