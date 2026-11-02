# M0 store admission and observed-growth accounting

Purpose: add a nonmutating BudgetController check for proposed growth deltas against repository/aggregate totals and free-before reserve plus headroom and proposed bytes. It reserves no capacity, admits only Running jobs, and does not grant cleanup or worker-stop authority. Existing record_store remains authoritative for actual/late/overshoot observations.

Wire the fixed owned payload fixture to that check before creation/each chunk, then record actual file-length growth even after a partial write error. Post-write free-space sampling failure is conservatively blocked; uncertain size observations retain artifacts. A completed fixture retains its charged bytes and cleanup remains unconfirmed. Other store errors mark the fixture job StoreIncomplete; first post-observation failure reason must not be overwritten. No worker-stop acknowledgment is synthesized.

Controller lifetime remains local to this fixture; there is no durable/global aggregate quota or production scheduling claim. Unit tests may simulate 2 GiB/4 GiB totals without writing those sizes; real file tests remain <=32KiB task-owned fixtures. No caller paths, cleanup expansion, remote, credentials, renderer or milestone completion.

Branch atop PR38 df309ca; root owns payload integration, bounded Luna owns controller API/tests. Root concrete review before execution; required focused behavior, strictClippy/fmt/version0.0.17/diff/current-headCI. Existing36/37/38watches notpolled. Preserve local checkpoints; host15GiB available before work.
