# Git HTTPS worker probe contract (0.0.4)

Status: bounded feasibility prototype, 1 November 2026. This contract records
what the standalone `probes/git-https-worker` implements and what remains
unproven. It does not certify the Git verification adapter or close M0.

## Implemented path

- The probe validates only `https://github.com/OWNER/REPO[.git]` URLs. It
  rejects credentials, query/fragment data, escapes, other hosts, and nested
  paths before creating a worker.
- The syntactic allowlist accepts both `https://github.com/OWNER/REPO` and
  `https://github.com/OWNER/REPO.git`; the exact caller URL is used and never
  canonicalized. In-memory configuration disables redirects. An extensionless
  URL that GitHub redirects to `.git` therefore fails closed at fetch. A root
  run of the extensionless form failed with zero response bytes; a later
  canonical `.git` run completed successfully as recorded below. That suggests
  a blocked redirect, but the earlier run predates stage diagnostics and did
  not establish the cause.
- The public manual network entry point is `--bounded-probe URL`, fixed at a
  32 MiB logical temporary-store limit and a 60-second deadline for a tiny
  public test repository. A root-reviewed run against public octocat/Hello-World
  through the wrapped transport and child watchdog completed in 738 ms, with
  16,387 logical bytes in the owned store,
  failure=None, and cleaned=true; repository metadata recorded a size of about
  1 KiB. This is one tiny public fetch observation, not an M0 scope pass. It
  preceded the receipt and private-credential additions in this batch. A parent
  later observed a sanitized `WorkerFailed` without a receipt. Two authorized
  reruns of the same public URL with the current receipt path succeeded in 922
  ms and 769 ms; each reported 16,387 logical store bytes, 2,191 response-body
  bytes exposed, 1,303 header bytes exposed, 13 objects, 4 refs, and cleaned
  the owned store. The failure was not reproducible in those runs, so its
  transient cause remains unknown. The later root-reviewed version-2 run of
  `https://github.com/octocat/Hello-World.git` completed in 712 ms with 16,387
  logical store bytes, 2,191 body bytes, 1,303 header bytes, 13 fetched
  objects, 4 refs, 13 reachable objects, 1,991 decoded bytes, zero Gitlinks,
  and cleaned=true. This is the first recorded public fetch with the closure
  validator active. A parent
  creates a private temporary directory with
  mode 0700, an exclusive random ownership receipt, and an empty isolated Git
  config. The child checks the receipt, then initializes a separate bare
  repository and calls gix `PrepareFetch::fetch_only`; no checkout occurs and
  the source clone is never opened.
- Public mode installs a credentials callback that always returns `None`. A
  separate `--private-probe URL` path reads one newline-terminated line with a
  maximum 512-byte printable ASCII secret from non-terminal stdin, rejects
  empty, oversized, whitespace, and
  non-ASCII input, and passes it to the child only on child stdin. URL arguments
  remain restricted to the fixed GitHub HTTPS host/path allowlist; extra argv
  values are rejected. The secret is never accepted through argv or env and is
  not included in the URL, Git config, worker receipt, normal output, or errors.
  Both the parent entry point and direct private-worker mode reject terminal
  stdin before reading any secret. The child validates the URL and starts its
  watchdog before reading stdin. It
  supplies a fixed `x-access-token` username plus the secret as password only
  for an exact match with the validated remote URL. The callback issues a
  credential once, and ignores Store/Erase actions so gix cannot persist it or
  request a second copy. Git helpers and terminal prompting remain disabled.
- Parent and child input buffers use zeroize's `Zeroizing` wrappers. This is
  best-effort for buffers owned by this prototype. gix creates its own account
  and Basic authorization header copies, and reqwest/TLS may retain copies;
  those library and operating-system copies are not proven to be zeroized.
- After successful fetch the child writes one JSON receipt to its stdout pipe.
  The parent reads at most 4,097 bytes, rejects over-4-KiB or invalid JSON, and
  strictly validates a deny-unknown-fields schema before temporary-store
  cleanup. The version-2 receipt contains schema version, bare-fetch-complete flag,
  exposed response body/header byte counts, fetched object/ref counts, reachable
  object count and decoded byte count, and excluded Gitlink count only.
  It has no URL, branch name, private path, or credential field. The parent
  prints only these safe counts. A missing or malformed receipt fails closed
  and still follows owned-store cleanup.
