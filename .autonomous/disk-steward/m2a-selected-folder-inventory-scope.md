# M2A initial selected-folder inventory

Main baseline e0e6c5a; feature branch feat/m2a-selected-folder-inventory. Native no-argument command selects a directory, holds a no-follow directory descriptor, enumerates immediate names and stats each without following links. Bound work to 512 entries and monotonic two-second checks between OS calls. No recursion, content reads, renderer path access, persistent path grants or cleanup authority. Non-Unix unsupported.

Show observed names/kinds/exact logical byte strings, hard-link warning, unreadable count and incomplete coverage. Sort largest first or by name. Do not call directory metadata a recursive size, logical byte sums occupied/reclaimable space, or shallow enumeration a full scan. Native OS calls are not interruptible deadline guarantees. Preserve prior observation on cancellation/error with explicit message. All tests use task-owned disposable fixtures. Full M2A acceptance remains open.
