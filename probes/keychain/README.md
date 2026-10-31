# macOS Keychain development probe

This development-only helper checks whether a synthetic generic-password item can be added, read back, deleted, and verified absent. The default invocation is a dry run and makes no Keychain calls.

Build and exercise only the dry-run path with:

```sh
/usr/bin/swiftc -module-cache-path /private/tmp/disk-steward-swift-module-cache \
  -o /private/tmp/disk-steward-keychain-probe probes/keychain/KeychainProbe.swift
/private/tmp/disk-steward-keychain-probe
```

The optional `--run` path is reserved for deliberate local development experiments. It creates a fresh UUID-backed service and account for a synthetic generic-password item, checks the value internally without printing it, deletes only that exact class/service/account tuple, then verifies the tuple is absent. Queries set Keychain interaction to fail rather than prompting, so a locked or unavailable Keychain fails closed. The helper does not enumerate items, read real credentials, accept roots or item identifiers as input, or use actual tokens.

This probe does not exercise Disk Steward's GitHub credential broker, OAuth/token acquisition, real Git operations, permission acceptance, packaged-app behavior, denied/revoked credentials, or any production authentication flow. A local synthetic item result cannot establish those M0/M4 gates. No live Keychain run is recorded by this document.