- The parent clears the child's inherited environment, sets `HOME` and `TMPDIR`
  to the owned store, empties `PATH`, and disables system/global Git config and
  terminal prompts. It also applies in-memory gix overrides disabling
  credential helpers, redirects, proxy config, and unverified TLS. A no-secret
  credential callback remains installed. Thus parent API keys and proxy
  credentials are not inherited, and this probe intentionally does not use a
  user's proxy configuration.
- The parent polls its child every 20 ms and sums logical file lengths in the
  owned store. It checks the time and byte limits during execution and again
  after observing child exit, then kills and waits for the worker on a limit
  violation. It also checks available filesystem bytes before creating the
  store, immediately after creation, during the child run, and after child
  exit. Safe rustix statvfs calls fail closed if observation fails or space is
  below a 2 GiB reserve plus 64 MiB headroom. A low-space observation kills and
  joins the child before cleanup. Cleanup requires the same device/inode
  directory identity plus
  the exclusive random receipt. A replaced or symlinked root is retained, and
  the attempt fails closed. Errors are collapsed to fixed codes so transport
  errors cannot echo a URL or credential.
- A gix-transport 0.55.1 HTTP decorator is injected through gix 0.81.0's
  PrepareFetch connection callback. It wraps the concrete blocking reqwest
  HTTPS transport and forwards configuration so the in-memory redirect
  disable remains active. One attempt shares a 32 MiB response-body budget
  across GET and POST responses and a separate 64 KiB response-header budget.
  It reserves byte counts before returning body/header slices to gix and uses
  one absolute deadline for both reader types. At exact exhaustion it checks
  the underlying reader for EOF and accepts EOF without exposing additional
  bytes; any available excess remains hidden and raises a cap error. This EOF
  check can call a buffered reader that has already prefetched data internally,
  so it is an exposure boundary to gix, not a one-byte wire probe.
- After fetching, the worker traverses every advertised ref root in gix's fetch
  ref map. It follows annotated tag targets, commit trees and parents, tree
  entries/subtrees, and blobs. It checks each object header against per-object
  and cumulative decoded-byte budgets before loading contents, parses each
  typed object with gix's object parsers, recomputes the Git object hash, and
  compares it with the referenced ID. Missing, malformed, wrong-kind,
  hash-mismatched, or over-budget objects fail closed before a success receipt.
  Gitlinks are counted and excluded because their targets belong to separate
  submodule repositories; they are not fetched or claimed as part of closure.
  Fixed limits are 100,000 unique objects, 64 MiB cumulative decoded object
  bytes, 16 MiB per object, 250,000 work units, and 100,000 pending object IDs.
  The worker's overall deadline remains active during traversal. gix header
  lookup can resolve packed delta chains; any internal allocations in that
  resolution are outside the decoded-byte budget and have not been measured.
  These are post-fetch closure bounds: gix unpacks the incoming pack before
  this check, so they do not establish a memory cap during pack ingestion.
- The internal worker starts its own read-only watchdog before gix repository
  initialization. It independently enforces the same 60-second deadline,
  logical store cap, and free-space reserve every 20 ms, even if invoked
  directly without the parent. It requires an absolute private directory, a
  regular private matching ownership receipt, and an absent objects.git path; it
  refuses symlink or existing object-store paths. On limit violation it exits
  with a fixed code and never cleans the directory. Only the parent removes a
  registered temporary store. The marker protects against accidental path
  selection, not arbitrary same-user code that can forge it; the direct worker
  applies the same limits regardless.
- Before a receipt is written, the worker uses fixed exit codes that the parent
  maps to sanitized stages: 74 is the watchdog, 75 is fetch failure or blocked
  redirect, 77 is receipt failure, and 80-84 identify closure missing,
  malformed, wrong-kind, hash-mismatch, and budget failure. No transport error
  text, URL, or child stderr is exposed. The codes distinguish the stage while
  collapsing library errors within that stage.
