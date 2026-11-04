# M0: exact worker readiness framing and interrupted-read bound

The closed worker stdout reader currently emits readiness for any first byte. Require the existing fixed `R` marker as the first byte before emitting readiness; malformed first output must report reader failure with its observed byte count and no readiness event. Empty EOF remains valid zero-byte completion for the separate partial-payload failure fixture. Subsequent output remains bounded to the existing 64 KiB plus one sentinel byte, so the flood probe still establishes its output-limit stop.

Bound interrupted reads cumulatively to sixteen per reader invocation. The seventeenth interruption reports failure; successful reads do not renew that allowance. Keep the fixed buffer and two-event channel, exact executable launch, environment clearing, existing supervisor stop/reap/join requirements, and output accounting. No generic protocol messages, arguments, paths, commands, environment switches, cleanup or renderer capability are added.

Regression cases must demonstrate malformed-first-byte refusal at the reader and readiness consumer, empty EOF, valid marker with split/coalesced data, cumulative interruption exhaustion across successful reads, and the exact output sentinel bound. Existing real-binary supervisor and store fixtures must continue to pass.

This addresses M0 task0.1 restricted worker protocols and the bounded-work part of R5.8. It does not authenticate an arbitrary peer, provide an OS sandbox/process-tree boundary, make blocking reads hard-interruptible, prove production Git worker integration/global storage accounting, grant cleanup or satisfy milestone acceptance. Main remains eventual integration baseline; branch builds on PR91 while its stack integrates. Version stays0.0.x/no release.
