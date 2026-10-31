# M0 Git transport decision (provisional)

**Reviewed:** 2026-10-31
**Scope:** GitHub HTTPS read transport and feasibility of the M0 per-attempt time and ingress bounds. This is a source review, not a transport validation or a completed M0 gate.

## Provisional selection: evaluate `gix` first

The application core is Rust, and gix exposes an HTTP transport configuration seam plus a remote connection API that allows a caller to configure the selected transport or provide a transport instance. Its documented HTTP options include `gitoxide.http.connectTimeout`, `http.lowSpeedTime`, `http.lowSpeedLimit`, and redirect policy. This gives a native Rust starting point for a controlled transport implementation. See the [gix transport option construction](https://docs.rs/gix/latest/src/gix/repository/config/transport.rs.html) and [`Remote::connect` documentation](https://docs.rs/gix/latest/gix/struct.Remote.html).

This is only a provisional preference. The inspected gix API documents a connection timeout and low-speed controls, but does not establish a hard five-minute total attempt deadline or a hard total network-ingress cap. A low-speed cutoff is not a wall-clock deadline. The adapter would still need a transport-level byte counter that aborts before exceeding the cap and an outer worker deadline that terminates blocked DNS, connect, or read operations. This needs an executable proof against the selected pinned gix/backend versions before it can support M4 estimates.

## libgit2 comparison

libgit2 provides connect and server read/write timeouts through `git_libgit2_opts`, and a maximum size for an individual downloaded pack object. Its fetch options expose callbacks and redirect policy. The Rust `git2::Progress::received_bytes()` reports bytes in the packfile so far, and transfer callbacks can stop an operation. These facilities are useful for progress and cancellation, but the reviewed APIs do not prove a strict wire-ingress ceiling before bytes are received. Its connection timeout settings are library-wide options, which also need concurrency/lifecycle analysis for an application-owned job budget. See [libgit2 options](https://libgit2.org/docs/reference/main/common/git_libgit2_opts.html), [fetch options](https://libgit2.org/docs/reference/main/remote/git_fetch_options.html), [`git2::Progress`](https://docs.rs/git2/latest/git2/struct.Progress.html), and [`RemoteCallbacks`](https://docs.rs/git2/latest/git2/struct.RemoteCallbacks.html).

## Required proof before either adapter passes M0

- Enforce the 2 GiB ingress limit in the transport path before writing additional bytes to the temporary store; progress callbacks alone are insufficient evidence.
- Enforce a five-minute outer deadline, including stalled DNS/connect/read cases; per-socket and low-speed timeouts are supplementary controls.
- Reject off-site redirects, prevent ambient credential-helper lookup, and confirm that only the app-mediated credential callback supplies credentials.
- Demonstrate cancellation overshoot, temporary-store cleanup, and correct fail-closed behavior with local adversarial tests and an explicitly consented private GitHub fixture.

The current Python probe demonstrates bounded reading of a single `info/refs` response only. It neither selects libgit2/gix for production nor proves full fetch/object ingress limits, credential storage behavior, or repository verification.