- Forty-three local tests cover endpoint rejection, receipt-based store cleanup,
  child deadline termination/join, a successful exit without a receipt failing
  closed, an exited child that has already exceeded
  the size cap, an exited child past its deadline, temporary-store budget
  termination, direct unparented worker deadline and low-space exit with caller
  sentinel retention, environment isolation, cleanup-failure handling, and
  replacement-symlink protection with an unrelated sentinel. They use no Git
  operation or network connection for the transport, byte-budget, watchdog,
  credential-canary, receipt, and disk-space tests. They test secret input
  limits, EOF/closed-pipe handling, and direct private-worker terminal-input
  rejection, typed object-graph traversal with parent commits, nested trees and
  Gitlink exclusion, hash and malformed/missing/type failures, a duplicate-ID
  wrong-kind edge after the ID was already visited, and object,
  decoded-byte, work, and pending-ID budget failures, a local gix object-store
  adapter fixture, fixed worker-exit stage mapping, extensionless URL syntax
  acceptance without canonicalization, callback URL matching and
  single issue,
  Store/Erase no-op behavior, child-pipe failure cleanup, and absence of a
  synthetic canary (plaintext and both Basic/base64 encodings) from child
  stdout, stderr, and store files. A callback-level Erase simulation checks
  that a rejected secret is not issued a second time; no local HTTPS 401 server
  or authenticated fetch has been exercised. The 100 ms deadline fixture kills and joins
  its child; the fast-exit fixture is rejected when its 256 KiB write exceeds
  the 64 KiB test cap before supervision observes exit. Clippy with warnings
  denied passes.

## Selected API and documentation evidence

The prototype uses gix 0.81.0's official `prepare_clone_bare` and
`clone::PrepareFetch::fetch_only` APIs with the `blocking-http-transport-reqwest-rust-tls`
feature. `fetch_only` performs a fetch into the prepared bare repository and
does not check out a worktree. The connection builder documents that its
default behavior may invoke configured Git credential helpers; `set_credentials`
replaces that behavior with the explicit public no-credential callback or
private single-issue callback. gix's HTTP
option construction reads `http.followRedirects`; the in-memory override sets
it to `false`. The reqwest backend stops redirects when this option disables
them and independently stops a redirect to another host. References:

