# Git HTTPS worker feasibility probe (0.0.4)

Standalone Rust prototype. It fetches advertised refs and reachable objects
from a GitHub HTTPS repository into a fresh bare object store using gix, with
the transfer running in a separately killable child process. It never opens or
changes a source clone and never checks out project files. Run `cargo run -- --bounded-probe
https://github.com/OWNER/REPO.git` to start a public, unauthenticated attempt.
This manual mode is fixed at 32 MiB logical store size and 60 seconds. It is
intended for a tiny public repository after review. A root-reviewed run through
the wrapped transport and worker watchdog against public `octocat/Hello-World`
completed in 738 ms and left 16,387 logical bytes in the owned store before
cleanup. This is reachability evidence for one tiny public fetch, not an M0
scope pass or a network-ingress cap proof. A later root-reviewed version-2
receipt run completed in 712 ms with 16,387 logical bytes, 2,191 body bytes,
1,303 header bytes, 13 fetched and reachable objects, 4 refs, 1,991 reachable
decoded bytes, zero Gitlinks, and successful cleanup. This is the first recorded
public fetch through the closure validator, but does not close M0.

The allowlist accepts only `https://github.com/OWNER/REPO[.git]`. Both spellings
are passed through unchanged; redirects are disabled. If GitHub redirects an
extensionless URL to its `.git` endpoint, that request will fail closed. Tokens in
URLs and credentials from Git helpers are unsupported. The public mode always
returns no credential. The private prototype accepts one newline-terminated
line containing a maximum 512-byte printable ASCII secret from non-terminal
stdin and passes it to the worker only through a pipe. Git helpers and terminal
prompts remain disabled. One authorized private fetch using the existing host
sign-in completed with 493 objects, six refs, 980,162 response-body bytes,
1,303 header bytes, and 1,000,028 logical store bytes in 1.349 seconds; cleanup
succeeded and no credential echo was detected. A synthetic invalid credential
failed closed and cleaned up. These observations do not establish app Keychain
integration or signed permission validation. Sanitized details are in
[`evidence/2026-11-01-live-checks.json`](evidence/2026-11-01-live-checks.json).
Those live fetches predate the closure validator below; a bounded network run
with this version's closure check remains for review.

After fetch, the worker traverses every advertised ref's reachable object graph
through tags, commits and parents, trees, and blobs. It verifies each object's
Git hash and typed links before emitting the receipt. Gitlink/submodule targets
are counted and explicitly excluded. Closure limits are 100,000 unique objects,
64 MiB decoded total, 16 MiB per object, 250,000 traversal work units, and
100,000 pending object IDs. Exceeding a limit or finding a missing, malformed,
wrong-kind, or hash-mismatched object fails closed.
These limits bound post-fetch graph traversal; gix unpacks the incoming pack
before this check, so they do not establish a memory cap during pack ingestion.
gix header lookup can resolve packed delta chains; internal allocations in
that resolution are not included in the decoded-byte budget.

Run the isolated process-supervision fixtures with:

```sh
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

The parent polls the child every 20 ms, checks limits again after child exit,
kills and joins on timeout, store growth, or low free space, then removes only
its marked private temp directory after checking its original filesystem
identity. A gix HTTP transport decorator limits shared GET/POST response-body
and response-header bytes exposed to gix and checks one attempt deadline. Its
tests exercise process kill/join, byte boundaries, concurrent readers,
deadline behavior, free-space failures, environment isolation, and safe
cleanup with synthetic fixtures. The internal worker also runs its own
read-only watchdog, so direct invocation cannot bypass the same time, logical
store-size, and free-space checks. It never removes caller-supplied paths; only
the parent removes a registered temporary store. The bounded public and private
fetches used real repositories; the synthetic tests exercise receipt
validation, object-closure parsing, and budgets without network access. The
decorator's accounting has no live private-server validation beyond the
sanitized observations above.
See the detailed boundary analysis in
[`docs/contracts/git-https-worker-probe.md`](../../docs/contracts/git-https-worker-probe.md).
