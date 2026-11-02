# M0 GitHub HTTPS candidate adapter

Purpose: extract a candidate backend from the validated local TLS design into a reusable backend-only crate with a strict GitHub endpoint boundary. Constructing an adapter performs no I/O. Only explicit gix HTTP requests may resolve/connect to github.com:443. No caller host/connector/trust override in the public API, no remote rewrite, credentials in URLs, ambient helpers, proxy/environment credentials, redirects or renderer commands.

Keep one attempt-wide raw-socket ingress cap (at most2GiB) beneath normal rustls certificate/hostname verification and a monotonic deadline (at most5min). Buffered POST bodies have a separate small hard cap and poison-on-error semantics. Bound/validate HTTP framing; unsupported compression/chunked responses fail closed. Private unit-test seam only may use numeric loopback and generated certificates; tests must make no remote/DNS requests.

This is a candidate, not an enabled application transport or M0/M4 acceptance. Synchronous system DNS still requires outer-worker supervision; no hard DNS/OS kill latency guarantee. No full-fetch/ref/object proof, repository mutation, stored-object budget, private GitHub/Keychain acceptance, credential memory-zeroization claim or release. Request-body/response-framing limits may reject legitimate remotes and must stay visible as incomplete verification.

Branch atop PR39 head75eb764; main final integration baseline. Bounded Luna owns new github-transport crate files, root owns workspace/lock/scope/progress and concrete source review before tests. Version0.0.17; format/strictClippy/build/behavior/version/diff/current-headCI required. Existing37/38/39watches notpolled; preserve local checkpoint. Host15GiB observed before work; no user-cache cleanup.
