# M0 combined ref observation outcome

Add a consuming backend observation API that combines bounded Git v2 ls-refs with explicit HTTP framing finalization on every observable protocol outcome. Preserve protocol error kind, HTTP framing/ingress/finalization evidence; expose advertised refs only when BOTH protocol and HTTP framing succeed. Empty valid ref lists remain distinguishable from incomplete observations. Poisoned metadata remains an error, not fabricated evidence.

This is ref advertisement observation, never full fetch/object integrity, remote retention, worker/store lifecycle, approval or cleanup authority. Existing low-level APIs remain documented as requiring both stages. No renderer/app exposure, generic execution, real GitHub/user files/credentials. Tests private loopbackTLS only; source review before execution.

Branch feat/m0-complete-ref-observation atop PR47 714578f; main final integrationbaseline afba4f6 through pendingstack. Bounded Luna owns refs.rs/lib.rs/README, root scope/progress/review. No duplicate agents/builds. Host8.1GiB free; focused tests/strictClippy/fmt/version0.0.17/diff/current-headCI required. Watched40–47 not polled.
