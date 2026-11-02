# M0 observed ingress accounting

Purpose: keep resource-limit evidence conservative when a transport chunk arrives beyond its limit or after cancellation/failure but before the worker is stopped. The pure verification-budget controller currently discards the over-limit chunk and rejects all late ingress, while temporary-store observations already remain charged.

Change only accounting of caller-reported received bytes: saturate overflow, never revive failed/canceled jobs, preserve the first failure, reject observations after stopped acknowledgment/completion. The real pre-read transport ceiling remains separate and unchanged. No network, worker, credentials, file operations or renderer authority is introduced. No claim of enforced full-fetch budgets or completed M0. Current development version remains 0.0.17.

Based on PR23 head4218e8a, awaiting watched integration baseline; main remains final target. Test exact overshoot and late arrivals, preserved states and closed worker boundary, then run crate tests/fmt/strictClippy.
