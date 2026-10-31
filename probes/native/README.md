# macOS Foundation recovery probe

Development-only native probe for milestone M0. It has no arbitrary-path API. The optional `--probe-selective-purge` flag tests exact-receipt removal using two new synthetic items. Each run creates a unique directory
under `FileManager.temporaryDirectory` and only uses its own regular files and
symlink fixture. The native Trash call receives one self-created regular file.

Build and run on macOS with the system toolchain:

```sh
/usr/bin/swiftc -module-cache-path /private/tmp/disk-steward-swift-module-cache \
  -o /private/tmp/disk-steward-recovery-probe probes/native/RecoveryProbe.swift
/private/tmp/disk-steward-recovery-probe
```

Observed on macOS 27.0 / Apple Silicon with Swift 6.4: the helper compiled;
same-volume Foundation rename staging and restore preserved the URL volume and
resource identifiers; an occupied restore destination was rejected while both
synthetic files remained unchanged; a symlink was identified and rejected by
the regular-file identity function. `FileManager.trashItem` failed in this
environment with Cocoa error 513 / OSStatus -5000 (insufficient Trash access).
The probe reported native Trash unsupported and did not attempt a delete.

When Trash staging succeeds on another setup, the helper records Foundation's
`returnedURL`, checks its resource identity against the pre-stage volume and
resource identifier (plus available size/date metadata), verifies the original
destination is absent, then moves only that returned URL back. A collision or
identity mismatch leaves the returned item untouched. This is a narrow
Foundation path-based test; it cannot eliminate the documented race between
validation and `trashItem`/restore.

The optional purge experiment removes only its verified target receipt and verifies that a separate synthetic sentinel survives before restoring it. The helper never enumerates or empties Trash. The fixture root is
removed on exit; any item moved outside that root is not covered by that
cleanup. This probe does not establish app permissions, sandbox entitlements,
packaged-app behavior, crash/restart reconciliation, concurrent mutation
safety, nonlocal/other-volume behavior, production selective purge, or actual free-space
recovery. Do not enable a production cleanup action from this evidence alone.

Coordinator follow-up: the same probe passed native Trash stage/returnedURL restoration when run with the required execution access. The opt-in `--probe-selective-purge` experiment then passed on two new synthetic items: only the verified target receipt was removed; the other item survived and was restored. The earlier restricted failure remains useful access-boundary evidence. The helper now includes fresh metadata queries and SHA-256 for regular files, including a same-size-edit negative case. This does not validate production race resistance or permission handling. See [evidence](../M0-EVIDENCE.md).
