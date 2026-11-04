# M0 required symbolic-ref identity

Extend the inactive caller-claim manifest with an explicit optional raw symbolic target. None claims a direct ref; Some must be a valid full refs/ name, at most 1024 bytes and not the entry itself. Branch targets require commit kind. Existing 64 KiB aggregate now includes targets before fallible copies. Canonical digest domain v2 encodes target presence, length and bytes; full entry comparison catches symbolic changes and detachment. No persisted manifests or migration are introduced.

Named coverage distinguishes DifferentSymrefTarget after existing missing-name/OID checks. The task-owned real-Git fixture captures for-each-ref symref fields plus explicit HEAD and proves branch switching and detachment at unchanged OID invalidate prior named evidence. Independent Python digest vectors, malformed targets, non-UTF8 bytes and aggregate boundaries are checked.

R5.4/ref preservation contribution only: caller claims and two native observations remain incomplete and non-atomic. No unborn HEAD/cycle/comprehensive inventory proof, preservation decision, persistent receipt, cleanup eligibility, renderer access, credentials or production worker/global-store integration. M0 remains in progress, version 0.0.17; no release. Feature branch atop PR77, eventual main integration requires each current head's required CI and no blocking findings.
