# M0 explicit Rust worker cancellation

Purpose: distinguish parent-requested cancellation from deadline/budget failures in the fixed-child Rust lifecycle proof. Add one synthetic parent case that starts the existing stalled child, waits for readiness, then explicitly cancels and uses the same stop/reap/reader-join path. No new child mode, generic executable/path/shell or renderer access, files, network or credentials.

The cap still bounds observed stdout, not network ingress. Cancellation is a fixed probe decision after readiness, not proof of user UI/network cancellation, process-tree isolation or OS shutdown-latency guarantees. Preserve missing-readiness/error refusal. Version remains0.0.17.

Branch starts PR25head43b4a45; pending local integration progress is preserved and will commit here. Required targeted subprocess tests/fmt/strictClippy plus current-head CI before merge. Main is final target after25integration; do not poll existingwatch on recurringwake.
