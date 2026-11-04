# M1B explicit pilot-report copy

The existing local aggregate-only anonymous pilot report can be copied through an explicit Copy report button only after recording finishes. Copy exactly the same pretty-printed JSON shown in the read-only textarea with navigator.clipboard.writeText; never read clipboard or invoke filesystem/network/native capabilities. No automatic copy/save/send. Human assessment remains not_recorded and executionAvailable false.

Pending state plus an immediate ref guard prevent repeat writes; disable copy while workspace busy and discard while a copy is pending. Success announced only after write resolves. Missing/rejected clipboard shows generic manual-select/copy guidance, leaves report intact and re-enables retry. No clipboard error details or report contents in diagnostics.

Independent main-based component change. 41 frontend tests, TypeScript/production build/version0.0.17, format/diff checks pass locally. Tests mock clipboard and prove explicit exact contents/no automatic copy/failure retention/pending/busy controls. They do not prove native clipboard permissions or platform behavior, and do not replace three participant/native accessibility acceptance. No product cleanup/IPC/schema/credential/release/milestone expansion. Active transport work remains82; independent83/84 pending.
