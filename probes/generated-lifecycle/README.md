# Generated-record lifecycle probe

This development-only Rust probe creates a private temporary fixture containing
committed pytest 9.2.0 `nodeids` and `lastfailed` corpus bytes plus a valuable
local-notes sentinel. It accepts no caller paths or arbitrary filenames and has
no renderer command or live-project integration.

The intended validation sequence is exact review and approval, recoverable
stage, separate restore approval, stage again, and separately approved permanent
removal of the current receipt. Unselected records and notes must survive.
Reviews and attempts are bounded; creation requires a 2 GiB free-space reserve
plus 64 MiB headroom. Run `cargo test -p generated-lifecycle-probe` from the
workspace root.

The approval calls simulate a decision for disposable fixtures; they do not
provide native user confirmation. The ledger journals intent before mutation,
but this probe does not implement restart recovery or prove power-loss durability.
The no-replace rename primitive detects some name swaps after movement; it does
not eliminate a same-user name-to-operation race. Its private fixture boundary
is essential. A production adapter must resolve these limits before enabling
user-file mutations.

Passing tests support lifecycle feasibility for copied manager output. They do
not establish project ownership, idle pytest processes, safe regeneration,
provider behavior, controlled-volume recovered bytes, or M3/MVP completion.
The installed application's cleanup eligibility remains unchanged.
