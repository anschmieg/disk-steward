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
scope pass or a network-ingress cap proof.

The allowlist accepts only `https://github.com/OWNER/REPO[.git]`. Tokens in
URLs and credentials from Git helpers are unsupported. The public mode always
returns no credential. The private prototype accepts one newline-terminated
line containing a maximum 512-byte printable ASCII secret from non-terminal
stdin and passes it to the worker only through a pipe. Git helpers and terminal
prompts remain disabled. The private
handoff has synthetic tests but no actual credential or private fetch evidence.

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
the parent removes a registered temporary store. Only the earlier reviewed
run used a Git repository; the private credential path and secret-free receipt
have only synthetic validation. The decorator's accounting has no live
private-server validation here.
See the detailed boundary analysis in
[`docs/contracts/git-https-worker-probe.md`](../../docs/contracts/git-https-worker-probe.md).
