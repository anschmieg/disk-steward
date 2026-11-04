# M0 advertised symbolic-reference consistency

Purpose: reject cyclic received symbolic-ref claims before producing advertised-pack closure evidence. A valid commit pack and equal OIDs on every edge cannot make a self-loop or longer symbolic-ref cycle valid. This closes an asymmetry with the caller-supplied required-ref manifest checks.

Retain exact same-advertisement target presence and OID equality. Bound traversal by the existing 1,024-reference cap using fixed state/path arrays, no recursion or heap graph. Each node is walked once; target-name lookup remains bounded by the advertisement count, with cooperative cancellation/deadline checks during lookup, traversal and completion. Valid chains and shared completed tails remain supported independently of advertisement order.

Work branch fix/m0-advertised-symref-cycles starts at reviewed PR82 d6deb70, with main as eventual integration baseline. No renderer, credentials, persistence, network policy, cleanup authority, version or release changes. This remains an inactive backend verifier; received claims are not an atomic remote snapshot, authentication, local preservation proof or milestone acceptance. Fresh current-head CI and no blocking findings required before main merge.
