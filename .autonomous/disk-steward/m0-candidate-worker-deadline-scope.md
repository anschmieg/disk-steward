# M0 candidate transport outer process deadline

Test-only integration: exact ignored child test runs the actual GitHub candidate against a private loopback TLS server. Server completes TLS and validates the fixed Git GET before readiness, then withholds the HTTP response past the parent outer deadline. Parent must terminate that exact child, wait/reap, and join bounded stdout capture before assertions on every path. No shell, arbitrary endpoint, environment credential or filesystem access. Review concrete subprocess code before execution.

Proves only this synthetic candidate read can be stopped by an outer process supervisor. No OS kill/reap latency, descendant-tree termination, DNS interruption, durable worker/store cleanup, private GitHub auth, Git receipt or milestone completion claim. Existing cooperative cancellation remains separate.

Branch test/m0-candidate-worker-deadline atop PR44 657bc1e; main final baseline. Bounded Luna owns candidate lib.rs/README, root scope/progress/review before tests. No duplicate agents/builds. Host10GiB free; focused tests/strictClippy/fmt/version/diff/current-head CI. Existing38–44 watches not polled.
