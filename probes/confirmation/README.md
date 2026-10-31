# Native confirmation boundary probe

Build and run the dry-run with Apple's system Swift toolchain:

```sh
/usr/bin/swiftc -module-cache-path /private/tmp/disk-steward-swift-module-cache \
  -o /private/tmp/disk-steward-confirmation-probe probes/confirmation/ConfirmationProbe.swift
/private/tmp/disk-steward-confirmation-probe
```

Default mode prints one deterministic synthetic plan and SHA-256 digest; it
does not show a dialog or perform a mutation. The explicit `--run` mode presents
an AppKit warning dialog for that plan. Cancel is the first/default button;
the second button approves only the synthetic plan. Both responses return the
plan ID, revision, and digest, and the program has no filesystem mutation code.

Observed on macOS 27.0 / Apple Silicon / Swift 6.4: compilation and default
dry-run passed, with stable plan digest
`69f85730b44a1a49d5fb9def0c02424941fc8404c17c83507f7c5166957dcdb5`.
The dialog was not tested through the Cancel button. Running the bare command
line binary did not expose a dialog through the available UI automation, and
attempting to open a temporary `.app` wrapper failed with Launch Services
`kLSNoExecutableErr` (-10827). No dialog choice was made.

This does not exercise the current Tauri window. The existing Tauri backend
registers only a read-only foundation status command; it has no dialog plugin,
confirmation command, execution service, or cleanup operation. This probe
shows that a native AppKit dialog can present a digest-bearing synthetic
summary, not that a Tauri renderer can safely request or bind an approval.

For production, the trusted backend must derive the summary from an immutable
plan revision and bind the returned choice to the exact digest, target
manifest, adapter, preconditions, and expiry. It must revalidate eligibility
and identity immediately before an operation. A renderer could be compromised
or stale; displaying a digest alone neither authenticates the renderer nor
authorizes execution. No production approval, Tauri bridge, or cleanup path is
implemented here.