- [gix 0.81 `prepare_clone_bare` source](https://docs.rs/gix/0.81.0/src/gix/lib.rs.html)
- [gix 0.81 `PrepareFetch::fetch_only` source](https://docs.rs/gix/0.81.0/src/gix/clone/fetch/mod.rs.html)
- [gix 0.81 `Connection::set_credentials` documentation](https://docs.rs/gix/0.81.0/gix/remote/struct.Connection.html)
- [gix 0.81 credential callback source](https://docs.rs/gix/0.81.0/src/gix/remote/connection/access.rs.html)
- [gix 0.81 HTTP option construction](https://docs.rs/gix/0.81.0/src/gix/repository/config/transport.rs.html)
- [gix-transport 0.55.1 reqwest redirect policy](https://docs.rs/crate/gix-transport/0.55.1/source/src/client/blocking_io/http/reqwest/remote.rs)
- [gix-transport 0.55.1 HTTP trait](https://docs.rs/crate/gix-transport/0.55.1/source/src/client/blocking_io/http/traits.rs)
- [rustix 1.1.6 statvfs API](https://docs.rs/rustix/1.1.6/rustix/fs/fn.statvfs.html)
- [zeroize `Zeroizing` documentation](https://docs.rs/zeroize/1.9.1/zeroize/struct.Zeroizing.html)
- [gix 0.81 HTTP transport feature definitions](https://docs.rs/crate/gix/0.81.0/source/Cargo.toml)
- [gix 0.81 object lookup and header APIs](https://docs.rs/gix/0.81.0/src/gix/repository/object.rs.html)
- [gix-object 0.58 typed object parsing and hash API](https://docs.rs/gix-object/0.58.0/gix_object/)
- [gix 0.81 repository initialization API](https://docs.rs/gix/0.81.0/src/gix/lib.rs.html)

The existing gix 0.60 HTTP trait probe is not source-compatible with gix
0.81.0's gix-transport 0.55.1 trait, so the accounting wrapper was ported to
the pinned trait. The Python HTTPS probe reads smart-HTTP advertisements only.
This prototype connects the wrapper to the concrete fetch-to-bare-store path.

## Limits and remaining M0 gates

- The 32 MiB logical-store ceiling is sampled every 20 ms and can be exceeded
  between samples; it is not a pre-write disk cap or allocation accounting.
  Separately, the HTTP wrapper bounds response body and header bytes exposed to
  gix. Inspection of the pinned reqwest backend shows response bytes copied
  through a zero-capacity rendezvous pipe using std::io::copy, which uses a copy
  buffer, and headers emitted through a similar pipe. The wrapper counts slices
  exposed by that pipe to gix; it does not measure socket reads, TLS records,
  reqwest/hyper buffers, or kernel buffering. The exact-cap EOF check may call
  the backend reader to fill its own buffer to distinguish EOF from excess, but
  no additional byte is exposed to gix. No strict network-ingress cap is
  claimed. Synthetic transport tests prove shared accounting at the wrapper
  seam, but not a live server's exact ingress or blocking-read overshoot. The
  MVP-CATALOG's proposed 2 GiB and five-minute defaults remain unproven and
  separate from this small probe mode.
- The 60-second limit is enforced both by the parent's kill-and-wait
  supervision and the worker-local watchdog. The watchdog exits the blocked
  worker process if DNS, connection, or body reads do not return. Polling and
  scheduler latency can cause bounded deadline overshoot. Tests exercise both
  parent termination/join and direct worker-local exits; they do not measure a
  live network timeout's overshoot.
- The store uses the system temp directory and checks a 2 GiB reserve plus
  64 MiB headroom at preflight and each supervisor poll. This is an observed
  free-space guard, not an atomic reservation; another process can consume
  space between observations. It has no durable journal, abandoned-store
  reconciliation, aggregate-store budget, or allocation-based accounting.
  The identity-bound receipt is process-lifetime probe evidence, not a durable
  production receipt protocol.
- The closure validator covers advertised ref roots returned by this gix fetch
  and their reachable commit/tree/blob/tag graph, while counting and excluding
  Gitlink targets. It does not prove preservation of pre-existing local refs,
  LFS payloads, worktree material, external alternates, or submodule object
  graphs. Synthetic object tests cover traversal and limits, but the public and
  private live observations above predate this validator. A reviewed network
  run with the validator in place remains outstanding.
  Pack ingestion itself still relies on gix's bounded transfer body/store and
  worker deadline; this probe has no hard cap for memory used while gix receives
  and unpacks a malicious compressed pack. The post-fetch closure budget cannot
  prevent that earlier allocation.
- One bounded private GitHub HTTPS fetch with the existing host sign-in
  succeeded, as recorded below. App-owned Keychain credential integration,
  identity, signing, and revocation remain unproven. Zeroizing covers the
  prototype-owned input buffers, not Gix's internal
  Account/header copies, Reqwest/TLS buffers, allocator remnants, or OS memory.
- The gix and gix-transport backends are pinned in the probe manifest. Build,
  forty-three local tests and clippy pass after fetching the pinned Rust
  dependencies. One reviewed public tiny-repository object fetch is observed
  above; it validates the bounded transport/worker integration and object-store
  writing before receipt and private-handoff additions, but not authenticated
  private HTTPS, a larger repository, hard network ingress, or complete M0
  scope.

Conclusion: a pure Rust path from supported GitHub HTTPS transport through a
byte-accounting wrapper and killable worker into a probe-owned bare object
store is compiled with gix, and one tiny public fetch completed. The wrapper
caps bytes exposed to gix; transport/TLS/kernel ingress remains unmeasured.
Private credentials and complete M0 scope evidence remain open. Keep D8
blocked pending those proofs.

## Coordinator private-path observation

A bounded fetch of this app's own small private repository succeeded using the existing host GitHub sign-in handed through memory and stdin. The receipt reported 493 objects, six refs, 980,162 body bytes exposed, 1,303 header bytes exposed and 1,000,028 logical temporary-store bytes, in 1,349 ms. Owned-store cleanup succeeded and captured output contained no credential echo. A deliberately invalid synthetic credential failed closed in 434 ms with owned cleanup. Sanitized observations are in `probes/git-https-worker/evidence/2026-11-01-live-checks.json`. No credential was persisted, changed or revoked by these checks. This does not demonstrate app OS credential-store integration. These runs predate object-closure validation; complete closure with the new code, local-only preservation analysis and production verification budgets remain open.

A coordinator run of the schema-v2 closure path against the app's own private repository succeeded in1,662ms, with552 fetched/reachable objects, six refs,4,408,809 decoded reachable bytes,1,032,118 body bytes exposed,1,303 header bytes exposed and1,053,626 logical temporary-store bytes. Cleanup succeeded and captured output contained no credential echo. It reused the existing host sign-in through memory/stdin; it did not create or change credentials. Sanitized evidence is in `probes/git-https-worker/evidence/2026-11-01-live-closure.json`. This is not app Keychain or local-clone preservation proof.
