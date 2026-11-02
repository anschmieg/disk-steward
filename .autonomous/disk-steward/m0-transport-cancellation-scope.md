# M0 transport cancellation

Add a one-way attempt-scoped cancellation handle to the isolated GitHub candidate. Stop subsequent connections, reads, writes, cached-body consumption and lazy POST initialization when cancellation is observed. Preserve actual ingress accounting and fail framing evidence on interrupted reads. No reset, cleanup authority, renderer API, remote access, credentials or application activation.

Cancellation is cooperative. Existing socket timeout slices and sliced POST waits permit rechecks; DNS, connect and OS scheduling still require an outer worker deadline. No hard cancellation latency or process termination claim. Test with private loopback fixtures and deterministic in-memory seams only.

Branch feat/m0-transport-cancellation atop PR42 3fb9d35, main remains final integration baseline. Luna owns candidate lib.rs/README, root reviews concrete source before tests and owns integration/progress. No duplicate agents/overlapping builds. Focused tests, strict Clippy, fmt/version/diff and fresh current-head CI required. Host10GiB free; no cache cleanup. Existing PR38–42 watches not polled.
