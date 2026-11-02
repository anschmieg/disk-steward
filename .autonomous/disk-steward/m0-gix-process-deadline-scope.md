# M0 gix subprocess deadline proof

Purpose: connect an actual pinned gix HTTP handshake to an outer process deadline. Test-only exact helper in the same test executable owns an IPv4 loopback listener, accepts and validates the fixed synthetic GET, signals readiness and withholds a response. The parent invokes only that exact helper, bounds its output and readiness/deadline waits, then kills, waits and joins its reader. Inner transport deadline must exceed the outer deadline, so inner socket timeouts cannot explain successful termination.

No new production APIs, renderer access, remote requests, credentials, paths, stores or cleanup. This is a process-boundary feasibility proof, not integration with the existing store worker, production DNS/TLS cancellation, OS kill latency guarantee or full M0 acceptance. Do not weaken the fixture's existing socket budget tests or claim whole-wire accounting.

Branch from PR33 c7a42eb; PR32/33 existing watches remain untouched. Root reviews concrete code before executing fixture tests. Require targeted behavioral tests, strict Clippy, formatting, version and diff checks, then reviewable stacked PR/current-head CI/main retarget before merge. Version remains 0.0.17.
