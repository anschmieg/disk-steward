# M0 deterministic transport ingress accounting

Close the source-reviewed cancellation-race evidence gap with a private shared production Budget read helper. Production socket read delegates into the same cap/admission/actual-byte charge/post-read cancellation/deadline path exercised by deterministic readers. No public test connector or generic execution access.

Prove successful socket-returned bytes are charged even when cancellation arrives during the read, later reads do not run, cap slices cannot exceed remaining ingress, and shared clones retain accounting. Errors with no returned count do not fabricate usage. These are socket-returned byte observations, not NIC/OS traffic or hard cancellation latency. No Git receipt, cleanup, worker-stop or M0 completion claim.

Branch test/m0-transport-ingress-races atop PR43 33c9828; main final integration baseline. Bounded Luna owns lib.rs/README; root scope/progress and source review before tests. No overlapping builds/agents, private in-memory fixtures only. Focused tests/strict Clippy/fmt/version/diff/current-head CI. Host10GiB free. Existing38–43 watches not polled.
