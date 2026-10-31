# Autonomous continuation handoff

Automation ID: continue-disk-steward-implementation
Kind: thread heartbeat, every 30 minutes, active when created.

User authorization: autonomously continue milestone by milestone until the agreed scope is done, a major strategic product decision requires Adrian, or applicable Codex remaining quota falls below 25%. Use GPT-6 Luna subagents heavily, with the lowest adequate reasoning effort. Stay on feature branches; versions remain 0.0.x. No automatic publishing, billing changes, or quota reset use.

Quota protocol: use get_usage_limits at run start, before new delegations and at bounded checkpoints. Remaining = 100 minus usedPercent in applicable windows; prefer rateLimitsByLimitId. If any applicable window is below 25%, or quota cannot be checked, checkpoint, stop delegates safely and pause automation ID above via automation_update. Do not auto-resume after reset without the user. At setup the weekly window reported 46% remaining; this is a historical observation, never a substitute for a fresh check.

Resume from /Users/adrian/Projects/disk-steward, current Git state, PROGRESS.md, ROADMAP.md and specifications. Private repo https://github.com/anschmieg/disk-steward; draft PR https://github.com/anschmieg/disk-steward/pull/1; branch feat/m0-foundation. M0 remains in progress. The shared contracts and shell compile independently, not yet wired together; synthetic fixtures have provisional model-authored labels. Live evaluation, native permission/recovery probes, bounded private Git verification, scope freeze, packaged/runtime tests and forecast remain open.

GitHub Actions did not run because of account billing/spending restrictions. Continue independent local work; do not alter billing or misreport CI as passing. If a specific M0 gate requires unavailable access, document it and work on unaffected gates rather than stopping at the first dependency. If no useful work remains, pause and explain the exact user action needed.

Before each milestone record the product-purpose check, scope cornerstones and failure cases. End each work batch with evidence, actual remaining work and branch/commit references. Do not run the autonomous skill's separate CLI runner in parallel with this app heartbeat; use its durable handoff pattern only to avoid duplicate writers and quota monitoring outside the app.
